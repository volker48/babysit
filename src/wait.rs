use std::thread;
use std::time::{Duration, Instant};

use crate::core::{PrSnapshot, SettleOptions, SettleResult, evaluate_settled};
use crate::forge::CliError;

/// A post-snapshot action requested by an event-aware wake source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotAction {
    /// Continue with the normal bounded wait.
    Wait,
    /// Fetch another authoritative snapshot immediately.
    RefetchNow,
}

/// Supplies monotonic time and bounded wakeups for the wait loop.
pub trait WakeSource {
    fn now(&self) -> Instant;
    fn wait(&mut self, duration: Duration) -> Result<(), CliError>;

    fn observe_snapshot(
        &mut self,
        _snapshot: &PrSnapshot,
        _remaining: Duration,
    ) -> Result<SnapshotAction, CliError> {
        Ok(SnapshotAction::Wait)
    }
}

/// Polling wake source backed by the system monotonic clock and thread sleep.
pub struct PollingWakeSource;

impl WakeSource for PollingWakeSource {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wait(&mut self, duration: Duration) -> Result<(), CliError> {
        thread::sleep(duration);
        Ok(())
    }
}

/// The authoritative snapshot and settle evaluation that ended a wait loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    Settled {
        snapshot: PrSnapshot,
        settle: SettleResult,
    },
    TimedOut {
        snapshot: PrSnapshot,
        settle: SettleResult,
    },
    TimedOutWithoutSnapshot,
}

/// Fetches bounded authoritative snapshots until settlement, timeout, or provider error.
pub fn wait_until_settled<F, W>(
    fetch_snapshot: &mut F,
    wake_source: &mut W,
    timeout: Duration,
    interval: Duration,
    settle_options: &SettleOptions,
) -> Result<WaitOutcome, CliError>
where
    F: FnMut(Duration) -> Result<PrSnapshot, CliError>,
    W: WakeSource + ?Sized,
{
    let deadline = wait_deadline(wake_source.now(), timeout)?;
    let mut last_snapshot = None;
    loop {
        let remaining = deadline.saturating_duration_since(wake_source.now());
        if remaining.is_zero() {
            return Ok(timeout_outcome(last_snapshot));
        }
        match fetch_snapshot(remaining) {
            Ok(_snapshot) if wake_source.now() >= deadline => {
                return Ok(timeout_outcome(last_snapshot));
            }
            Ok(snapshot) => {
                let settle = evaluate_settled(&snapshot, settle_options);
                if settle.settled {
                    return Ok(WaitOutcome::Settled { snapshot, settle });
                }
                last_snapshot = Some((snapshot.clone(), settle.clone()));
                let remaining = deadline.saturating_duration_since(wake_source.now());
                if wake_source.observe_snapshot(&snapshot, remaining)? == SnapshotAction::RefetchNow
                {
                    if wake_source.now() >= deadline {
                        return Ok(timeout_outcome(last_snapshot));
                    }
                    continue;
                }
            }
            Err(error) if error.retryable => {}
            Err(error) => return Err(error),
        }
        let remaining = deadline.saturating_duration_since(wake_source.now());
        if remaining.is_zero() {
            return Ok(timeout_outcome(last_snapshot));
        }
        wake_source.wait(interval.min(remaining))?;
    }
}

fn wait_deadline(now: Instant, timeout: Duration) -> Result<Instant, CliError> {
    now.checked_add(timeout)
        .ok_or_else(|| CliError::new("--timeout is too large", false))
}

fn timeout_outcome(last_snapshot: Option<(PrSnapshot, SettleResult)>) -> WaitOutcome {
    match last_snapshot {
        Some((snapshot, settle)) => WaitOutcome::TimedOut { snapshot, settle },
        None => WaitOutcome::TimedOutWithoutSnapshot,
    }
}
