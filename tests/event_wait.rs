use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use babysit::core::{PrSnapshot, SettleOptions};
use babysit::credentials::{SecretToken, TokenStore};
use babysit::event::{
    EventRuntime, EventWakeSource, GatewayConfig, GatewayError, GatewaySocket,
    GatewaySocketFactory, classify_gateway_status, classify_transport_kind,
};
use babysit::forge::CliError;
use babysit::wait::{WaitOutcome, wait_until_settled};

#[derive(Clone)]
struct MemoryStore(SecretToken);

impl TokenStore for MemoryStore {
    fn load(&self) -> Result<Option<SecretToken>, CliError> {
        Ok(Some(self.0.clone()))
    }

    fn save(&self, _token: &SecretToken) -> Result<(), CliError> {
        Ok(())
    }

    fn delete(&self) -> Result<(), CliError> {
        Ok(())
    }
}

struct ScriptedSocket {
    received: Rc<RefCell<VecDeque<String>>>,
    sent: Rc<RefCell<Vec<String>>>,
}

impl GatewaySocket for ScriptedSocket {
    fn send_text(&mut self, value: String, _timeout: Duration) -> Result<(), GatewayError> {
        self.sent.borrow_mut().push(value);
        Ok(())
    }

    fn read_text(&mut self, _timeout: Duration) -> Result<Option<String>, GatewayError> {
        Ok(self.received.borrow_mut().pop_front())
    }
}

struct ScriptedFactory {
    received: Rc<RefCell<VecDeque<String>>>,
    sent: Rc<RefCell<Vec<String>>>,
}

struct FailingFactory;
struct RetryableFactory;

struct RecordingFactory(Rc<RefCell<Vec<Duration>>>);

struct TimedFactory {
    runtime: Rc<FakeRuntime>,
    timeouts: Rc<RefCell<Vec<Duration>>>,
}

struct TimedSocket {
    runtime: Rc<FakeRuntime>,
    timeouts: Rc<RefCell<Vec<Duration>>>,
}

impl GatewaySocket for TimedSocket {
    fn send_text(&mut self, _value: String, timeout: Duration) -> Result<(), GatewayError> {
        self.timeouts.borrow_mut().push(timeout);
        self.runtime.advance(Duration::from_secs(2));
        Ok(())
    }

    fn read_text(&mut self, timeout: Duration) -> Result<Option<String>, GatewayError> {
        self.timeouts.borrow_mut().push(timeout);
        self.runtime.advance(Duration::from_secs(1));
        Ok(Some(
            r#"{"type":"ready","version":1,"cursor":1}"#.to_string(),
        ))
    }
}

impl GatewaySocketFactory for TimedFactory {
    fn connect(
        &self,
        _config: &GatewayConfig,
        _token: &str,
        timeout: Duration,
    ) -> Result<Box<dyn GatewaySocket>, GatewayError> {
        self.timeouts.borrow_mut().push(timeout);
        self.runtime.advance(Duration::from_secs(2));
        Ok(Box::new(TimedSocket {
            runtime: self.runtime.clone(),
            timeouts: self.timeouts.clone(),
        }))
    }
}

struct RetryThenReadyFactory {
    attempts: RefCell<u8>,
    received: Rc<RefCell<VecDeque<String>>>,
    sent: Rc<RefCell<Vec<String>>>,
}

impl GatewaySocketFactory for RetryThenReadyFactory {
    fn connect(
        &self,
        _config: &GatewayConfig,
        _token: &str,
        _timeout: Duration,
    ) -> Result<Box<dyn GatewaySocket>, GatewayError> {
        let next = *self.attempts.borrow() + 1;
        let attempt = self.attempts.replace(next);
        if matches!(attempt, 1 | 3) {
            return Ok(Box::new(ScriptedSocket {
                received: self.received.clone(),
                sent: self.sent.clone(),
            }));
        }
        Err(GatewayError::Retryable)
    }
}

impl GatewaySocketFactory for RecordingFactory {
    fn connect(
        &self,
        _config: &GatewayConfig,
        _token: &str,
        timeout: Duration,
    ) -> Result<Box<dyn GatewaySocket>, GatewayError> {
        self.0.borrow_mut().push(timeout);
        Err(GatewayError::Retryable)
    }
}

struct FakeRuntime {
    now: RefCell<Instant>,
    sleeps: RefCell<Vec<Duration>>,
}

impl FakeRuntime {
    fn new() -> Self {
        Self {
            now: RefCell::new(Instant::now()),
            sleeps: RefCell::new(Vec::new()),
        }
    }

