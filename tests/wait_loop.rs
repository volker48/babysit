use std::time::{Duration, Instant};

use babysit::core::{CheckState, Finding, PrCheck, PrSnapshot, SettleOptions, exit_code_for};
use babysit::forge::CliError;
use babysit::wait::{WaitOutcome, WakeSource, wait_until_settled};

struct FakeWakeSource {
    now: Instant,
    waits: Vec<Duration>,
}

impl FakeWakeSource {
    fn new() -> Self {
        Self {
            now: Instant::now(),
            waits: Vec::new(),
        }
    }
}

impl WakeSource for FakeWakeSource {
    fn now(&self) -> Instant {
        self.now
    }

    fn wait(&mut self, duration: Duration) {
        self.waits.push(duration);
        self.now += duration;
    }
}

fn snapshot(state: &str) -> PrSnapshot {
    PrSnapshot {
        number: 1,
        title: "Test pull request".to_string(),
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
fn settled_clean() {
    let mut wake_source = FakeWakeSource::new();
    let mut fetches = 0;
    let outcome = wait_until_settled(
        &mut || {
            fetches += 1;
            Ok(snapshot("CLOSED"))
        },
        &mut wake_source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap();

    let WaitOutcome::Settled { snapshot, settle } = outcome else {
        panic!("expected settled outcome");
    };
    assert_eq!(fetches, 1);
    assert!(wake_source.waits.is_empty());
    assert_eq!(exit_code_for(&snapshot, &settle), 0);
}

#[test]
fn unsettled_snapshot_waits_then_returns_unresolved() {
    let mut wake_source = FakeWakeSource::new();
    let mut settled = snapshot("CLOSED");
    settled.findings.push(Finding {
        path: "src/lib.rs".to_string(),
        line: "1".to_string(),
        bot: "coderabbit".to_string(),
        severity: None,
        title: "Unresolved finding".to_string(),
        detail: "Fix this.".to_string(),
        resolved: false,
        outdated: false,
    });
    let mut snapshots = vec![snapshot("OPEN"), settled].into_iter();
    let outcome = wait_until_settled(
        &mut || Ok(snapshots.next().unwrap()),
        &mut wake_source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap();

    let WaitOutcome::Settled { snapshot, settle } = outcome else {
        panic!("expected settled outcome");
    };
    assert_eq!(wake_source.waits, [Duration::from_secs(30)]);
    assert_eq!(exit_code_for(&snapshot, &settle), 1);
}

#[test]
fn settled_failed_checks_preserve_exit_code() {
    let mut wake_source = FakeWakeSource::new();
    let mut failed = snapshot("CLOSED");
    failed.checks.push(PrCheck {
        name: "test".to_string(),
        state: CheckState::Failed,
    });
    let outcome = wait_until_settled(
        &mut || Ok(failed.clone()),
        &mut wake_source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap();

    let WaitOutcome::Settled { snapshot, settle } = outcome else {
        panic!("expected settled outcome");
    };
    assert!(wake_source.waits.is_empty());
    assert_eq!(exit_code_for(&snapshot, &settle), 2);
}

#[test]
fn pending_snapshot_times_out() {
    let mut wake_source = FakeWakeSource::new();
    let mut fetches = 0;
    let outcome = wait_until_settled(
        &mut || {
            fetches += 1;
            Ok(snapshot("OPEN"))
        },
        &mut wake_source,
        Duration::from_secs(60),
        Duration::from_secs(45),
        &SettleOptions::default(),
    )
    .unwrap();

    let WaitOutcome::TimedOut { snapshot, settle } = outcome else {
        panic!("expected timeout outcome");
    };
    assert_eq!(fetches, 3);
    assert_eq!(
        wake_source.waits,
        [Duration::from_secs(45), Duration::from_secs(15)]
    );
    assert_eq!(exit_code_for(&snapshot, &settle), 3);
}

#[test]
fn retryable_error_waits_and_retries() {
    let mut wake_source = FakeWakeSource::new();
    let mut results = vec![
        Err(CliError::new("temporary forge failure", true)),
        Ok(snapshot("CLOSED")),
    ]
    .into_iter();
    let outcome = wait_until_settled(
        &mut || results.next().unwrap(),
        &mut wake_source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap();

    let WaitOutcome::Settled { snapshot, settle } = outcome else {
        panic!("expected settled outcome");
    };
    assert_eq!(wake_source.waits, [Duration::from_secs(30)]);
    assert_eq!(exit_code_for(&snapshot, &settle), 0);
}

#[test]
fn non_retryable_error_returns_immediately() {
    let mut wake_source = FakeWakeSource::new();
    let error = wait_until_settled(
        &mut || Err(CliError::new("invalid forge response", false)),
        &mut wake_source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap_err();

    assert_eq!(error.message, "invalid forge response");
    assert!(!error.retryable);
    assert!(wake_source.waits.is_empty());
}

#[test]
fn retryable_error_at_deadline_returns_immediately() {
    let mut wake_source = FakeWakeSource::new();
    let error = wait_until_settled(
        &mut || Err(CliError::new("deadline forge failure", true)),
        &mut wake_source,
        Duration::ZERO,
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap_err();

    assert_eq!(error.message, "deadline forge failure");
    assert_eq!(error.exit_code, 4);
    assert!(error.retryable);
    assert!(wake_source.waits.is_empty());
}
