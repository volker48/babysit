use std::time::{Duration, Instant};

use babysit::core::{CheckState, Finding, PrCheck, PrSnapshot, SettleOptions, exit_code_for};
use babysit::forge::CliError;
use babysit::wait::{SnapshotAction, WaitOutcome, WakeSource, wait_until_settled};

struct FakeWakeSource {
    now: Instant,
    waits: Vec<Duration>,
    actions: Vec<SnapshotAction>,
}

impl FakeWakeSource {
    fn new() -> Self {
        Self {
            now: Instant::now(),
            waits: Vec::new(),
            actions: Vec::new(),
        }
    }
}

impl WakeSource for FakeWakeSource {
    fn now(&self) -> Instant {
        self.now
    }

    fn wait(&mut self, duration: Duration) -> Result<(), CliError> {
        self.waits.push(duration);
        self.now += duration;
        Ok(())
    }

    fn observe_snapshot(
        &mut self,
        _snapshot: &PrSnapshot,
        _remaining: Duration,
    ) -> Result<SnapshotAction, CliError> {
        Ok(self.actions.pop().unwrap_or(SnapshotAction::Wait))
    }
}

struct ClockWakeSource {
    now: std::rc::Rc<std::cell::RefCell<Instant>>,
    waits: Vec<Duration>,
}

impl WakeSource for ClockWakeSource {
    fn now(&self) -> Instant {
        *self.now.borrow()
    }

    fn wait(&mut self, duration: Duration) -> Result<(), CliError> {
        self.waits.push(duration);
        *self.now.borrow_mut() += duration;
        Ok(())
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
fn fetches_receive_shrinking_remaining_budget() {
    let clock = std::rc::Rc::new(std::cell::RefCell::new(Instant::now()));
    let mut wake_source = ClockWakeSource {
        now: clock.clone(),
        waits: Vec::new(),
    };
    let mut budgets = Vec::new();
    let mut states = ["OPEN", "CLOSED"].into_iter();
    let outcome = wait_until_settled(
        &mut |remaining| {
            budgets.push(remaining);
            Ok(snapshot(states.next().unwrap()))
        },
        &mut wake_source,
        Duration::from_secs(5),
        Duration::from_secs(2),
        &SettleOptions::default(),
    )
    .unwrap();

    assert!(matches!(outcome, WaitOutcome::Settled { .. }));
    assert_eq!(budgets, [Duration::from_secs(5), Duration::from_secs(3)]);
}

#[test]
fn settled_snapshot_fetched_past_deadline_is_accepted() {
    let clock = std::rc::Rc::new(std::cell::RefCell::new(Instant::now()));
    let mut wake_source = ClockWakeSource {
        now: clock.clone(),
        waits: Vec::new(),
    };
    let outcome = wait_until_settled(
        &mut |_| {
            *clock.borrow_mut() += Duration::from_secs(5);
            Ok(snapshot("CLOSED"))
        },
        &mut wake_source,
        Duration::from_secs(1),
        Duration::from_secs(1),
        &SettleOptions::default(),
    )
    .unwrap();

    assert!(matches!(outcome, WaitOutcome::Settled { .. }));
}

#[test]
fn unsettled_snapshot_fetched_past_deadline_times_out_with_that_snapshot() {
    let clock = std::rc::Rc::new(std::cell::RefCell::new(Instant::now()));
    let mut wake_source = ClockWakeSource {
        now: clock.clone(),
        waits: Vec::new(),
    };
    let outcome = wait_until_settled(
        &mut |_| {
            *clock.borrow_mut() += Duration::from_secs(5);
            Ok(snapshot("OPEN"))
        },
        &mut wake_source,
        Duration::from_secs(1),
        Duration::from_secs(1),
        &SettleOptions::default(),
    )
    .unwrap();

    let WaitOutcome::TimedOut { snapshot, .. } = outcome else {
        panic!("expected timeout outcome");
    };
    assert_eq!(snapshot.state, "OPEN");
}

#[test]
fn retryable_fetch_at_deadline_uses_the_last_snapshot() {
    let clock = std::rc::Rc::new(std::cell::RefCell::new(Instant::now()));
    let mut wake_source = ClockWakeSource {
        now: clock.clone(),
        waits: Vec::new(),
    };
    let mut calls = 0;
    let outcome = wait_until_settled(
        &mut |_| {
            calls += 1;
            if calls == 1 {
                Ok(snapshot("OPEN"))
            } else {
                *clock.borrow_mut() += Duration::from_secs(1);
                Err(CliError::new("fetch timed out", true))
            }
        },
        &mut wake_source,
        Duration::from_secs(2),
        Duration::from_secs(1),
        &SettleOptions::default(),
    )
    .unwrap();

    assert!(matches!(outcome, WaitOutcome::TimedOut { .. }));
}

#[test]
fn source_requested_refetch_skips_sleep_and_uses_authoritative_snapshot() {
    let mut wake_source = FakeWakeSource::new();
    wake_source.actions.push(SnapshotAction::RefetchNow);
    let mut snapshots = vec![snapshot("OPEN"), snapshot("CLOSED")].into_iter();
    let outcome = wait_until_settled(
        &mut |_| Ok(snapshots.next().unwrap()),
        &mut wake_source,
        Duration::from_secs(60),
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap();

    assert!(matches!(outcome, WaitOutcome::Settled { .. }));
    assert!(wake_source.waits.is_empty());
}

#[test]
fn settled_clean() {
    let mut wake_source = FakeWakeSource::new();
    let mut fetches = 0;
    let outcome = wait_until_settled(
        &mut |_| {
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
        &mut |_| Ok(snapshots.next().unwrap()),
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
        &mut |_| Ok(failed.clone()),
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
        &mut |_| {
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
    assert_eq!(fetches, 2);
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
        &mut |_| results.next().unwrap(),
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
        &mut |_| Err(CliError::new("invalid forge response", false)),
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
    let outcome = wait_until_settled(
        &mut |_| Err(CliError::new("deadline forge failure", true)),
        &mut wake_source,
        Duration::ZERO,
        Duration::from_secs(30),
        &SettleOptions::default(),
    )
    .unwrap();

    assert!(matches!(outcome, WaitOutcome::TimedOutWithoutSnapshot));
    assert!(wake_source.waits.is_empty());
}