    fn advance(&self, duration: Duration) {
        *self.now.borrow_mut() += duration;
    }
}

struct SharedRuntime(Rc<FakeRuntime>);

impl EventRuntime for SharedRuntime {
    fn now(&self) -> Instant {
        *self.0.now.borrow()
    }

    fn sleep(&self, duration: Duration) {
        self.0.sleeps.borrow_mut().push(duration);
        *self.0.now.borrow_mut() += duration;
    }

    fn jitter(&self, maximum: Duration) -> Duration {
        maximum
    }
}

impl EventRuntime for FakeRuntime {
    fn now(&self) -> Instant {
        *self.now.borrow()
    }

    fn sleep(&self, duration: Duration) {
        self.sleeps.borrow_mut().push(duration);
        *self.now.borrow_mut() += duration;
    }

    fn jitter(&self, maximum: Duration) -> Duration {
        maximum
    }
}

impl GatewaySocketFactory for RetryableFactory {
    fn connect(
        &self,
        _config: &GatewayConfig,
        _token: &str,
        _timeout: Duration,
    ) -> Result<Box<dyn GatewaySocket>, GatewayError> {
        Err(GatewayError::Retryable)
    }
}

impl GatewaySocketFactory for FailingFactory {
    fn connect(
        &self,
        _config: &GatewayConfig,
        _token: &str,
        _timeout: Duration,
    ) -> Result<Box<dyn GatewaySocket>, GatewayError> {
        Err(GatewayError::Fatal("gateway authorization failed"))
    }
}

impl GatewaySocketFactory for ScriptedFactory {
    fn connect(
        &self,
        _config: &GatewayConfig,
        _token: &str,
        _timeout: Duration,
    ) -> Result<Box<dyn GatewaySocket>, GatewayError> {
        Ok(Box::new(ScriptedSocket {
            received: self.received.clone(),
            sent: self.sent.clone(),
        }))
    }
}

fn snapshot(state: &str) -> PrSnapshot {
    PrSnapshot {
        number: 63,
        title: "Event wait".to_string(),
        state: state.to_string(),
        is_draft: false,
        head_ref_name: "feature".to_string(),
        base_ref_name: "main".to_string(),
        head_oid: "abc123".to_string(),
        head_committed_at: None,
        owner: "example".to_string(),
        repo: "project".to_string(),
        checks: Vec::new(),
        bot_reviews: Vec::new(),
        findings: Vec::new(),
    }
}

