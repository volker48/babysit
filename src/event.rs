use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rand::Rng;
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::Message;
use tungstenite::{HandshakeError, client_tls_with_config};
use url::Url;

use crate::core::PrSnapshot;
use crate::credentials::{TokenStore, production_store};
use crate::forge::CliError;
use crate::wait::{SnapshotAction, WakeSource};

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
            .map_err(|_| CliError::new("--gateway-url must be a valid wss URL", false))?;
        if url.scheme() != "wss"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(CliError::new(
                "--gateway-url must be a plain wss URL",
                false,
            ));
        }
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
    pending_refetch: bool,
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
            pending_refetch: false,
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
        let timeout = self.remaining(deadline)?;
        let mut socket = self
            .factory
            .connect(&self.config, token.expose(), timeout)
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
            Ok(Some(message)) => {
                self.handle_message(&message)?;
                Ok(self.pending_refetch)
            }
            Ok(None) => {
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

    fn handle_message(&mut self, message: &str) -> Result<(), CliError> {
        let (kind, cursor) = parse_notification(message)?;
        let Some(ready_cursor) = self.ready_cursor else {
            return Err(protocol_error());
        };
        self.last_seen = Some(self.last_seen.unwrap_or(ready_cursor).max(cursor));
        if cursor > ready_cursor && matches!(kind.as_str(), "wake" | "replay" | "resync") {
            self.pending_refetch = true;
        }
        Ok(())
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
                self.pending_refetch = false;
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
        if self.pending_refetch {
            self.pending_refetch = false;
            return Ok(SnapshotAction::RefetchNow);
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

struct TungsteniteFactory;

impl GatewaySocketFactory for TungsteniteFactory {
    fn connect(
        &self,
        config: &GatewayConfig,
        token: &str,
        timeout: Duration,
    ) -> Result<Box<dyn GatewaySocket>, GatewayError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(GatewayError::Retryable)?;
        let request = gateway_request(config, token)?;
        let addresses = resolve_addresses(config.url.clone(), remaining_timeout(deadline)?)?;
        let stream =
            connect_resolved_addresses(addresses, deadline, Instant::now, |address, timeout| {
                TcpStream::connect_timeout(&address, timeout).map_err(|_| GatewayError::Retryable)
            })?;
        stream
            .set_read_timeout(Some(remaining_timeout(deadline)?))
            .map_err(|_| GatewayError::Retryable)?;
        stream
            .set_write_timeout(Some(remaining_timeout(deadline)?))
            .map_err(|_| GatewayError::Retryable)?;
        let handshake_timeout = remaining_timeout(deadline)?;
        stream
            .set_read_timeout(Some(handshake_timeout))
            .map_err(|_| GatewayError::Retryable)?;
        stream
            .set_write_timeout(Some(handshake_timeout))
            .map_err(|_| GatewayError::Retryable)?;
        client_tls_with_config(request, stream, None, None)
            .map(|(socket, _)| Box::new(TungsteniteSocket(socket)) as Box<dyn GatewaySocket>)
            .map_err(classify_handshake_error)
    }
}

struct TungsteniteSocket(tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>);

impl GatewaySocket for TungsteniteSocket {
    fn send_text(&mut self, value: String, timeout: Duration) -> Result<(), GatewayError> {
        set_write_timeout(self.0.get_mut(), timeout)?;
        self.0
            .send(Message::Text(value.into()))
            .map_err(classify_tungstenite_error)
    }

    fn read_text(&mut self, timeout: Duration) -> Result<Option<String>, GatewayError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(GatewayError::Retryable)?;
        read_text_until(
            &mut self.0,
            deadline,
            Instant::now,
            |socket, timeout| {
                set_socket_timeout(socket.get_mut(), timeout)?;
                match socket.read() {
                    Ok(message) => Ok(Some(message)),
                    Err(tungstenite::Error::Io(error)) if is_read_timeout(&error) => Ok(None),
                    Err(error) => Err(classify_tungstenite_error(error)),
                }
            },
            |socket, timeout| {
                set_write_timeout(socket.get_mut(), timeout)?;
                socket.flush().map_err(classify_tungstenite_error)
            },
        )
    }
}

fn read_text_until<S, N, R, F>(
    mut socket: S,
    deadline: Instant,
    mut now: N,
    mut read: R,
    mut flush: F,
) -> Result<Option<String>, GatewayError>
where
    N: FnMut() -> Instant,
    R: FnMut(&mut S, Duration) -> Result<Option<Message>, GatewayError>,
    F: FnMut(&mut S, Duration) -> Result<(), GatewayError>,
{
    loop {
        let timeout = remaining_timeout_at(deadline, now())?;
        match read(&mut socket, timeout)? {
            Some(Message::Text(value)) => return Ok(Some(value.to_string())),
            Some(Message::Ping(_)) => flush(&mut socket, remaining_timeout_at(deadline, now())?)?,
            Some(Message::Pong(_)) => {}
            Some(Message::Close(_)) => return Err(GatewayError::Retryable),
            Some(_) => return Err(GatewayError::Fatal("gateway protocol error")),
            None => return Ok(None),
        }
    }
}

fn remaining_timeout(deadline: Instant) -> Result<Duration, GatewayError> {
    remaining_timeout_at(deadline, Instant::now())
}

fn remaining_timeout_at(deadline: Instant, now: Instant) -> Result<Duration, GatewayError> {
    let remaining = deadline.saturating_duration_since(now);
    if remaining.is_zero() {
        return Err(GatewayError::Retryable);
    }
    Ok(remaining)
}

fn resolve_addresses(url: Url, timeout: Duration) -> Result<Vec<SocketAddr>, GatewayError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = url
            .socket_addrs(|| None)
            .map_err(|_| GatewayError::Retryable);
        let _ = sender.send(result);
    });
    receiver
        .recv_timeout(timeout)
        .map_err(|_| GatewayError::Retryable)?
        .and_then(|addresses| {
            if addresses.is_empty() {
                Err(GatewayError::Retryable)
            } else {
                Ok(addresses)
            }
        })
}

