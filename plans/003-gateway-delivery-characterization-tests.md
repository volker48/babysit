# Plan 003: Pin gateway alarm delivery, migration, and failure behavior with tests

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 9b5cac3..HEAD -- gateway/src gateway/test`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW (tests only — zero production-code changes)
- **Depends on**: none. **Blocks Plan 004** (the delivery-hardening fix), which will deliberately invert two of these tests.
- **Category**: tests
- **Planned at**: commit `9b5cac3`, 2026-07-11

## Why this matters

The Cloudflare Worker gateway (`gateway/`) persists webhook wakes in a Durable Object and delivers them to WebSocket watchers via an outbox + alarm retry loop. Three load-bearing behaviors have **zero test coverage today**: (a) the alarm handler end-to-end (no test in `gateway/test/` ever invokes `alarm()` — `grep -c alarm gateway/test/*.ts` finds no gateway-level use), (b) the schema-migration branch that silently **drops** legacy outbox rows, and (c) what actually happens across multiple delivery cycles when a watcher socket persistently fails. Plan 004 rewrites the failure-path code; without these tests first, that rewrite has no safety net and two known bugs (out-of-order retry delivery, duplicate frames to healthy watchers) stay invisible. This plan adds the tests, all passing against current behavior; tests that pin *buggy* behavior are labeled so Plan 004 can invert them knowingly.

## Current state

Production code (context only — this plan must NOT modify it):

- `gateway/src/worker.ts` — Worker front door + `RepositoryGateway` DO.
  - `alarm()` (lines 53-55): `await this.history.deliver(Date.now(), (intent) => this.broadcast(intent.cursor, intent.wake));`
  - `broadcast` (lines 98-115): sends `frame("wake", cursor)` to every matching watcher; on any send throw sets `failed = true`, warns `"failed to deliver wake to watcher"`, and after the loop `if (failed) throw new Error("wake delivery failed for one or more watchers");` — so one dead socket makes the whole cursor count as undelivered, and retries re-send to healthy watchers too.
- `gateway/src/replay.ts` — `WakeHistory` (SQLite in DO storage).
  - `deliver` (lines 91-107): prearm → prune → for each `dueIntents(now)` (ordered `cursor ASC`, `retry_at_ms <= now` — lines 290-300): `send(intent)`; on throw `this.defer(intent.cursor, now); break;` else `removeIntent(cursor)`; finally `schedule(now)`.
  - `defer` (lines 405-411): `UPDATE wake_outbox SET retry_at_ms = now + RETRY_DELAY_MS WHERE cursor = ?` — only the failed cursor is pushed out; later untried cursors keep `retry_at_ms <= now`.
  - `nextObligation` (lines 307-312): `Math.max(now, Math.min(nextRetry, nextDeadline, nextExpiry))` — with an untried due cursor, the alarm re-arms at `now`, so the tail delivers *before* the deferred cursor retries → **out-of-order delivery (known bug, fixed in Plan 004)**.
  - `rebuildOutbox` (lines 197-208): renames `wake_outbox` → `wake_outbox_legacy`, recreates tables, copies rows **only** `if (OUTBOX_COLUMNS.every((column) => columns.includes(column)))`, then `DROP TABLE wake_outbox_legacy` — a legacy table missing any current column (e.g. `repository_id`) is dropped without copying. Untested branch.
  - Constants exported: `WAKE_RETENTION_MS` (6h), `DEBOUNCE_WINDOW_MS` (2000), and `RETRY_DELAY_MS` is `1000` (check whether it is exported; if not, use the literal `1_000` in tests).

Test conventions to copy:

- `gateway/test/replay.test.ts` — direct `WakeHistory` tests. Harness (lines 1-41): `withHistory(repository, (history, state) => ...)` wraps `runInDurableObject`; `wake(cursor, options)` builds a `WakeEvent` with `deliveryId: "delivery-<n>"`; `base = Date.now() + 60_000`; `tableColumns(state, table)` reads `PRAGMA table_info`. Existing migration test to model after — "migrates intermediate outbox rows without repository names" (lines 113-152): DROPs `wake_outbox`, recreates a legacy shape **with** all current columns plus `repository_full_name`, inserts a row, `await evictDurableObject(stub(repository))`, re-enters and asserts columns + that `deliver` still sends the copied intent.
  Existing failure test (lines 318-331) "retains ordered outbox intents after a broadcast failure": accept cursors 1,2 → `deliver(base, throw)` → attempts `[1]` → `deliver(base + 1_000, push)` → attempts `[1, 1, 2]`.
- `gateway/test/gateway.test.ts` — worker-level tests via `cloudflare:test`. Harness (lines 1-70): `signedWebhook(...)` builds an HMAC-signed request (test secret literal `"webhook-test-secret"`); `watcher(repository)` opens an authenticated WebSocket (Bearer `"watcher-test-token"`, expects 101); `nextMessage(socket)` awaits one parsed frame; `register(socket, repository, headOid, after, number)` sends the registration frame. Existing failure test to model after — "continues broadcasting when one matching watcher rejects a frame" (lines 230-279): two watchers, `runInDurableObject(stub, async (instance, state) => ...)`, finds the target socket via `state.getWebSockets()` + `deserializeAttachment()`, overrides its `send` with `Object.defineProperty(failingSocket, "send", { value: () => { throw ... } })`, POSTs a wake to `instance.fetch(new Request("https://repository-gateway/wake", ...))`, asserts outbox contents via `state.storage.sql.exec("SELECT cursor FROM wake_outbox")`.

Verified baseline: `pnpm --filter @babysit/gateway test` → 63 tests pass.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Install (if needed) | `pnpm install --frozen-lockfile --ignore-scripts` | exit 0 |
| Tests | `pnpm --filter @babysit/gateway test` | all pass (63 + new) |
| Typecheck | `pnpm --filter @babysit/gateway typecheck` | exit 0 |
| Lint | `pnpm --filter @babysit/gateway lint` | exit 0 |
| Format | `pnpm --filter @babysit/gateway format:check` | exit 0 (`pnpm --filter @babysit/gateway format` to fix) |

## Scope

**In scope** (the only files you should modify):
- `gateway/test/replay.test.ts`
- `gateway/test/gateway.test.ts`

**Out of scope** (do NOT touch):
- `gateway/src/**` — this plan changes NO production behavior. If a test you write fails, the test is wrong (or you hit a STOP condition); never "fix" the source.
- `gateway/scripts/smoke.ts`, wrangler config.

## Git workflow

- Branch: `advisor/003-gateway-delivery-tests`
- Commit style: imperative, ≤72-char subject, e.g. `Pin gateway alarm and failure delivery behavior`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Alarm-driven trailing-debounce delivery (worker level)

In `gateway/test/gateway.test.ts`, add a test `"delivers the trailing debounced wake when the alarm fires"`:

1. Open one `watcher(repository)` and `register` it (await its ready frame), using a fresh repository name (e.g. `"alarm-delivery/repo"` — each test must use a unique repo so DOs don't leak state).
2. Inside `runInDurableObject(stub, async (instance, state) => ...)`: POST two wakes for the same PR route in quick succession (same `repository`, same shape; distinct `deliveryId`s) via `instance.fetch(new Request("https://repository-gateway/wake", ...))`. The first is the leading wake (delivered immediately); the second lands inside the 2s debounce window and becomes the pending trailing wake.
3. Assert `await state.storage.getAlarm()` is a number (the debounce deadline).
4. Advance past the window by invoking the handler directly — `await instance.alarm()` — after the debounce deadline. Since `alarm()` calls `Date.now()` internally, first assert the alarm timestamp, then use `vi.useFakeTimers({ now: <deadline> })`/`vi.setSystemTime` **or** simply wait: prefer calling `await instance.alarm()` after stubbing `Date.now` is NOT needed if you instead drive `WakeHistory.deliver` at worker level is awkward — simplest reliable approach: `vi.spyOn(Date, "now").mockReturnValue(deadline + 1)` before `await instance.alarm()`, then restore.
5. Assert the watcher received wake frames for cursor 1 and then the trailing cursor 2 (`{ type: "wake", version: 1, cursor: 2 }`).

**Verify**: `pnpm --filter @babysit/gateway test` → all pass including the new test.

### Step 2: Migration drop-branch characterization (history level)

In `gateway/test/replay.test.ts`, add `"drops legacy outbox rows missing current columns during rebuild"`, modeled directly on the lines 113-152 test:

1. `withHistory`: DROP `wake_outbox`, CREATE it **without** the `repository_id` column (keep the others), INSERT one row matching that shape.
2. `await evictDurableObject(stub(repository))`.
3. Re-enter `withHistory`: assert `tableColumns(state, "wake_outbox")` equals the current 8-column list (copy from the existing test), assert the table is empty (`state.storage.sql.exec("SELECT * FROM wake_outbox").toArray()` → `[]`), and assert `deliver` sends nothing.
4. Add a comment on the test: `// Characterizes current behavior: rows from unrecognized legacy shapes are dropped, not migrated.`

**Verify**: `pnpm --filter @babysit/gateway test` → all pass.

### Step 3: Out-of-order retry delivery characterization (history level)

In `gateway/test/replay.test.ts`, add `"KNOWN BUG (inverted by plan 004): tail intents deliver before a deferred cursor retries"`:

1. `withHistory`: accept `wake(1, { changeNumber: 1 })` and `wake(2, { changeNumber: 2 })` at `base` (distinct routes → both materialize immediately, matching the lines 318-331 test).
2. `deliver(base, ...)` with a send that throws **only** for cursor 1 and records attempts. Expected attempts: `[1]` (the `break` stops the cycle).
3. Assert `await state.storage.getAlarm()` equals `base` (re-armed at `now` because cursor 2 is still due — this is the bug's signature).
4. `deliver(base + 1, ...)` with a recording, non-throwing send → attempts become `[1, 2]` while cursor 1's retry is still parked at `base + 1_000` → cursor 2 delivered before cursor 1.
5. Comment block on the test explaining Plan 004 will invert it (deferred failure must also defer the tail to preserve cursor order).

**Verify**: `pnpm --filter @babysit/gateway test` → all pass.

### Step 4: Duplicate frames to healthy watchers on retry (worker level)

In `gateway/test/gateway.test.ts`, add `"KNOWN BUG (inverted by plan 004): retry re-sends the wake to healthy watchers"`, modeled on the lines 230-279 test:

1. Two watchers on a fresh repository; make the first one's `send` always throw (the `Object.defineProperty` pattern); spy on `console.warn` as the existing test does.
2. POST one wake inside `runInDurableObject`. Assert the healthy watcher got `{ type: "wake", cursor: 1 }` and the outbox still contains cursor 1 (broadcast threw → intent deferred).
3. Mock `Date.now` forward by `1_001` ms (past `RETRY_DELAY_MS`) and `await instance.alarm()`; restore the mock.
4. Assert the healthy watcher receives a **second** `{ type: "wake", cursor: 1 }` frame — the duplicate that Plan 004 eliminates. Assert the outbox STILL contains cursor 1 (the failing socket keeps the cursor undeliverable forever).
5. Comment the test as characterizing the amplification bug.

**Verify**: `pnpm --filter @babysit/gateway test` → all pass.

### Step 5: Full gate

**Verify**: `pnpm --filter @babysit/gateway lint`, `format:check`, `typecheck`, `test` all exit 0. `git diff --stat` touches only the two test files.

## Test plan

This plan IS the test plan: 4 new tests (Steps 1-4), each modeled on a named existing test, all passing against `9b5cac3` behavior. The two "KNOWN BUG" tests are intentionally assertions of wrong behavior with comments pointing at Plan 004.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `pnpm --filter @babysit/gateway test` exits 0 with ≥67 tests (63 baseline + 4 new)
- [ ] `grep -c "KNOWN BUG (inverted by plan 004)" gateway/test/*.ts` → 2
- [ ] `grep -rn "instance.alarm()" gateway/test/` → at least 2 matches (alarm handler finally exercised)
- [ ] `git status --porcelain` shows only the two test files (and `plans/README.md`)
- [ ] `pnpm --filter @babysit/gateway lint|format:check|typecheck` all exit 0
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Any new test fails in a way that contradicts the "Current state" description (e.g. the alarm after a deferred failure is NOT `base`, or the drop-branch actually copies rows) — the diagnosis feeding Plan 004 would be wrong, and Plan 004 must be re-planned before landing.
- You cannot invoke `instance.alarm()` or mock `Date.now` under `@cloudflare/vitest-pool-workers` — report the limitation rather than restructuring production code to compensate.
- A pre-existing test starts failing.

## Maintenance notes

- The two "KNOWN BUG" tests are scaffolding for Plan 004; whoever executes Plan 004 must rewrite them to assert the corrected behavior (ordered retries, no duplicate frames) — they must not survive as-is after 004 lands.
- The alarm test (Step 1) is the first end-to-end coverage of the DO alarm path; keep it green in any future `deliver`/`schedule` refactor.
- Deferred: a test for `dedupeExistingHistory`'s constructor cost (unguarded full-table scans) — a perf concern, rejected as low-impact in `plans/README.md`.
