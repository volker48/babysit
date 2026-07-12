# Plan 002: Make the wait loop honor the last fetched snapshot at the deadline

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 9b5cac3..HEAD -- src/wait.rs tests/wait_loop.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: MED (deliberately reverses behavior pinned by an existing test — see below)
- **Depends on**: none (Plan 001 gives it CI coverage; not a hard dependency)
- **Category**: bug
- **Planned at**: commit `9b5cac3`, 2026-07-11

## Why this matters

`babysit wait` fetches authoritative PR snapshots until the PR settles or a timeout elapses. Exit codes are a product contract: 0 clean, 1 unresolved findings, 2 failed checks, 3 pending/timed out. Today, when a snapshot fetch *completes after the deadline passes* (slow `gh` call near the timeout), the freshly fetched snapshot is **discarded**: even if it shows the PR settled, the loop reports a timeout, and if no earlier snapshot exists it reports `TIMEOUT: no authoritative snapshot was fetched` — which is factually false, a snapshot *was* fetched. Agents scripting on top of babysit get exit code 3 ("pending") for PRs that are actually settled, and a misleading message. After this plan, a successfully fetched snapshot is always evaluated: settled → `Settled` (even past the deadline), unsettled past deadline → `TimedOut` carrying that fresh snapshot.

**⚠️ Deliberate behavior reversal.** The current discard behavior is pinned by the test `settled_snapshot_after_deadline_times_out_without_accepting_it` (`tests/wait_loop.rs:104-123`). That test was introduced wholesale in commit `f4d6482` ("Add event-assisted wait client") with no recorded rationale (no comment, no PR discussion found), so it is treated as incidental characterization, not intent. This plan replaces it. The repo owner approved this plan knowing that; if you find NEW evidence the discard is intentional (a comment or doc added since `9b5cac3`), STOP.

## Current state

- `src/wait.rs` — the whole wait loop (117 lines). The bug is the first match arm:
  ```rust
  // src/wait.rs:77-95
  match fetch_snapshot(remaining) {
      Ok(_snapshot) if wake_source.now() >= deadline => {
          return Ok(timeout_outcome(last_snapshot));   // <-- fresh snapshot discarded
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
  ```
  After the match, the loop already re-checks the deadline before sleeping:
  ```rust
  // src/wait.rs:99-103
  let remaining = deadline.saturating_duration_since(wake_source.now());
  if remaining.is_zero() {
      return Ok(timeout_outcome(last_snapshot));
  }
  wake_source.wait(interval.min(remaining))?;
  ```
  and `timeout_outcome` (`src/wait.rs:112-117`) maps `Some((snapshot, settle))` → `WaitOutcome::TimedOut { snapshot, settle }`, `None` → `WaitOutcome::TimedOutWithoutSnapshot`.
- `src/cli.rs:436-437` — renders `TimedOutWithoutSnapshot` as `TIMEOUT: no authoritative snapshot was fetched` (context only; do not modify).
- `tests/wait_loop.rs` — test conventions to follow:
  - `ClockWakeSource` (lines 43-58): a `Rc<RefCell<Instant>>` clock shared with the fetch closure so fetches can advance time.
  - `snapshot(state)` helper (lines 60-76) builds a `PrSnapshot`; `"CLOSED"` settles, `"OPEN"` does not.
  - The test to replace, verbatim:
    ```rust
    // tests/wait_loop.rs:103-123
    #[test]
    fn settled_snapshot_after_deadline_times_out_without_accepting_it() {
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
            Duration::from_secs(1),
            Duration::from_secs(1),
            &SettleOptions::default(),
        )
        .unwrap();

        assert!(matches!(outcome, WaitOutcome::TimedOutWithoutSnapshot));
    }
    ```
    (Note: the real file passes `&mut wake_source` between the closure and the durations — copy the structure from the file, not from this excerpt.)
  - `retryable_fetch_at_deadline_uses_the_last_snapshot` (lines 126-151) covers the *error*-at-deadline path; its semantics must not change.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Format | `cargo fmt --all --check` | exit 0 |
| Lint | `cargo clippy --locked --all-targets --all-features -- -D warnings` | exit 0 |
| Focused tests | `cargo test --locked --test wait_loop` | all pass |
| Full tests | `cargo test --locked --all` | all pass (119+ at baseline) |

## Scope

**In scope** (the only files you should modify):
- `src/wait.rs`
- `tests/wait_loop.rs`

