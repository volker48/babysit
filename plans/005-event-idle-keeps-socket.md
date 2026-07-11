# Plan 005: Keep the gateway socket across idle read timeouts in event mode

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 9b5cac3..HEAD -- src/event.rs src/wait.rs tests/event_wait.rs`
> Plan 002 is expected to have changed `src/wait.rs`/`tests/wait_loop.rs`; that
> is fine. If `src/event.rs` or `tests/event_wait.rs` changed since `9b5cac3`,
> compare the "Current state" excerpts against the live code before proceeding;
> on a mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED (touches the reconnect state machine of the event client)
- **Depends on**: plans/002-wait-accepts-fresh-snapshot-at-deadline.md (land first so event tests run against the corrected wait loop)
- **Category**: bug
- **Planned at**: commit `9b5cac3`, 2026-07-11

## Why this matters

Event mode (`babysit wait --events --gateway-url wss://...`) is supposed to *reduce* work versus polling: hold one WebSocket open, wake on gateway frames, fall back to a 300s poll. Today, an **idle interval** — no wake arrives before the fallback deadline — tears the connection down and doubles the fetch load: the benign read timeout makes the client drop a perfectly healthy socket, and on the next loop iteration `observe_snapshot` sees `socket.is_none()`, reconnects (TCP + TLS + register), and requests an *extra* immediate authoritative `gh` fetch via `RefetchNow`. Net effect per quiet interval: one full reconnect plus ~2× the `gh` fetches of plain polling — the opposite of the feature's purpose, plus needless gateway connection churn. After this plan, an idle read timeout keeps the socket and simply falls through to the normal interval fetch; reconnects happen only on real transport errors.

## Current state

(All line numbers at commit `9b5cac3`.)

- `src/event.rs` — `EventWakeSource` (implements the `WakeSource` seam from `src/wait.rs`).
  - The read path, `wait_for_socket`:
    ```rust
    // src/event.rs:271-294
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
                self.socket = None;          // <-- healthy socket dropped on idle timeout
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
    ```
  - `Ok(None)` from `read_text` means "no message before the timeout" — the production socket maps a benign OS read timeout to it (`src/event.rs:491`: `Err(tungstenite::Error::Io(error)) if is_read_timeout(&error) => Ok(None)`), and `read_text_until`'s `None => return Ok(None)` (line 523) propagates it. It is NOT a transport failure.
  - One wrinkle: `read_text_until`'s loop entry calls `remaining_timeout_at(deadline, now())?` (line 516), and `remaining_timeout_at` (lines 532-538) returns `Err(GatewayError::Retryable)` when the deadline is already exhausted — so a deadline that expires exactly while handling a ping/pong surfaces as `Retryable` and also drops the socket.
  - The reconnect trigger, `observe_snapshot`:
    ```rust
    // src/event.rs:337-354 (excerpt)
    let watch = WatchRegistration::from_snapshot(snapshot);
    if self.socket.is_none() || self.watch.as_ref() != Some(&watch) {
        self.watch = Some(watch);
        self.ready_cursor = None;
        self.connect_and_register(remaining)?;
        return Ok(if self.socket.is_some() {
            SnapshotAction::RefetchNow    // <-- the extra gh fetch after every idle interval
        } else {
            SnapshotAction::Wait
        });
    }
    Ok(SnapshotAction::Wait)
    ```
  - `register()` treats an `Ok(None)` ready-read as `ready_timeout()` (a retryable `CliError`, `src/event.rs:368-370`) — registration semantics are unaffected by this plan.
- `tests/event_wait.rs` — the fake seams (lines 1-260): `ScriptedSocket` (`read_text` pops a queue; empty queue → `Ok(None)`), `PlannedFactory`/`PlannedSocket` (a `VecDeque<Option<String>>` per connection; a `None` entry → `read_text` returns `Ok(None)`), `FakeRuntime`/`SharedRuntime` (fake clock; `sleep` records and advances), `MemoryStore` (in-memory token).
  - **Test that pins the buggy behavior** — `established_read_timeout_falls_back_then_reconnects_and_refetches_after_ready` (lines 428-491): first socket plan is `[ready(cursor 1), None]`; after the `None` idle timeout the source is *expected* to reconnect (second plan `Err(Retryable)`, third plan `ready(cursor 2)`) and refetch at `start + 31s`, asserting `fetch_times == [start, start, start+30, start+31]`, `sleeps == [30s, 1s]`, and a re-register with `"after":1`. This plan rewrites that test.
  - Healthy-behavior test that must keep passing — `a_wake_causes_only_one_immediate_authoritative_refetch` (lines 382-426): `ScriptedFactory` + queue `[ready(45), wake(46)]`, asserts `fetch_times == [start, start, start, start+30]`.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Focused tests | `cargo test --locked --test event_wait` | all pass |
