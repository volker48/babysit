mod transport;

use std::thread;
use std::time::{Duration, Instant};

use rand::Rng;
use url::Url;

use crate::core::PrSnapshot;
use crate::credentials::{TokenStore, production_store};
use crate::error::CliError;
use crate::wait::{SnapshotAction, WakeSource};

use transport::TungsteniteFactory;
pub use transport::{classify_gateway_status, classify_transport_kind};

const PROTOCOL_VERSION: u8 = 1;
const REGISTRATION_ATTEMPT_CAP: Duration = Duration::from_secs(30);

/// Supplies time, bounded waiting, and jitter for event reconnection.
pub trait EventRuntime {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
    fn jitter(&self, maximum: Duration) -> Duration;
}

struct SystemRuntime;

impl EventRuntime for SystemRuntime {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }

    fn jitter(&self, maximum: Duration) -> Duration {
        jittered_delay(maximum)
    }
}

/// Validated, non-secret endpoint configuration for the event gateway.
#[derive(Clone)]
pub struct GatewayConfig {
    url: Url,
}

impl GatewayConfig {
    pub fn parse(value: &str) -> Result<Self, CliError> {
        let url = Url::parse(value)
            .map_err(|_| CliError::new("--gateway-url must be wss://host/watch", false))?;
        if url.scheme() != "wss"
            || url.host_str().is_none()
            || url.path() != "/watch"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(CliError::new(
                "--gateway-url must be wss://host/watch",
                false,
            ));
        }
        Ok(Self { url })
    }

    fn for_watch(&self, watch: &WatchRegistration) -> Result<Self, CliError> {
        let (owner, repository) = watch
            .repository
            .split_once('/')
            .ok_or_else(protocol_error)?;
        let mut url = self.url.clone();
        let mut segments = url.path_segments_mut().map_err(|_| protocol_error())?;
        segments.clear();
        segments.push("watch");
        segments.push(owner);
        segments.push(repository);
        drop(segments);
        Ok(Self { url })
    }
}

#[derive(Clone, PartialEq, Eq)]
struct WatchRegistration {
    repository: String,
    number: u64,
    head_oid: String,
}

impl WatchRegistration {
    fn from_snapshot(snapshot: &PrSnapshot) -> Self {
        Self {
            repository: format!("{}/{}", snapshot.owner, snapshot.repo),
            number: snapshot.number,
            head_oid: snapshot.head_oid.clone(),
        }
    }
}

/// A WebSocket adapter boundary; event data is only used to request a new snapshot.
pub trait GatewaySocket {
    fn send_text(&mut self, value: String, timeout: Duration) -> Result<(), GatewayError>;
    fn read_text(&mut self, timeout: Duration) -> Result<Option<String>, GatewayError>;
}