fn connect_resolved_addresses<T, N, C>(
    addresses: impl IntoIterator<Item = SocketAddr>,
    deadline: Instant,
    mut now: N,
    mut connect: C,
) -> Result<T, GatewayError>
where
    N: FnMut() -> Instant,
    C: FnMut(SocketAddr, Duration) -> Result<T, GatewayError>,
{
    for address in addresses {
        let timeout = remaining_timeout_at(deadline, now())?;
        match connect(address, timeout) {
            Ok(stream) => return Ok(stream),
            Err(GatewayError::Retryable) => {}
            Err(error) => return Err(error),
        }
    }
    Err(GatewayError::Retryable)
}

fn gateway_request(
    config: &GatewayConfig,
    token: &str,
) -> Result<tungstenite::handshake::client::Request, GatewayError> {
    let mut request = config
        .url
        .clone()
        .into_client_request()
        .map_err(|_| GatewayError::Fatal("gateway request failed"))?;
    let header = format!("Bearer {token}")
        .parse()
        .map_err(|_| GatewayError::Fatal("gateway authorization failed"))?;
    request.headers_mut().insert("Authorization", header);
    Ok(request)
}

fn set_socket_timeout(
    stream: &mut tungstenite::stream::MaybeTlsStream<TcpStream>,
    timeout: Duration,
) -> Result<(), GatewayError> {
    let result = match stream {
        tungstenite::stream::MaybeTlsStream::Plain(stream) => {
            stream.set_read_timeout(Some(timeout))
        }
        tungstenite::stream::MaybeTlsStream::Rustls(stream) => {
            stream.sock.set_read_timeout(Some(timeout))
        }
        _ => return Err(GatewayError::Fatal("gateway transport is unsupported")),
    };
    result.map_err(|_| GatewayError::Retryable)
}

fn set_write_timeout(
    stream: &mut tungstenite::stream::MaybeTlsStream<TcpStream>,
    timeout: Duration,
) -> Result<(), GatewayError> {
    let result = match stream {
        tungstenite::stream::MaybeTlsStream::Plain(stream) => {
            stream.set_write_timeout(Some(timeout))
        }
        tungstenite::stream::MaybeTlsStream::Rustls(stream) => {
            stream.sock.set_write_timeout(Some(timeout))
        }
        _ => return Err(GatewayError::Fatal("gateway transport is unsupported")),
    };
    result.map_err(|_| GatewayError::Retryable)
}

fn is_read_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

fn classify_handshake_error(
    error: HandshakeError<
        tungstenite::handshake::client::ClientHandshake<
            tungstenite::stream::MaybeTlsStream<TcpStream>,
        >,
    >,
) -> GatewayError {
    match error {
        HandshakeError::Failure(error) => classify_tungstenite_error(error),
        HandshakeError::Interrupted(_) => GatewayError::Retryable,
    }
}

/// Classifies an HTTP opening-handshake status without exposing response contents.
pub fn classify_gateway_status(status: u16) -> GatewayError {
    match status {
        401 | 403 => GatewayError::Fatal("gateway authorization failed"),
        429 | 500..=599 => GatewayError::Retryable,
        _ => GatewayError::Fatal("gateway handshake failed"),
    }
}

/// Classifies a transport failure without carrying its potentially sensitive details.
pub fn classify_transport_kind(_kind: std::io::ErrorKind) -> GatewayError {
    GatewayError::Retryable
}