| Full tests | `cargo test --locked --all` | all pass |
| Lint | `cargo clippy --locked --all-targets --all-features -- -D warnings` | exit 0 |
| Format | `cargo fmt --all --check` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `src/event.rs`
- `tests/event_wait.rs`

**Out of scope** (do NOT touch):
- `src/wait.rs`, `tests/wait_loop.rs` — the loop contract (`WakeSource`, `SnapshotAction`) is unchanged.
- `gateway/**` — server side is Plans 003/004.
- The retry/backoff machinery (`next_retry_delay`, `jittered_delay`, `reconnect_during_wait`) and `register()`.
- `observe_snapshot` — its logic stays; it stops firing on idle because the socket survives.

## Git workflow

- Branch: `advisor/005-event-idle-keeps-socket`
- Commit style: imperative, ≤72-char subject, e.g. `Keep gateway socket across idle read timeouts`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Write the new idle test first

In `tests/event_wait.rs`, add `established_idle_timeout_keeps_the_socket_and_fetches_on_interval`, structured like the lines 428-491 test but with a `PlannedFactory` holding **exactly one** connection plan: `Ok(VecDeque::from([Some(ready cursor 1), None, None]))`. Drive `wait_until_settled` with timeout 90s, interval 30s, snapshots `[OPEN, OPEN, OPEN, CLOSED]` (recording `fetch_times` from the shared `FakeRuntime` clock). Assert:

- outcome is `Settled`;
- `fetch_times == [start, start, start + 30s, start + 60s]` — the two leading `start` fetches are the initial fetch plus the post-registration `RefetchNow`, then one fetch per interval with **no** extra refetches;
- `sent.borrow().len() == 1` — a single registration, i.e. never reconnected (the factory would panic/`Retryable` on a second `connect` since it has one plan; the length check makes the intent explicit).

**Verify**: `cargo test --locked --test event_wait` → the new test FAILS (currently the second connect attempt finds no plan → behavior diverges); all pre-existing tests still pass.

### Step 2: Keep the socket on idle read timeout

In `src/event.rs` `wait_for_socket`, change only the `Ok(None)` arm — do not clear the socket:

```rust
Ok(None) => {
    self.sleep_until(deadline);
    Ok(true)
}
```

(`sleep_until` stays: the production socket has consumed real time in `read_text`, but the fake clock in tests has not; and after a ping-only exchange the remaining budget must still be honored.)

**Verify**: `cargo test --locked --test event_wait` → the Step 1 test passes; `established_read_timeout_falls_back_then_reconnects_and_refetches_after_ready` now FAILS (expected — Step 4); everything else passes.

### Step 3: Make deadline exhaustion inside a read surface as idle, not retryable

In `read_text_until` (src/event.rs:503-526), change the loop entry so an exhausted deadline returns `Ok(None)` instead of propagating `Err(GatewayError::Retryable)`:

```rust
let Ok(timeout) = remaining_timeout_at(deadline, now()) else {
    return Ok(None);
};
```

Leave the *flush* call's `remaining_timeout_at(...)?` (line 519) as-is — failing a pong write near the deadline is a genuine transport problem. Leave `remaining_timeout_at` itself unchanged (it is used by send/connect paths where exhaustion must stay retryable).

**Verify**: `cargo test --locked --test event_wait` → same pass/fail set as after Step 2 (this branch is exercised only through the production `TungsteniteSocket`; unit behavior is covered by the existing `read_text_until` doctests/tests if present — if `rg -n "read_text_until" tests/ src/event.rs` shows dedicated unit tests, confirm they still pass or update their exhaustion-case expectation to `Ok(None)`).

