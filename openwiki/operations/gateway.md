# Gateway operations

This runbook covers the manual MVP for the same-repository Cloudflare gateway. It does not create a
GitHub webhook or Cloudflare credentials automatically; automated webhook setup is intentionally
out of scope.

## Fixed endpoints and prerequisites

The deployed custom domain in `gateway/wrangler.toml` is **`babysit.mindgoblin.pw`**. Use these
fixed endpoints:

| Purpose | Value |
| --- | --- |
| GitHub webhook payload URL | `https://babysit.mindgoblin.pw/webhooks/github` |
| CLI gateway base URL | `wss://babysit.mindgoblin.pw/watch` |

The Cloudflare account must manage the `mindgoblin.pw` zone and be authorized to deploy the Worker
and its Durable Object. The event client also needs an authenticated `gh`, a macOS Keychain, and a
built `babysit` binary. The gateway is GitHub-only; GitLab cannot use `--events`.

The CLI accepts only the base URL shown above. It derives and percent-encodes the repository-specific
`/watch/OWNER/REPOSITORY` path from the authoritative GitHub snapshot. Do not put a repository,
query, fragment, or credential in `--gateway-url`.

## Deploy the Worker and configure secrets

Install the committed gateway toolchain from the repository root, then authenticate Wrangler to the
Cloudflare account that manages the custom domain:

```bash
corepack enable
pnpm config set minimumReleaseAge 1440
pnpm config set ignore-scripts true
pnpm install --frozen-lockfile --ignore-scripts
cd gateway
```

The Worker has exactly two required secrets:

| Binding | Used for | Must match |
| --- | --- | --- |
| `WEBHOOK_SECRET` | GitHub `X-Hub-Signature-256` verification | The webhook's configured secret |
| `WATCHER_TOKEN` | Bearer authentication for event-mode WebSocket clients | The token enrolled in each client's Keychain |

Set each binding through Wrangler's interactive secret prompt, then deploy:

```bash
pnpm exec wrangler secret put WEBHOOK_SECRET
pnpm exec wrangler secret put WATCHER_TOKEN
pnpm exec wrangler deploy
```

Never place either value in `wrangler.toml`, source control, command arguments, shell history,
`.env` files, or ordinary logs. Use a credential manager to generate and retrieve values. The
repository configuration intentionally contains only binding names, not secret values.

## Create the GitHub webhook manually

In the repository's **Settings → Webhooks**, add an active webhook with these exact settings:

| Setting | Value |
| --- | --- |
| Payload URL | `https://babysit.mindgoblin.pw/webhooks/github` |
| Content type | `application/json` |
| Secret | The same protected value configured as `WEBHOOK_SECRET` |
| Event selection | **Let me select individual events** |

Select exactly these events:

- `check_run`
- `check_suite`
- `status`
- `pull_request`
- `pull_request_review`
- `pull_request_review_comment`
- `pull_request_review_thread`
- `issue_comment`

The Worker validates the SHA-256 signature before it parses a supported payload. A valid supported
delivery is accepted with HTTP 202. Unsupported signed events are acknowledged but do not wake a
waiter. Do not automate this setup or add webhook-provisioning commands here.

## Enroll the local bearer token

On macOS, enroll the `WATCHER_TOKEN` value without displaying it:

```bash
babysit gateway-token enroll
babysit gateway-token status
```

`enroll` reads a no-echo terminal prompt. For protected automation, supply only a credential
manager's protected standard output to its standard input; `babysit` does not read this token from
environment variables or files. `status` reports only whether a token is configured, never its
value.

The Keychain entry is deliberately fixed to service **`babysit`** and account
**`gateway-bearer-token`**. Manage it with:

```bash
babysit gateway-token delete
babysit gateway-token rotate
```

`delete` removes the entry. `rotate` replaces it using the same no-echo prompt or protected stdin
flow. A missing Keychain token is a fatal event-mode configuration error, not a fallback to an
environment variable or file.

## Choose polling or event-assisted waits

Polling remains the default and needs neither a gateway nor a Keychain token:

```bash
babysit wait 63 --repo OWNER/REPOSITORY
```

Use event-assisted mode only after the gateway and token are configured:

```bash
babysit wait 63 --repo OWNER/REPOSITORY --events \
  --gateway-url wss://babysit.mindgoblin.pw/watch
```

A wake is not PR state. Both modes settle only after an authoritative GitHub snapshot fetch. Normal
polling defaults to a 30-second interval. In event mode, omitting `--interval` uses a 300-second
fallback poll; an explicit `--interval SECS` replaces that 300-second fallback. The overall
`--timeout SECS` remains the deadline for initial fetches, reconnects, sleeps, and fallback polls. A
fetch that began before the deadline may still complete after it: a settled snapshot is accepted, an
unsettled snapshot is retained for timeout reporting, and no late event observation or refetch is
started once no time remains.

## How the event protocol preserves correctness

Each GitHub repository has a repository-scoped Durable Object with hibernatable WebSocket watchers.
After the initial authoritative fetch, the CLI registers its GitHub host, repository, PR number,
current head SHA, and last cursor. The Durable Object responds with `ready` and its current cursor,
then sends retained ordered replay frames after the requested cursor or a `resync` frame when that
cursor is unknown or expired.

The CLI immediately performs another authoritative fetch after `ready`; only wake or replay cursors
newer than `ready` are actionable. A `resync` also causes an immediate authoritative fetch. Whenever
a fetch observes a different head SHA, the CLI re-registers and repeats the ready-then-refetch
ordering. This applies again after reconnects, preventing a connect-time race from hiding a change.

