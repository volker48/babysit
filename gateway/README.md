# babysit gateway

The Worker accepts only GitHub `status` webhooks and wakes authenticated `babysit wait --events`
clients. It does not determine PR state: a wake asks the CLI to refetch GitHub authoritatively.
Each repository's Durable Object persists a monotonic wake cursor, delivery-ID dedupe record, and
compact wake history for six hours, so a reconnect receives `ready` before ordered retained replay or
a `resync` signal. Accepted deliveries atomically record the dedupe ID, allocate the cursor, append
history, and prune both retention sets. Stored wake fields are limited to delivery ID, cursor, event
kind, repository ID, optional PR number, optional head SHA, and receipt time; webhook payloads are
not retained.

A repository uses a fixed two-second leading-and-trailing debounce window: its first accepted wake is
sent immediately, and if more accepted wakes arrive in that window the final wake is sent when the
window closes. Thus a related burst produces at most two socket wakes while still sending one after
its final state change. Every accepted delivery remains in replay history, so reconnect/replay and
fallback polling retain at-least-once wake behavior.

Deploy configuration is intentionally secret-free. Before deploying, set the two bindings:

```bash
cd gateway
pnpm exec wrangler secret put WEBHOOK_SECRET
pnpm exec wrangler secret put WATCHER_TOKEN
```

Point the GitHub webhook at `https://babysit.mindgoblin.pw/webhooks/github` and use a `status`
event. Configure CLI clients with `--gateway-url wss://babysit.mindgoblin.pw/watch`; the CLI
adds the authoritative snapshot repository as encoded path segments.

## Live smoke

The smoke test is for a deployed Worker only. It requires macOS Keychain support, an authenticated
`gh`, an open PR whose checks/reviews keep it unsettled, a locally built `babysit`, and a gateway
base URL ending in `/watch`. It never deploys or creates credentials.

Set secrets only in the invoking environment, preferably by reading your credential manager; do not
place them on a command line or commit them:

```bash
export WATCHER_TOKEN="$(your-secret-manager read babysit/watcher-token)"
export WEBHOOK_SECRET="$(your-secret-manager read babysit/webhook-secret)"
babysit gateway-token enroll
cd gateway && pnpm smoke --repository OWNER/REPO --pr NUMBER --gateway-url wss://HOST/watch
```

`gateway-token enroll` prompts for the same watcher token without echoing it. `BABYSIT_BIN` can
override the default `target/debug/babysit` path. The smoke wraps the real authenticated `gh` with a
temporary counter, starts `babysit wait` with a deliberately nonmatching bot and a 20-second bound,
and requires exactly three `gh pr view` calls: initial, post-ready, and post-wake. It verifies the
count stays at two during a short quiet window before sending the signed status. It cannot validate a
closed PR, a gateway that cannot be reached, or a local Keychain/`gh` setup; it reports failure rather
than claiming a live run succeeded.
