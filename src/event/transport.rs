use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::Message;
use tungstenite::{HandshakeError, client_tls_with_config};
use url::Url;

use super::{GatewayConfig, GatewayError, GatewaySocket, GatewaySocketFactory};

static RESOLVER_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

pub(super) struct TungsteniteFactory;

impl GatewaySocketFactory for TungsteniteFactory {
    fn connect(
        &self,
        config: &GatewayConfig,
        token: &str,
        timeout: Duration,
    ) -> Result<Box<dyn GatewaySocket>, GatewayError> {
        initialize_tls_provider()?;
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

struct ResolverPermit {
    in_flight: &'static AtomicBool,
}

impl ResolverPermit {
    fn acquire(in_flight: &'static AtomicBool) -> Result<Self, GatewayError> {
        in_flight
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| GatewayError::Retryable)?;
        Ok(Self { in_flight })
    }
}

impl Drop for ResolverPermit {
    fn drop(&mut self) {
        self.in_flight.store(false, Ordering::Release);
    }
}

fn resolve_addresses(url: Url, timeout: Duration) -> Result<Vec<SocketAddr>, GatewayError> {
    resolve_addresses_with(&RESOLVER_IN_FLIGHT, timeout, move || {
        url.socket_addrs(|| None)
            .map_err(|_| GatewayError::Retryable)
    })
}

fn resolve_addresses_with<F>(
    in_flight: &'static AtomicBool,
    timeout: Duration,
    resolve: F,
) -> Result<Vec<SocketAddr>, GatewayError>
where
    F: FnOnce() -> Result<Vec<SocketAddr>, GatewayError> + Send + 'static,
{
    let permit = ResolverPermit::acquire(in_flight)?;
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("babysit-dns-resolver".to_string())
        .spawn(move || {
            let result = resolve();
            drop(permit);
            let _ = sender.send(result);
        })
        .map_err(|_| GatewayError::Retryable)?;
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

// Rustls requires a process-wide provider before any TLS client builder is used.
fn initialize_tls_provider() -> Result<(), GatewayError> {
    install_tls_provider_once(
        || rustls::crypto::CryptoProvider::get_default().is_some(),
        || rustls::crypto::ring::default_provider().install_default(),
    )
}

fn install_tls_provider_once<Installed, Install, Error>(
    mut is_installed: Installed,
    install: Install,
) -> Result<(), GatewayError>
where
    Installed: FnMut() -> bool,
    Install: FnOnce() -> Result<(), Error>,
{
    if is_installed() {
        return Ok(());
    }
    match install() {
        Ok(()) => Ok(()),
        Err(_) if is_installed() => Ok(()),
        Err(_) => Err(GatewayError::Fatal(
            "gateway TLS provider failed to initialize",
        )),
    }
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
    use std::sync::Arc;

    use super::*;

    #[test]
    fn installs_process_level_tls_provider() {
        initialize_tls_provider().unwrap();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    #[test]
    fn accepts_provider_installed_by_racing_caller() {
        let installed = Cell::new(false);
        assert!(
            install_tls_provider_once(
                || installed.get(),
                || {
                    installed.set(true);
                    Err(())
                },
            )
            .is_ok()
        );
    }

    #[test]
    fn limits_resolvers_left_running_after_timeout() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

        let (release_sender, release_receiver) = mpsc::channel();
        let first = resolve_addresses_with(&IN_FLIGHT, Duration::from_millis(20), move || {
            release_receiver.recv().unwrap();
            Ok(vec!["127.0.0.1:443".parse().unwrap()])
        });
        let second_called = Arc::new(AtomicBool::new(false));
        let called = Arc::clone(&second_called);
        let second = resolve_addresses_with(&IN_FLIGHT, Duration::from_secs(1), move || {
            called.store(true, Ordering::Release);
            Ok(vec!["127.0.0.2:443".parse().unwrap()])
        });
        release_sender.send(()).unwrap();
        let release_deadline = Instant::now() + Duration::from_secs(1);
        while IN_FLIGHT.load(Ordering::Acquire) {
            assert!(Instant::now() < release_deadline, "resolver permit leaked");
            thread::yield_now();
        }
        let third = resolve_addresses_with(&IN_FLIGHT, Duration::from_secs(1), || {
            Ok(vec!["127.0.0.3:443".parse().unwrap()])
        });

        assert_eq!(first, Err(GatewayError::Retryable));
        assert_eq!(second, Err(GatewayError::Retryable));
        assert!(!second_called.load(Ordering::Acquire));
        assert_eq!(third, Ok(vec!["127.0.0.3:443".parse().unwrap()]));
    }

    #[test]
    fn resolver_failures_remain_retryable() {
        static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

        let result = resolve_addresses_with(&IN_FLIGHT, Duration::from_secs(1), || {
            Err(GatewayError::Retryable)
        });

        assert_eq!(result, Err(GatewayError::Retryable));
    }

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
