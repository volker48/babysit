# Plan 004: Stop dead watcher sockets from wedging and reordering wake delivery

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 9b5cac3..HEAD -- gateway/src gateway/test`
> Plan 003 is EXPECTED to have changed `gateway/test/` (it must be DONE first —
> check its row in `plans/README.md`). If `gateway/src/` changed since `9b5cac3`,
> compare the "Current state" excerpts against the live code; on a mismatch,
> treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED (changes delivery/ack semantics of a deployed service)
- **Depends on**: plans/003-gateway-delivery-characterization-tests.md (must be DONE)
- **Category**: bug
- **Planned at**: commit `9b5cac3`, 2026-07-11

## Why this matters

The gateway's Durable Object delivers persisted wake events to WebSocket watchers. Three defects interact badly:

1. **One dead socket poisons delivery for everyone, forever.** `broadcast` throws if *any* matching watcher's `send` throws, so `deliver` defers the cursor and retries every `RETRY_DELAY_MS` (1s). A socket that is dead-but-not-closed (no `webSocketError` handler exists to evict it) makes the cursor undeliverable indefinitely, and each retry **re-sends the same wake frame to every healthy watcher** — duplicate frames plus a 1s-period alarm loop that runs until the 6h retention prunes the event (compute + billable DO wakeups).
2. **Retries deliver out of order.** On failure, only the failed cursor is deferred (`retry_at_ms = now + 1000`); later cursors stay due, and `nextObligation` clamps to `now`, so the alarm refires immediately and delivers the tail *before* the deferred head — violating the cursor-ordered contract the client's `ready_cursor` logic assumes.
3. **No `webSocketError` handler** — only `webSocketClose` (worker.ts:77-79) — so errored hibernatable sockets are never evicted.

The client treats wakes as signals only (it always does an authoritative `gh` fetch), so a wake *skipped* for a dead socket is harmless — that watcher's next fallback poll (≤300s) covers it. This plan makes broadcast best-effort per socket (evict the dead, never throw), adds the missing error handler, and makes `deliver`'s failure path defer the whole remaining tail so order is preserved for genuine send-callback failures.

## Current state

(All line numbers at commit `9b5cac3`.)

- `gateway/src/worker.ts` — `RepositoryGateway` DO:
  ```ts
  // worker.ts:53-55
  async alarm(): Promise<void> {
    await this.history.deliver(Date.now(), (intent) => this.broadcast(intent.cursor, intent.wake));
  }
  ```
  ```ts
  // worker.ts:77-79
  webSocketClose(socket: WebSocket): void {
    socket.close();
  }
  ```
  ```ts
  // worker.ts:98-115
  private broadcast(cursor: number, wake: WakeEvent): void {
    const watchers = activeWatchers(this.ctx.getWebSockets());
    const route = selectWakeRoute(
      wake,
      watchers.map(({ registration }) => registration),
    );
    let failed = false;
    for (const { socket, registration } of watchers) {
      if (!matchesWake(wake, registration, route)) continue;
      try {
        socket.send(frame("wake", cursor));
      } catch {
        failed = true;
        console.warn("failed to deliver wake to watcher");
      }
    }
    if (failed) throw new Error("wake delivery failed for one or more watchers");
  }
  ```
- `gateway/src/replay.ts` — `WakeHistory`:
  ```ts
  // replay.ts:91-107
  async deliver(now: number, send: (intent: WakeIntent) => void): Promise<void> {
    await this.prearm(now);
    this.storage.transactionSync(() => {
      this.materializeOverdue(now);
      this.prune(now);
    });
    for (const intent of this.dueIntents(now)) {
      try {
        send(intent);
      } catch {
        this.defer(intent.cursor, now);
        break;
      }
      this.removeIntent(intent.cursor);
    }
    await this.schedule(now);
  }
  ```
  ```ts
  // replay.ts:405-411
  private defer(cursor: number, now: number): void {
    this.storage.sql.exec(
      "UPDATE wake_outbox SET retry_at_ms = ? WHERE cursor = ?",
      now + RETRY_DELAY_MS,
      cursor,
    );
  }
  ```
  `dueIntents` (lines 290-300) selects `WHERE retry_at_ms <= ? ORDER BY cursor ASC`. `RETRY_DELAY_MS = 1000` (constant near the top of replay.ts). `nextObligation` (307-312) re-arms at `now` while any due row remains.
- Tests affected (after Plan 003):
  - `gateway/test/gateway.test.ts:230-279` — `"continues broadcasting when one matching watcher rejects a frame"`: currently asserts the outbox **retains** cursor 1 after a failed send (`SELECT cursor FROM wake_outbox` → `[{ cursor: 1 }]`) and `console.warn` called once. The retention assertion inverts under this plan.
  - Plan 003's two tests titled `"KNOWN BUG (inverted by plan 004): ..."` — one in each test file; both must be rewritten here.
  - `gateway/test/replay.test.ts:318-331` — `"retains ordered outbox intents after a broadcast failure"`: attempts `[1]` then, at `base + 1_000`, `[1, 1, 2]`. Still valid under the new `defer`-the-tail semantics (both cursors become due again at `base + 1_000` in cursor order) — must keep passing unmodified.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Tests | `pnpm --filter @babysit/gateway test` | all pass |
| Typecheck | `pnpm --filter @babysit/gateway typecheck` | exit 0 |
| Lint / format | `pnpm --filter @babysit/gateway lint` / `format:check` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `gateway/src/worker.ts`
- `gateway/src/replay.ts`
- `gateway/test/gateway.test.ts`
- `gateway/test/replay.test.ts`

**Out of scope** (do NOT touch):
- `gateway/src/wake.ts`, `gateway/src/github.ts` — routing/normalization are unrelated.
- The Rust client (`src/event.rs`) — its protocol expectations are unchanged (cursor-ordered `wake`/`replay`/`resync` frames).
- Schema/migrations in `replay.ts` (constructor, `rebuildOutbox`, etc.).
- The webhook/auth paths in `worker.ts` (`receiveWebhook`, `connectWatcher`, token compare).

## Git workflow

- Branch: `advisor/004-gateway-delivery-hardening`
- Commit style: imperative, ≤72-char subject, e.g. `Evict dead watcher sockets instead of retrying wakes`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Add the missing `webSocketError` handler

In `gateway/src/worker.ts`, next to `webSocketClose` (lines 77-79), add:

```ts
webSocketError(socket: WebSocket): void {
  socket.close();
}
```

(`close()` on an errored hibernatable socket evicts it from `ctx.getWebSockets()`.)

**Verify**: `pnpm --filter @babysit/gateway typecheck` → exit 0.

### Step 2: Make `broadcast` best-effort per socket

In `broadcast` (worker.ts:98-115): when `socket.send` throws, `console.warn` as today, then `socket.close(1011, "wake delivery failed")` to evict the dead socket, and **do not throw** — remove the `failed` flag and the trailing `if (failed) throw ...` line entirely. A completed loop now acks the cursor (`deliver` removes the intent), because a send that throws means the socket is dead and that watcher's fallback poll covers it.

**Verify**: `pnpm --filter @babysit/gateway test` → the two `"KNOWN BUG (inverted by plan 004)"` tests and `"continues broadcasting when one matching watcher rejects a frame"` now FAIL; nothing else fails. (Expected — fixed in Steps 4-5.)

### Step 3: Preserve cursor order in `deliver`'s failure path

`deliver`'s catch path still matters for non-broadcast callers and future send callbacks. In `gateway/src/replay.ts`, change the catch block so a failure defers the failed cursor **and every later still-due cursor**, then breaks. Implement by generalizing `defer`:

```ts
private defer(cursor: number, now: number): void {
  this.storage.sql.exec(
    "UPDATE wake_outbox SET retry_at_ms = ? WHERE cursor >= ? AND retry_at_ms <= ?",
    now + RETRY_DELAY_MS,
    cursor,
    now,
  );
}
```

The `deliver` loop body itself is unchanged (`this.defer(intent.cursor, now); break;`). Effect: after a failure nothing is due until `now + RETRY_DELAY_MS`, `nextObligation` arms the alarm there (not at `now`), and the next cycle walks cursors in order from the failed one.

**Verify**: `pnpm --filter @babysit/gateway test` → `"retains ordered outbox intents after a broadcast failure"` (replay.test.ts:318) still passes; the Plan 003 out-of-order characterization test still fails (rewritten next step).

### Step 4: Rewrite the Plan 003 "KNOWN BUG" tests to assert the fixed behavior

- In `gateway/test/replay.test.ts`, rewrite `"KNOWN BUG (inverted by plan 004): tail intents deliver before a deferred cursor retries"` → rename to e.g. `"defers the whole tail so retries stay cursor-ordered"`. Same setup (accept cursors 1 and 2; first `deliver` throws on cursor 1 → attempts `[1]`). New assertions: `await state.storage.getAlarm()` is `base + 1_000` (NOT `base`); `deliver(base + 1, ...)` attempts nothing (`[1]` unchanged); `deliver(base + 1_000, ...)` with a passing send yields attempts `[1, 1, 2]` — in cursor order.
- In `gateway/test/gateway.test.ts`, rewrite `"KNOWN BUG (inverted by plan 004): retry re-sends the wake to healthy watchers"` → rename to e.g. `"evicts a dead watcher and does not re-send to healthy ones"`. Same setup (two watchers, first one's `send` throws). New assertions: healthy watcher gets exactly one `{ type: "wake", cursor: 1 }`; outbox is **empty** after the POST (cursor acked); the failing socket is gone from `state.getWebSockets()` (length 1); `await state.storage.getAlarm()` reflects only debounce/retention obligations (assert it is NOT `base + 1_000`-style retry — simplest: assert the outbox is empty).

**Verify**: `pnpm --filter @babysit/gateway test` → only `"continues broadcasting when one matching watcher rejects a frame"` still fails.

### Step 5: Update the pre-existing failure test

In `gateway/test/gateway.test.ts:230-279`, `"continues broadcasting when one matching watcher rejects a frame"`: keep the setup, the `console.warn` spy assertion, and the healthy-watcher wake assertion. Change the outbox assertion from `[{ cursor: 1 }]` to `[]` (delivery is acked once every live matching socket got the frame). Also assert the failing socket was evicted (`state.getWebSockets()` length 1 after the POST).

**Verify**: `pnpm --filter @babysit/gateway test` → ALL tests pass.

### Step 6: Full gate

**Verify**: `pnpm --filter @babysit/gateway lint`, `format:check`, `typecheck`, `test` all exit 0. Then run the Rust suite untouched-check: `cargo test --locked --all` → all pass (client protocol unchanged; nothing in `src/` was modified — this is a sanity check only).

## Test plan

- Rewritten tests (Step 4) assert: cursor-ordered retries with tail-deferral; single delivery to healthy watchers; eviction of dead sockets; empty outbox after ack.
- Updated test (Step 5) keeps warn + healthy-delivery coverage under the new ack semantics.
- Regression guards that must pass unmodified: `"retains ordered outbox intents after a broadcast failure"` (replay.test.ts:318), the Plan 003 alarm-delivery test, the Plan 003 migration drop test, and all resume/replay/debounce tests.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `grep -n "wake delivery failed for one or more watchers" gateway/src/worker.ts` → no matches
- [ ] `grep -n "webSocketError" gateway/src/worker.ts` → 1 match
- [ ] `grep -c "KNOWN BUG" gateway/test/*.ts` → 0
- [ ] `pnpm --filter @babysit/gateway lint|format:check|typecheck|test` all exit 0
- [ ] `git status --porcelain` shows only the four in-scope files (and `plans/README.md`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Plan 003's row in `plans/README.md` is not DONE, or its "KNOWN BUG" tests are absent from the test files.
- The `broadcast`/`deliver`/`defer` code does not match the excerpts above.
- After Step 3, `"retains ordered outbox intents after a broadcast failure"` fails — the tail-deferral SQL interacts with that test's timestamps differently than analyzed; report rather than adjusting the test.
- Evicting via `socket.close(...)` inside `broadcast` throws or misbehaves under `@cloudflare/vitest-pool-workers` — report the runtime limitation.
- You find any consumer that relies on `deliver` retrying a cursor that was already sent to some watchers (there is none known — the client dedupes via `ready_cursor`, `handle_message` in `src/event.rs:296-304`).

## Maintenance notes

- **Semantics change to review**: a wake is now acked once every *currently-open matching* socket has been offered the frame; watchers with dead sockets rely on reconnect + fallback poll (≤300s) instead of gateway retries. This matches the documented "wakes are signals only" design, but a reviewer should confirm the operations runbook (`openwiki/operations/gateway.md`) doesn't promise stronger delivery.
- `defer`'s tail-deferral means one persistently failing send callback delays *all* wakes by `RETRY_DELAY_MS` per cycle — acceptable because after Step 2 broadcast no longer throws; the path exists for defense in depth.
- If a future change makes `broadcast` async or per-watcher-targeted, revisit `deliver`'s single-`send`-callback shape.