#[test]
fn registration_uses_one_deadline_and_prevents_a_post_deadline_refetch() {
    let runtime = Rc::new(FakeRuntime::new());
    let timeouts = Rc::new(RefCell::new(Vec::new()));
    let mut source = EventWakeSource::with_runtime(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(TimedFactory {
            runtime: runtime.clone(),
            timeouts: timeouts.clone(),
        }),
        Box::new(SharedRuntime(runtime)),
    )
    .unwrap();
    let mut fetches = 0;
    let outcome = wait_until_settled(
        &mut |_| {
            fetches += 1;
            Ok(snapshot("OPEN"))
        },
        &mut source,
        Duration::from_secs(5),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap();

    assert!(matches!(outcome, WaitOutcome::TimedOut { .. }));
    assert_eq!(fetches, 1);
    assert_eq!(
        *timeouts.borrow(),
        [
            Duration::from_secs(5),
            Duration::from_secs(3),
            Duration::from_secs(1)
        ]
    );
}

#[test]
fn registration_attempt_caps_each_gateway_operation_at_thirty_seconds() {
    let runtime = Rc::new(FakeRuntime::new());
    let timeouts = Rc::new(RefCell::new(Vec::new()));
    let mut source = EventWakeSource::with_runtime(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(TimedFactory {
            runtime: runtime.clone(),
            timeouts: timeouts.clone(),
        }),
        Box::new(SharedRuntime(runtime)),
    )
    .unwrap();

    let action = babysit::wait::WakeSource::observe_snapshot(
        &mut source,
        &snapshot("OPEN"),
        Duration::from_secs(1800),
    )
    .unwrap();

    assert_eq!(action, babysit::wait::SnapshotAction::RefetchNow);
    assert_eq!(
        *timeouts.borrow(),
        [
            Duration::from_secs(30),
            Duration::from_secs(28),
            Duration::from_secs(26)
        ]
    );
}

#[test]
fn initial_ready_timeout_degrades_to_retryable_fallback() {
    let received = Rc::new(RefCell::new(VecDeque::new()));
    let mut source = EventWakeSource::with_dependencies(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(ScriptedFactory {
            received,
            sent: Rc::new(RefCell::new(Vec::new())),
        }),
    )
    .unwrap();
    let action = babysit::wait::WakeSource::observe_snapshot(
        &mut source,
        &snapshot("OPEN"),
        Duration::from_secs(5),
    )
    .unwrap();

    assert_eq!(action, babysit::wait::SnapshotAction::Wait);
}

#[test]
fn a_wake_causes_only_one_immediate_authoritative_refetch() {
    let runtime = Rc::new(FakeRuntime::new());
    let start = *runtime.now.borrow();
    let received = Rc::new(RefCell::new(VecDeque::from([
        r#"{"type":"ready","version":1,"cursor":45}"#.to_string(),
        r#"{"type":"wake","version":1,"cursor":46}"#.to_string(),
    ])));
    let mut source = EventWakeSource::with_runtime(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(ScriptedFactory {
            received,
            sent: Rc::new(RefCell::new(Vec::new())),
        }),
        Box::new(SharedRuntime(runtime.clone())),
    )
    .unwrap();
    let mut fetch_times = Vec::new();
    let mut snapshots = vec![
        snapshot("OPEN"),
        snapshot("OPEN"),
        snapshot("OPEN"),
        snapshot("CLOSED"),
    ]
    .into_iter();
    let outcome = wait_until_settled(
        &mut |_| {
            fetch_times.push(*runtime.now.borrow());
            Ok(snapshots.next().unwrap())
        },
        &mut source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap();

    assert!(matches!(outcome, WaitOutcome::Settled { .. }));
    assert_eq!(
        fetch_times,
        [start, start, start, start + Duration::from_secs(30),]
    );
}

#[test]
fn replay_at_ready_cursor_is_ignored_until_a_newer_wake_arrives() {
    let received = Rc::new(RefCell::new(VecDeque::from([
        r#"{"type":"ready","version":1,"cursor":45}"#.to_string(),
        r#"{"type":"replay","version":1,"cursor":45}"#.to_string(),
        r#"{"type":"wake","version":1,"cursor":46}"#.to_string(),
    ])));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let remaining = received.clone();
    let mut source = EventWakeSource::with_dependencies(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(ScriptedFactory { received, sent }),
    )
    .unwrap();
    let mut fetches = 0;
    let mut snapshots = vec![snapshot("OPEN"), snapshot("OPEN"), snapshot("CLOSED")].into_iter();
    let outcome = wait_until_settled(
        &mut |_| {
            fetches += 1;
            Ok(snapshots.next().unwrap())
        },
        &mut source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap();

    assert!(matches!(outcome, WaitOutcome::Settled { .. }));
    assert_eq!(fetches, 3);
    assert!(remaining.borrow().is_empty());
}

#[test]
fn head_change_replaces_registration_and_refetches_after_its_ready_cursor() {
    let received = Rc::new(RefCell::new(VecDeque::from([
        r#"{"type":"ready","version":1,"cursor":45}"#.to_string(),
        r#"{"type":"wake","version":1,"cursor":46}"#.to_string(),
        r#"{"type":"ready","version":1,"cursor":47}"#.to_string(),
    ])));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut source = EventWakeSource::with_dependencies(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(ScriptedFactory {
            received,
            sent: sent.clone(),
        }),
    )
    .unwrap();
    let mut changed = snapshot("OPEN");
    changed.head_oid = "def456".to_string();
    let mut snapshots = vec![
        snapshot("OPEN"),
        snapshot("OPEN"),
        changed,
        snapshot("CLOSED"),
    ]
    .into_iter();
    let outcome = wait_until_settled(
        &mut |_| Ok(snapshots.next().unwrap()),
        &mut source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap();

    assert!(matches!(outcome, WaitOutcome::Settled { .. }));
    assert_eq!(sent.borrow().len(), 2);
    assert!(sent.borrow()[1].contains("\"headOid\":\"def456\""));
    assert!(sent.borrow()[1].contains("\"after\":46"));
}

#[test]
fn authorization_failure_is_fatal_not_a_polling_fallback() {
    let mut source = EventWakeSource::with_dependencies(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(FailingFactory),
    )
    .unwrap();
    let error = wait_until_settled(
        &mut |_| Ok(snapshot("OPEN")),
        &mut source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap_err();

    assert_eq!(error.message, "gateway authorization failed");
    assert!(!error.retryable);
}

#[test]
fn connector_receives_the_remaining_deadline_as_its_timeout() {
    let runtime = Rc::new(FakeRuntime::new());
    let timeouts = Rc::new(RefCell::new(Vec::new()));
    let mut source = EventWakeSource::with_runtime(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(RecordingFactory(timeouts.clone())),
        Box::new(SharedRuntime(runtime)),
    )
    .unwrap();
    let action = babysit::wait::WakeSource::observe_snapshot(
        &mut source,
        &snapshot("OPEN"),
        Duration::from_secs(5),
    )
    .unwrap();

    assert_eq!(action, babysit::wait::SnapshotAction::Wait);
    assert_eq!(*timeouts.borrow(), [Duration::from_secs(5)]);
}

#[test]
fn successful_registration_resets_the_next_retry_delay() {
    let runtime = Rc::new(FakeRuntime::new());
    let received = Rc::new(RefCell::new(VecDeque::from([
        r#"{"type":"ready","version":1,"cursor":1}"#.to_string(),
        r#"{"type":"ready","version":1,"cursor":2}"#.to_string(),
    ])));
    let mut source = EventWakeSource::with_runtime(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(RetryThenReadyFactory {
            attempts: RefCell::new(0),
            received,
            sent: Rc::new(RefCell::new(Vec::new())),
        }),
        Box::new(SharedRuntime(runtime.clone())),
    )
    .unwrap();
    let open = snapshot("OPEN");
    assert_eq!(
        babysit::wait::WakeSource::observe_snapshot(&mut source, &open, Duration::from_secs(60))
            .unwrap(),
        babysit::wait::SnapshotAction::Wait
    );
    babysit::wait::WakeSource::wait(&mut source, Duration::from_secs(30)).unwrap();
    let mut changed = open.clone();
    changed.head_oid = "new-head".to_string();
    assert_eq!(
        babysit::wait::WakeSource::observe_snapshot(&mut source, &changed, Duration::from_secs(59))
            .unwrap(),
        babysit::wait::SnapshotAction::Wait
    );
    babysit::wait::WakeSource::wait(&mut source, Duration::from_secs(30)).unwrap();

    assert_eq!(
        *runtime.sleeps.borrow(),
        [Duration::from_secs(1), Duration::from_secs(1)]
    );
}

#[test]
fn repeated_reconnect_failures_wait_for_the_full_fallback_before_refetching() {
    let runtime = Rc::new(FakeRuntime::new());
    let start = *runtime.now.borrow();
    let mut source = EventWakeSource::with_runtime(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(RetryableFactory),
        Box::new(SharedRuntime(runtime.clone())),
    )
    .unwrap();
    let mut fetch_times = Vec::new();
    let outcome = wait_until_settled(
        &mut |_| {
            fetch_times.push(*runtime.now.borrow());
            Ok(snapshot("OPEN"))
        },
        &mut source,
        Duration::from_secs(601),
        Duration::from_secs(300),
        &SettleOptions::default(),
    )
    .unwrap();

    assert!(matches!(outcome, WaitOutcome::TimedOut { .. }));
    assert_eq!(
        fetch_times,
        [
            start,
            start + Duration::from_secs(300),
            start + Duration::from_secs(600),
        ]
    );
}

#[test]
fn successful_reconnect_requests_an_immediate_authoritative_fetch() {
    let runtime = Rc::new(FakeRuntime::new());
    let start = *runtime.now.borrow();
    let received = Rc::new(RefCell::new(VecDeque::from([
        r#"{"type":"ready","version":1,"cursor":1}"#.to_string(),
    ])));
    let mut source = EventWakeSource::with_runtime(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(RetryThenReadyFactory {
            attempts: RefCell::new(0),
            received,
            sent: Rc::new(RefCell::new(Vec::new())),
        }),
        Box::new(SharedRuntime(runtime.clone())),
    )
    .unwrap();
    let mut fetch_times = Vec::new();
    let mut states = ["OPEN", "CLOSED"].into_iter();
    let outcome = wait_until_settled(
        &mut |_| {
            fetch_times.push(*runtime.now.borrow());
            Ok(snapshot(states.next().unwrap()))
        },
        &mut source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap();

    assert!(matches!(outcome, WaitOutcome::Settled { .. }));
    assert_eq!(fetch_times, [start, start + Duration::from_secs(1)]);
}

#[test]
fn retryable_connection_failure_degrades_to_the_wait_loop() {
    let mut source = EventWakeSource::with_dependencies(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(RetryableFactory),
    )
    .unwrap();
    let action = babysit::wait::WakeSource::observe_snapshot(
        &mut source,
        &snapshot("OPEN"),
        Duration::from_secs(60),
    )
    .unwrap();

    assert_eq!(action, babysit::wait::SnapshotAction::Wait);
}

#[test]
fn resync_requests_an_authoritative_fetch() {
    let received = Rc::new(RefCell::new(VecDeque::from([
        r#"{"type":"ready","version":1,"cursor":45}"#.to_string(),
        r#"{"type":"resync","version":1,"cursor":46}"#.to_string(),
    ])));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut source = EventWakeSource::with_dependencies(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(ScriptedFactory { received, sent }),
    )
    .unwrap();
    let mut fetches = 0;
    let mut snapshots = vec![snapshot("OPEN"), snapshot("OPEN"), snapshot("CLOSED")].into_iter();
    let outcome = wait_until_settled(
        &mut |_| {
            fetches += 1;
            Ok(snapshots.next().unwrap())
        },
        &mut source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap();

    assert!(matches!(outcome, WaitOutcome::Settled { .. }));
    assert_eq!(fetches, 3);
}

#[test]
fn handshake_and_transport_failure_classes_follow_the_gateway_policy() {
    for status in [401, 403] {
        assert!(matches!(
            classify_gateway_status(status),
            GatewayError::Fatal("gateway authorization failed")
        ));
    }
    for status in [429, 500, 501, 502, 503, 504, 505, 599] {
        assert!(matches!(
            classify_gateway_status(status),
            GatewayError::Retryable
        ));
    }
    for kind in [
        std::io::ErrorKind::TimedOut,
        std::io::ErrorKind::ConnectionReset,
    ] {
        assert!(matches!(
            classify_transport_kind(kind),
            GatewayError::Retryable
        ));
    }
}

#[test]
fn malformed_gateway_frame_is_fatal_before_a_settled_snapshot_can_succeed() {
    let received = Rc::new(RefCell::new(VecDeque::from([
        r#"{"type":"ready","version":1,"cursor":45}"#.to_string(),
        "not-json".to_string(),
    ])));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut source = EventWakeSource::with_dependencies(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(
            SecretToken::new("test-token".to_string()).unwrap(),
        )),
        Box::new(ScriptedFactory { received, sent }),
    )
    .unwrap();
    let mut snapshots = vec![snapshot("OPEN"), snapshot("OPEN"), snapshot("CLOSED")].into_iter();
    let error = wait_until_settled(
        &mut |_| Ok(snapshots.next().unwrap()),
        &mut source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap_err();

    assert_eq!(error.message, "gateway protocol error");
}

#[test]
fn rejects_malformed_or_non_wss_gateway_urls() {
    for value in [
        "https://gateway.example",
        "wss://gateway.example",
        "wss://gateway.example/watch/example/project",
        "wss://gateway.example/path?token=no",
        "wss://user@gateway.example",
        "wss://gateway.example/#fragment",
    ] {
        assert!(GatewayConfig::parse(value).is_err(), "{value}");
    }
}

#[test]
fn ready_then_wake_causes_authoritative_refetches_without_sending_the_token_in_frames() {
    let received = Rc::new(RefCell::new(VecDeque::from([
        r#"{"type":"ready","version":1,"cursor":45}"#.to_string(),
        r#"{"type":"wake","version":1,"cursor":46}"#.to_string(),
    ])));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let token = SecretToken::new("do-not-send-this".to_string()).unwrap();
    let mut source = EventWakeSource::with_dependencies(
        GatewayConfig::parse("wss://gateway.example/watch").unwrap(),
        Box::new(MemoryStore(token)),
        Box::new(ScriptedFactory {
            received,
            sent: sent.clone(),
        }),
    )
    .unwrap();
    let mut snapshots = vec![snapshot("OPEN"), snapshot("OPEN"), snapshot("CLOSED")].into_iter();
    let outcome = wait_until_settled(
        &mut |_| Ok(snapshots.next().unwrap()),
        &mut source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap();

    assert!(matches!(outcome, WaitOutcome::Settled { .. }));
    assert_eq!(sent.borrow().len(), 1);
    assert!(sent.borrow()[0].contains("\"type\":\"register\""));
    assert!(sent.borrow()[0].contains("\"repository\":\"example/project\""));
    assert!(!sent.borrow()[0].contains("do-not-send-this"));
}