**Out of scope** (do NOT touch, even though they look related):
- `src/event.rs`, `tests/event_wait.rs` — the event wake source builds on this loop; its own idle bug is Plan 005.
- `src/cli.rs` rendering and exit codes — `WaitOutcome` variants keep their meanings; only *which* variant is returned in the late-fetch case changes.
- `timeout_outcome` and the `WaitOutcome` enum shape — no new variants.

## Git workflow

- Branch: `advisor/002-wait-accepts-fresh-snapshot`
- Commit style: imperative, ≤72-char subject, e.g. `Honor snapshots fetched at the wait deadline`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Write the failing tests first

In `tests/wait_loop.rs`, replace `settled_snapshot_after_deadline_times_out_without_accepting_it` (lines 103-123) with these two tests (same `ClockWakeSource` + clock-advancing-closure structure):

1. `settled_snapshot_fetched_past_deadline_is_accepted` — identical setup to the removed test (fetch advances the shared clock by 5s, timeout 1s, returns `snapshot("CLOSED")`), but assert `matches!(outcome, WaitOutcome::Settled { .. })`.
2. `unsettled_snapshot_fetched_past_deadline_times_out_with_that_snapshot` — same setup but the fetch returns `snapshot("OPEN")`. Assert the outcome is `WaitOutcome::TimedOut { snapshot, .. }` **and** that `snapshot.state == "OPEN"` (destructure with `if let`/`match` rather than only `matches!`, so the assertion proves the fresh snapshot was kept).

**Verify**: `cargo test --locked --test wait_loop` → exactly these 2 new tests FAIL; every other test in the file passes.

### Step 2: Remove the discarding match arm

In `src/wait.rs`, delete the arm at lines 78-80:

```rust
Ok(_snapshot) if wake_source.now() >= deadline => {
    return Ok(timeout_outcome(last_snapshot));
}
```

so every successful fetch flows into the existing `Ok(snapshot)` arm. No other logic change is needed: a settled snapshot returns `Settled` regardless of the clock, and an unsettled one is stored in `last_snapshot` before the deadline re-check at lines 99-102 (and the `RefetchNow` deadline check at lines 90-92) returns `TimedOut` carrying it.

**Verify**: `cargo test --locked --test wait_loop` → ALL tests pass, including the 2 from Step 1.

### Step 3: Run the full gate

**Verify**, in order:
1. `cargo fmt --all --check` → exit 0 (run `cargo fmt --all` first if needed)
2. `cargo clippy --locked --all-targets --all-features -- -D warnings` → exit 0
3. `cargo test --locked --all` → all pass. Pay attention to `tests/event_wait.rs` — it drives this same loop through `EventWakeSource`; all its tests passed at baseline and must still pass. If an `event_wait` test fails, STOP (do not edit that file).

## Test plan

- New tests (Step 1) in `tests/wait_loop.rs`: settled-past-deadline → `Settled`; unsettled-past-deadline → `TimedOut` with the fresh snapshot. Both verified failing before the fix and passing after.
- Regression guard: `retryable_fetch_at_deadline_uses_the_last_snapshot` and `fetches_receive_shrinking_remaining_budget` must pass unmodified.
- Verification: `cargo test --locked --all` → all pass.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `grep -n "settled_snapshot_after_deadline_times_out_without_accepting_it" tests/wait_loop.rs` → no matches
- [ ] `grep -c "past_deadline" tests/wait_loop.rs` ≥ 2 (both new tests present)
- [ ] `grep -n "Ok(_snapshot)" src/wait.rs` → no matches
- [ ] `cargo fmt --all --check`, `cargo clippy --locked --all-targets --all-features -- -D warnings`, `cargo test --locked --all` all exit 0
- [ ] `git status --porcelain` shows only `src/wait.rs`, `tests/wait_loop.rs` (and `plans/README.md`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- `src/wait.rs:77-95` no longer matches the excerpt above.
- You find a comment, doc, or commit since `9b5cac3` stating the at-deadline discard is intentional.
- Any test in `tests/event_wait.rs` or the unmodified `wait_loop` tests fails after Step 2 — the fix should be a pure arm deletion; collateral failures mean an interaction this plan didn't foresee.
- Step 1's new tests do not fail before the fix (would mean the bug is already fixed or mis-diagnosed).

## Maintenance notes

- Reviewers should scrutinize exactly one thing: that accepting a settled snapshot past the deadline is desired (it changes `wait`'s exit code from 3 to 0/1/2 in the slow-final-fetch case). This is the intended product improvement.
- Plan 005 (event idle reconnect) touches the callers of this loop's `WakeSource` seam; land this first so its tests run against the corrected loop.
- If a future change adds a "hard deadline even if settled" mode, it must be a new explicit option, not a revival of the silent discard.
