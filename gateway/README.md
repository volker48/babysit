# babysit gateway

The Worker accepts signed GitHub `check_run`, `check_suite`, `status`, `pull_request`,
`pull_request_review`, `pull_request_review_comment`, `pull_request_review_thread`, and
`issue_comment` webhooks and wakes authenticated `babysit wait --events` clients. It does not
determine PR state: a wake asks the CLI to refetch GitHub authoritatively. Each repository's
Durable Object persists a monotonic wake cursor and compact wake history for six hours, so a
reconnect receives `ready` before ordered retained replay or a `resync` signal. Retained delivery
IDs have a unique index, so a duplicate is acknowledged without another cursor or logical wake.

Wake metadata contains only the delivery ID, cursor, event kind, repository identifiers, optional PR
number, optional head SHA, and receipt time. A durable outbox holds only undelivered operational
work. An intent is deleted immediately after a successful broadcast; failed sends remain retryable
and are capped and pruned with the six-hour retention window. The broker uses a fixed, non-sliding
two-second burst window per canonical route (PR, then head SHA, then repository): one event creates
one leading logical wake; two or more related events create exactly a leading and a trailing logical
wake. Physical sends can be duplicated after a crash or send failure, and Cloudflare can run an
alarm late, but late/retried alarms drain retained work in cursor order rather than lose it.

## Deploy and operate

The canonical manual deployment, webhook, Keychain, rotation, privacy, and troubleshooting runbook
is [Gateway operations](../openwiki/operations/gateway.md). It names the two required Worker
secrets (`WEBHOOK_SECRET` and `WATCHER_TOKEN`) without storing their values, the fixed GitHub
webhook endpoint, and the CLI base URL. The CLI starts from `wss://babysit.mindgoblin.pw/watch` and
adds encoded repository path segments from its authoritative snapshot.

## Live smoke

The smoke is for an already deployed Worker only; it never deploys or creates credentials. It
requires macOS Keychain enrollment of the matching watcher token, authenticated `gh`, an open PR
whose checks/reviews remain unsettled, a locally built `babysit`, and a gateway base URL ending in
`/watch`. The script itself requires protected `WATCHER_TOKEN` and `WEBHOOK_SECRET` process
variables to check the client and sign the test delivery. Use only a credential manager's
protected process injection; do not hand-export values, put them in arguments, or log them.

```bash
pnpm --filter @babysit/gateway smoke -- --repository OWNER/REPOSITORY --pr NUMBER \
  --gateway-url wss://babysit.mindgoblin.pw/watch
```

`BABYSIT_BIN` can override the default `target/debug/babysit` path. The smoke wraps the real
authenticated `gh` with a temporary counter, starts `babysit wait` with a deliberately nonmatching
bot and a 20-second bound, and requires exactly three `gh pr view` calls: initial, post-ready, and
post-wake. It verifies the count stays at two during a short quiet window before sending the signed
status. It cannot validate a closed PR, an unreachable gateway, or a missing Keychain/`gh` setup; it
reports failure rather than claiming a live run succeeded.