/// Connects sockets with the gateway bearer passed only to the opening handshake.
pub trait GatewaySocketFactory {
    fn connect(
        &self,
        config: &GatewayConfig,
        token: &str,
        timeout: Duration,
    ) -> Result<Box<dyn GatewaySocket>, GatewayError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayError {
    Fatal(&'static str),
    Retryable,
}

impl GatewayError {
    fn cli_error(self) -> CliError {
        match self {
            Self::Fatal(message) => CliError::new(message, false),
            Self::Retryable => CliError::new("gateway connection failed", true),
        }
    }
}

/// Event-assisted wake source that preserves snapshots as the sole authority.
pub struct EventWakeSource {
    config: GatewayConfig,
    store: Box<dyn TokenStore>,
    factory: Box<dyn GatewaySocketFactory>,
    socket: Option<Box<dyn GatewaySocket>>,
    watch: Option<WatchRegistration>,
    ready_cursor: Option<u64>,
    last_seen: Option<u64>,
    retry_delay: Duration,
    runtime: Box<dyn EventRuntime>,
}

impl EventWakeSource {
    pub fn new(gateway_url: &str) -> Result<Self, CliError> {
        Self::with_dependencies(
            GatewayConfig::parse(gateway_url)?,
            production_store(),
            Box::new(TungsteniteFactory),
        )
    }

    pub fn with_dependencies(
        config: GatewayConfig,
        store: Box<dyn TokenStore>,
        factory: Box<dyn GatewaySocketFactory>,
    ) -> Result<Self, CliError> {
        Self::with_runtime(config, store, factory, Box::new(SystemRuntime))
    }

    pub fn with_runtime(
        config: GatewayConfig,
        store: Box<dyn TokenStore>,
        factory: Box<dyn GatewaySocketFactory>,
        runtime: Box<dyn EventRuntime>,
    ) -> Result<Self, CliError> {
        Ok(Self {
            config,
            store,
            factory,
            socket: None,
            watch: None,
            ready_cursor: None,
            last_seen: None,
            retry_delay: Duration::from_secs(1),
            runtime,
        })
    }

    fn register(&mut self, watch: &WatchRegistration, remaining: Duration) -> Result<(), CliError> {
        let deadline = self
            .runtime
            .now()
            .checked_add(remaining.min(REGISTRATION_ATTEMPT_CAP))
            .ok_or_else(deadline_error)?;
        let token = self.store.load()?.ok_or_else(missing_token)?;
        let socket_config = self.config.for_watch(watch)?;
        let timeout = self.remaining(deadline)?;
        let mut socket = self
            .factory
            .connect(&socket_config, token.expose(), timeout)
            .map_err(GatewayError::cli_error)?;
        let timeout = self.remaining(deadline)?;
        socket
            .send_text(register_frame(watch, self.last_seen)?, timeout)
            .map_err(GatewayError::cli_error)?;
        let timeout = self.remaining(deadline)?;
        let ready = socket.read_text(timeout).map_err(GatewayError::cli_error)?;
        let cursor = ready
            .as_deref()
            .ok_or_else(ready_timeout)
            .and_then(parse_ready)?;
        self.socket = Some(socket);
        self.ready_cursor = Some(cursor);
        self.last_seen = Some(cursor);
        self.retry_delay = Duration::from_secs(1);
        Ok(())
    }

    fn remaining(&self, deadline: Instant) -> Result<Duration, CliError> {
        let remaining = deadline.saturating_duration_since(self.runtime.now());
        if remaining.is_zero() {
            return Err(deadline_error());
        }
        Ok(remaining)
    }

    fn connect_and_register(&mut self, remaining: Duration) -> Result<(), CliError> {
        let watch = self
            .watch
            .clone()
            .expect("watch is set before registration");
        match self.register(&watch, remaining) {
            Ok(()) => Ok(()),
            Err(error) if error.retryable => {
                self.socket = None;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn retry_connection(&mut self, deadline: Instant) -> Result<Option<bool>, CliError> {
        let remaining = deadline.saturating_duration_since(self.runtime.now());
        if remaining.is_zero() {
            return Ok(None);
        }
        self.connect_and_register(remaining)?;
        Ok(Some(self.socket.is_some()))
    }

    fn sleep_before_retry(&mut self, deadline: Instant) -> bool {
        let remaining = deadline.saturating_duration_since(self.runtime.now());
        if remaining.is_zero() {
            return false;
        }
        self.runtime
            .sleep(remaining.min(self.runtime.jitter(self.retry_delay)));
        self.retry_delay = next_retry_delay(self.retry_delay);
        true
    }

    fn sleep_until(&self, deadline: Instant) {
        let remaining = deadline.saturating_duration_since(self.runtime.now());
        if !remaining.is_zero() {
            self.runtime.sleep(remaining);
        }
    }

    fn reconnect_during_wait(&mut self, deadline: Instant) -> Result<bool, CliError> {
        if !self.sleep_before_retry(deadline) {
            return Ok(true);
        }
        Ok(self.retry_connection(deadline)?.unwrap_or(true))
    }

    fn wait_for_socket(&mut self, deadline: Instant) -> Result<bool, CliError> {
        let remaining = deadline.saturating_duration_since(self.runtime.now());
        if remaining.is_zero() {
            return Ok(true);
        }
        let result = self
            .socket
            .as_mut()
            .expect("socket was checked")
            .read_text(remaining);
        match result {
            Ok(Some(message)) => self.handle_message(&message),
            Ok(None) => {
                self.socket = None;
                self.sleep_until(deadline);
                Ok(true)
            }
            Err(GatewayError::Fatal(message)) => Err(CliError::new(message, false)),
            Err(GatewayError::Retryable) => {
                self.socket = None;
                Ok(false)
            }
        }
    }

    fn handle_message(&mut self, message: &str) -> Result<bool, CliError> {
        let (kind, cursor) = parse_notification(message)?;
        let Some(ready_cursor) = self.ready_cursor else {
            return Err(protocol_error());
        };
        self.last_seen = Some(self.last_seen.unwrap_or(ready_cursor).max(cursor));
        Ok(kind == "resync"
            || (cursor > ready_cursor && matches!(kind.as_str(), "wake" | "replay")))
    }
}

impl WakeSource for EventWakeSource {
    fn now(&self) -> Instant {
        self.runtime.now()
    }

    fn wait(&mut self, duration: Duration) -> Result<(), CliError> {
        let deadline = self
            .runtime
            .now()
            .checked_add(duration)
            .ok_or_else(deadline_error)?;
        if self.watch.is_none() {
            self.runtime.sleep(duration);
            return Ok(());
        }
        loop {
            if self.runtime.now() >= deadline {
                return Ok(());
            }
            let woke = if self.socket.is_none() {
                self.reconnect_during_wait(deadline)?
            } else {
                self.wait_for_socket(deadline)?
            };
            if woke {
                return Ok(());
            }
        }
    }

    fn observe_snapshot(
        &mut self,
        snapshot: &PrSnapshot,
        remaining: Duration,
    ) -> Result<SnapshotAction, CliError> {
        let watch = WatchRegistration::from_snapshot(snapshot);
        if self.socket.is_none() || self.watch.as_ref() != Some(&watch) {
            self.watch = Some(watch);
            self.ready_cursor = None;
            self.connect_and_register(remaining)?;
            return Ok(if self.socket.is_some() {
                SnapshotAction::RefetchNow
            } else {
                SnapshotAction::Wait
            });
        }
        Ok(SnapshotAction::Wait)
    }
}

fn missing_token() -> CliError {
    CliError::new(
        "gateway token is not configured in the macOS Keychain",
        false,
    )
}

fn protocol_error() -> CliError {
    CliError::new("gateway protocol error", false)
}

fn ready_timeout() -> CliError {
    CliError::new("gateway ready timed out", true)
}

fn deadline_error() -> CliError {
    CliError::new("gateway operation timed out", true)
}

fn next_retry_delay(current: Duration) -> Duration {
    current.saturating_mul(2).min(Duration::from_secs(30))
}

fn jittered_delay(maximum: Duration) -> Duration {
    let milliseconds = maximum.as_millis().try_into().unwrap_or(u64::MAX);
    Duration::from_millis(rand::rng().random_range(1..=milliseconds.max(1)))
}

fn register_frame(watch: &WatchRegistration, after: Option<u64>) -> Result<String, CliError> {
    serde_json::to_string(&serde_json::json!({
        "type": "register",
        "version": PROTOCOL_VERSION,
        "watch": {
            "forge": "github",
            "host": "github.com",
            "repository": watch.repository,
            "number": watch.number,
            "headOid": watch.head_oid,
        },
        "after": after,
    }))
    .map_err(|_| protocol_error())
}

fn parse_ready(message: &str) -> Result<u64, CliError> {
    let (kind, cursor) = parse_frame(message)?;
    if kind != "ready" {
        return Err(protocol_error());
    }
    Ok(cursor)
}

fn parse_notification(message: &str) -> Result<(String, u64), CliError> {
    let (kind, cursor) = parse_frame(message)?;
    if matches!(kind.as_str(), "wake" | "replay" | "resync") {
        Ok((kind, cursor))
    } else {
        Err(protocol_error())
    }
}

fn parse_frame(message: &str) -> Result<(String, u64), CliError> {
    let frame: serde_json::Value = serde_json::from_str(message).map_err(|_| protocol_error())?;
    let version = frame.get("version").and_then(serde_json::Value::as_u64);
    let kind = frame.get("type").and_then(serde_json::Value::as_str);
    let cursor = frame.get("cursor").and_then(serde_json::Value::as_u64);
    match (version, kind, cursor) {
        (Some(version), Some(kind), Some(cursor)) if version == u64::from(PROTOCOL_VERSION) => {
            Ok((kind.to_string(), cursor))
        }
        _ => Err(protocol_error()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_repository_socket_path_with_percent_encoded_segments() {
        let config = GatewayConfig::parse("wss://gateway.example/watch").unwrap();
        let watch = WatchRegistration {
            repository: "owner name/repo?#".to_string(),
            number: 1,
            head_oid: "head".to_string(),
        };

        let socket = config.for_watch(&watch).unwrap();

        assert_eq!(
            socket.url.as_str(),
            "wss://gateway.example/watch/owner%20name/repo%3F%23"
        );
    }
}