### Step 4: Rewrite the reconnect test to cover a *real* transport error

Replace `established_read_timeout_falls_back_then_reconnects_and_refetches_after_ready` with `established_transport_error_reconnects_and_refetches_after_ready`: same three-plan `PlannedFactory` shape, but make the first connection end in a transport error instead of idle. `PlannedSocket::read_text` returns `Ok(self.received.pop_front().flatten())` and cannot yield `Err`; extend the fake minimally — change `PlannedSocket.received` to `VecDeque<Result<Option<String>, GatewayError>>` (or add an `errors_after` flag), keeping all other tests' plan literals compiling (update the handful of `VecDeque::from([...])` literals mechanically, e.g. wrap entries in `Ok(...)`). First plan: `[Ok(Some(ready 1)), Err(GatewayError::Retryable)]`; second plan `Err(GatewayError::Retryable)` (connect fails); third `[Ok(Some(ready 2))]`. Keep the original assertions: refetch after re-ready with `"after":1` in the second registration, backoff sleep of 1s recorded.

**Verify**: `cargo test --locked --test event_wait` → ALL tests pass.

### Step 5: Full gate

**Verify**: `cargo fmt --all --check`, `cargo clippy --locked --all-targets --all-features -- -D warnings`, `cargo test --locked --all` → all exit 0.

## Test plan

- New: `established_idle_timeout_keeps_the_socket_and_fetches_on_interval` (Step 1) — fails before the fix, passes after; proves one connect, one registration, interval-paced fetches with no doubling.
- Rewritten: `established_transport_error_reconnects_and_refetches_after_ready` (Step 4) — preserves coverage of the legitimate reconnect path (backoff, re-register with `after`, post-ready refetch).
- Regression guards, unmodified: `a_wake_causes_only_one_immediate_authoritative_refetch`, `replay_at_ready_cursor_is_ignored_until_a_newer_wake_arrives`, `repeated_reconnect_failures_wait_for_the_full_fallback_before_refetching`, `successful_registration_resets_the_next_retry_delay`, and the registration/deadline tests (lines 282-380).

## Done criteria

Machine-checkable. ALL must hold:

- [ ] In `src/event.rs`, the `Ok(None)` arm of `wait_for_socket` no longer contains `self.socket = None` (`sed -n '271,300p' src/event.rs` to inspect)
- [ ] `grep -n "established_idle_timeout_keeps_the_socket" tests/event_wait.rs` → 1 match
- [ ] `grep -n "established_read_timeout_falls_back" tests/event_wait.rs` → no matches
- [ ] `cargo fmt --all --check`, `cargo clippy --locked --all-targets --all-features -- -D warnings`, `cargo test --locked --all` all exit 0
- [ ] `git status --porcelain` shows only `src/event.rs`, `tests/event_wait.rs` (and `plans/README.md`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- `wait_for_socket`, `observe_snapshot`, or `read_text_until` don't match the excerpts above.
- The Step 1 test does not fail before the fix.
- After Step 2, any test other than `established_read_timeout_falls_back_then_reconnects_and_refetches_after_ready` fails — an unforeseen dependency on the drop-on-idle behavior exists; report it.
- The Step 4 fake-socket change requires touching more than `tests/event_wait.rs` (e.g. changing the `GatewaySocket` trait): the trait already returns `Result`, so it must not — if it somehow does, stop.
- The assumption "`Ok(None)` from `GatewaySocket::read_text` never indicates a broken socket" turns out false anywhere (check `rg -n "Ok(None)" src/event.rs` call sites).

## Maintenance notes

- Review focus: after this change a socket is only dropped on `Err(GatewayError::Retryable)`/`Fatal`. Confirm in review that server-initiated closes still reconnect — `read_text_until` maps `Message::Close` to `Err(GatewayError::Retryable)` (src/event.rs:521), which is unchanged and covered by Step 4's rewritten test.
- If the gateway ever adds server→client pings with liveness expectations, an idle socket that is silently dead (half-open TCP) now survives until the next interval's fetch/`RefetchNow` path exercises it; the fallback poll bounds staleness at one interval (≤300s), same as polling mode.
- Deferred (rejected as low-value for now): splitting `src/event.rs` (~964 lines) into client/transport modules — see `plans/README.md`.