fn classify_tungstenite_error(error: tungstenite::Error) -> GatewayError {
    match error {
        tungstenite::Error::Http(response) => classify_gateway_status(response.status().as_u16()),
        tungstenite::Error::Io(error) => classify_transport_kind(error.kind()),
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
            GatewayError::Retryable
        }
        _ => GatewayError::Fatal("gateway protocol error"),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    use super::*;

    #[test]
    fn reads_text_after_control_frames_and_flushes_automatic_pong() {
        let start = Instant::now();
        let now = Cell::new(start);
        let frames = RefCell::new(VecDeque::from([
            Message::Ping(Vec::new().into()),
            Message::Pong(Vec::new().into()),
            Message::Text("message".into()),
        ]));
        let read_timeouts = RefCell::new(Vec::new());
        let flush_timeouts = RefCell::new(Vec::new());

        let result = read_text_until(
            (),
            start + Duration::from_secs(5),
            || now.get(),
            |_, timeout| {
                read_timeouts.borrow_mut().push(timeout);
                now.set(now.get() + Duration::from_secs(1));
                Ok(Some(frames.borrow_mut().pop_front().unwrap()))
            },
            |_, timeout| {
                flush_timeouts.borrow_mut().push(timeout);
                Ok(())
            },
        );

        assert_eq!(result, Ok(Some("message".to_string())));
        assert_eq!(
            *read_timeouts.borrow(),
            [
                Duration::from_secs(5),
                Duration::from_secs(4),
                Duration::from_secs(3)
            ]
        );
        assert_eq!(*flush_timeouts.borrow(), [Duration::from_secs(4)]);
    }

    #[test]
    fn connects_to_a_later_address_when_earlier_is_retryable() {
        let first = "127.0.0.1:443".parse().unwrap();
        let second = "127.0.0.2:443".parse().unwrap();
        let start = Instant::now();
        let now = Cell::new(start);
        let calls = RefCell::new(Vec::new());
        let timeouts = RefCell::new(Vec::new());

        let result = connect_resolved_addresses(
            [first, second],
            start + Duration::from_secs(10),
            || now.get(),
            |address, timeout| {
                calls.borrow_mut().push(address);
                timeouts.borrow_mut().push(timeout);
                if address == first {
                    now.set(start + Duration::from_secs(2));
                    Err(GatewayError::Retryable)
                } else {
                    Ok("connected")
                }
            },
        );

        assert_eq!(result, Ok("connected"));
        assert_eq!(*calls.borrow(), [first, second]);
        assert_eq!(
            *timeouts.borrow(),
            [Duration::from_secs(10), Duration::from_secs(8)]
        );
    }

    #[test]
    fn returns_retryable_after_all_addresses_fail() {
        let first = "127.0.0.1:443".parse().unwrap();
        let second = "127.0.0.2:443".parse().unwrap();
        let start = Instant::now();
        let calls = RefCell::new(Vec::new());

        let result: Result<(), GatewayError> = connect_resolved_addresses(
            [first, second],
            start + Duration::from_secs(10),
            || start,
            |address, _| {
                calls.borrow_mut().push(address);
                Err(GatewayError::Retryable)
            },
        );

        assert_eq!(result, Err(GatewayError::Retryable));
        assert_eq!(*calls.borrow(), [first, second]);
    }

    #[test]
    fn deadline_exhaustion_stops_address_attempts() {
        let first = "127.0.0.1:443".parse().unwrap();
        let second = "127.0.0.2:443".parse().unwrap();
        let start = Instant::now();
        let deadline = start + Duration::from_secs(10);
        let now = Cell::new(start);
        let calls = RefCell::new(Vec::new());

        let result: Result<(), GatewayError> = connect_resolved_addresses(
            [first, second],
            deadline,
            || now.get(),
            |address, _| {
                calls.borrow_mut().push(address);
                now.set(deadline);
                Err(GatewayError::Retryable)
            },
        );

        assert_eq!(result, Err(GatewayError::Retryable));
        assert_eq!(*calls.borrow(), [first]);
    }

    #[test]
    fn keeps_close_retryable_and_binary_frames_fatal() {
        let deadline = Instant::now() + Duration::from_secs(1);
        let close = read_text_until(
            (),
            deadline,
            Instant::now,
            |_, _| Ok(Some(Message::Close(None))),
            |_, _| unreachable!(),
        );
        let binary = read_text_until(
            (),
            deadline,
            Instant::now,
            |_, _| Ok(Some(Message::Binary(Vec::new().into()))),
            |_, _| unreachable!(),
        );

        assert_eq!(close, Err(GatewayError::Retryable));
        assert_eq!(binary, Err(GatewayError::Fatal("gateway protocol error")));
    }
}