Wake history and delivery-ID deduplication are retained for six hours. The durable outbox holds only
undelivered operational work: successful broadcast intents are deleted immediately, while failed
sends remain retryable and are capped and pruned with that six-hour window. Delivery is at-least-once:
duplicate physical wake frames can cause an extra snapshot fetch, while replay, resync, and fallback
polls protect against a missed frame. Related bursts are debounced, but the leading and trailing
logical wakes are retained.

### Stored data and privacy

The gateway retains compact routing metadata only: delivery ID, cursor, event kind, repository ID,
optional PR number, optional head SHA, and receipt time. It does not retain complete webhook
payloads, comment or review bodies, snippets, logs, GitHub credentials, prompts, or the watcher
bearer token. Keep diagnostic logging to the same boundary; do not paste webhook bodies or tokens
into tickets or logs.

## Disconnect and fallback behavior

Malformed gateway configuration or protocol frames, missing local credentials, and gateway HTTP 401
or 403 authorization responses are fatal configuration/authorization errors. Correct the problem;
the CLI does not silently turn those errors into polling.

Transport failures, connection and read timeouts, disconnects, HTTP 429, and HTTP 5xx responses are
retryable. A read timeout releases the established socket before the fallback fetch, so the next
snapshot reconnects, registers, receives `ready`, and performs its post-ready fetch. The CLI
continues fallback polling while it reconnects with randomized exponential delays: a maximum of 1
second, then 2, 4, 8, 16, and 30 seconds thereafter. A successful registration resets the delay.
Every successful authoritative fetch resets the event-mode fallback timer, including the initial,
post-ready, wake, replay, resync, and fallback fetches. `--timeout` overrides the entire process even
while it is reconnecting.

## Routine checks and troubleshooting

| Symptom | Check and safe response |
| --- | --- |
| GitHub reports a failed delivery | Confirm the payload URL, `application/json`, selected event, and that the GitHub secret matches `WEBHOOK_SECRET`. A 401 indicates a bad/missing signature; 400 means an invalid supported payload; 503 means a required Worker secret is absent. Inspect the delivery response without copying its body into logs. |
| A delivery returns 202 but the waiter does not react | First confirm the event is one of the [supported webhook events](#create-the-github-webhook-manually): `check_run`, `check_suite`, `status`, `pull_request`, `pull_request_review`, `pull_request_review_comment`, `pull_request_review_thread`, or `issue_comment`. Unsupported signed events intentionally return 202 without waking. Then confirm `babysit gateway-token status`, the exact `wss://babysit.mindgoblin.pw/watch` base URL, and the GitHub repository/PR used by the wait. A delivery is only a wake; inspect the subsequent authoritative `gh pr view` fetch and `gh` authentication. |
| Event wait exits with authorization or configuration error | Treat 401/403, malformed URL/protocol, or missing Keychain token as fatal. Correct the token/URL, run `babysit gateway-token status`, then start a new wait. |
| The WebSocket disconnects or the gateway is temporarily unavailable | This is retryable. The client reconnects with bounded jitter while fallback polling continues; wait for the next authoritative fetch or use polling mode if immediate manual control is needed. |
| A change appears to have been missed | Start a fresh event wait or use polling. Reconnect registration requests replay; an unavailable six-hour cursor produces `resync`, followed by an authoritative refetch. |

## Rotation and deployed tracer evidence

There is one active shared `WATCHER_TOKEN` binding, so rotation is a coordinated cutover rather than
a zero-downtime overlap:

1. Generate replacement values in the credential manager and retain the old values only for a
   controlled rollback.
2. Stop active event-assisted waits; use polling-only `babysit wait` during the cutover.
3. Update `WATCHER_TOKEN` with `pnpm exec wrangler secret put WATCHER_TOKEN` through its prompt.
4. On every affected macOS client, run `babysit gateway-token rotate` through its no-echo prompt or
   protected stdin, verify `babysit gateway-token status`, then restart event waits.
5. Verify a new authenticated wait before retiring the old credential from the manager.

Rotate `WEBHOOK_SECRET` as a paired, short maintenance operation: update its Worker binding with
`pnpm exec wrangler secret put WEBHOOK_SECRET`, immediately update the GitHub webhook secret to the
same protected value, and inspect the GitHub delivery list. With one signature secret there is no
server-side dual-secret overlap; redeliver any delivery that failed during the change, then retire
the old value after a successful signed 202 response.

Issue [#8](https://github.com/volker48/babysit/issues/8) owns the first deployment tracer, not this
runbook. Its merged PR [#16](https://github.com/volker48/babysit/pull/16) records the 2026-07-10
live evidence for `babysit.mindgoblin.pw`: a signed `status` webhook reached an authenticated
hibernatable Durable Object watcher, and the smoke observed distinct initial, post-ready, and
post-wake authoritative `gh pr view` fetches. Re-run the smoke after material gateway changes; it
does not deploy the Worker or create credentials.

The smoke script requires `WATCHER_TOKEN` and `WEBHOOK_SECRET` in its own process because it signs a
real test delivery and checks the enrolled watcher. Do not hand-export values or put them in a
command line. Invoke it only through your credential manager's protected process/environment
injection mechanism, then run:

```bash
pnpm --filter @babysit/gateway smoke -- --repository OWNER/REPOSITORY --pr NUMBER \
  --gateway-url wss://babysit.mindgoblin.pw/watch
```

It requires an open, unsettled PR, authenticated `gh`, macOS Keychain enrollment of the matching
watcher token, and a locally built binary (or `BABYSIT_BIN`). The smoke expects exactly three
authoritative `gh pr view` calls: initial, post-ready, and post-wake.
