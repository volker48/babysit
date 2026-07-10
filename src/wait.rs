use std::thread;
use std::time::{Duration, Instant};

use crate::core::{PrSnapshot, SettleOptions, SettleResult, evaluate_settled};
use crate::forge::CliError;

/// Supplies monotonic time and bounded wakeups for the wait loop.
pub trait WakeSource {
    /// Returns the current monotonic time used to calculate the overall deadline.
    fn now(&self) -> Instant;

    /// Waits for at most `duration` before returning.
    ///
    /// Event-backed implementations may return early when an event arrives. Every completed wait
    /// is followed by a fresh authoritative snapshot fetch before another settle decision.
    fn wait(&mut self, duration: Duration);
}

/// Polling wake source backed by the system monotonic clock and thread sleep.
pub struct PollingWakeSource;

impl WakeSource for PollingWakeSource {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wait(&mut self, duration: Duration) {
        thread::sleep(duration);
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
}

/// Fetches authoritative snapshots until settlement, timeout, or a provider error.
///
/// Each completed [`WakeSource::wait`] is followed by a fresh authoritative fetch. A wake source
/// may return early for an event, but this loop never asks it to block longer than the interval
/// capped by the remaining timeout. Retryable errors are retried only before the deadline.
pub fn wait_until_settled<F, W>(
    fetch_snapshot: &mut F,
    wake_source: &mut W,
    timeout: Duration,
    interval: Duration,
    settle_options: &SettleOptions,
) -> Result<WaitOutcome, CliError>
where
    F: FnMut() -> Result<PrSnapshot, CliError>,
    W: WakeSource,
{
    let deadline = wake_source
        .now()
        .checked_add(timeout)
        .ok_or_else(|| CliError::new("--timeout is too large", false))?;
    loop {
        match fetch_snapshot() {
            Ok(snapshot) => {
                let settle = evaluate_settled(&snapshot, settle_options);
                if settle.settled {
                    return Ok(WaitOutcome::Settled { snapshot, settle });
                }
                if wake_source.now() >= deadline {
                    return Ok(WaitOutcome::TimedOut { snapshot, settle });
                }
            }
            Err(error) if error.retryable && wake_source.now() < deadline => {}
            Err(error) => return Err(error),
        }
        let remaining = deadline.saturating_duration_since(wake_source.now());
        wake_source.wait(interval.min(remaining));
    }
}
