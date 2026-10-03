# End-to-end tests

These tests use [tester-army/e2e](https://github.com/tester-army/e2e) 0.16.0 against
babysit's real Rust executable and a local Cloudflare Worker. This project has no
browser UI, so the runner uses an engine-free API target. No model, browser,
GitHub/GitLab login, Cloudflare account, or Keychain enrollment is required.

From the repository root, with the pinned Rust toolchain and Node.js 24:

```bash
corepack enable
pnpm install --frozen-lockfile --ignore-scripts
pnpm test:e2e
```

The workspace disables lifecycle scripts and enforces a 1440-minute dependency
release cooldown. Dependencies are pinned; `ws` supplies authenticated WebSocket
upgrades, which the built-in Node WebSocket client cannot make with custom headers.

`test:e2e` builds the debug CLI, runs every test without retries, and writes
`e2e/.e2e/report.json` and local Worker logs. Those outputs are ignored by Git.
To run a focused test after building:

```bash
cargo build --locked
E2E_TELEMETRY_DISABLED=1 WRANGLER_SEND_METRICS=false \
  WRANGLER_LOG_PATH=.e2e/logs/wrangler.log \
  pnpm --filter @babysit/e2e exec e2e run tests/cli.e2e.ts --grep 'wait --all'
```

The normal command disables e2e telemetry and Wrangler metrics. GitHub Actions and
GitLab CI run lint, formatting, type checking, and the suite, retaining reports for
seven days. TypeScript checks all suite code strictly; `skipLibCheck` applies only
to dependency declarations (including Wrangler's combined Node/Worker types).

## Coverage

| Flow                               | Checks                                                                                                                                                  |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Help, version, argument validation | Every command's help, crate version, invalid flags, zero/overflowing change numbers, exit 4 without a forge call                                        |
| GitHub status                      | Clean/findings/failed/pending exit codes, precedence, merged/closed changes, review requirement and `--no-reviews`, custom bots                         |
| Findings                           | GitHub/GitLab bot filtering, human exclusion, resolved/outdated filtering, `--all`, CodeRabbit `--nitpicks`                                             |
| GitHub review pagination           | Second-page findings and malformed GraphQL responses                                                                                                    |
| GitLab integration                 | Forge auto-detection, current-branch MR lookup, self-hosted host forwarding, status outcomes, job pagination, malformed MR data                         |
| Wait                               | Pending to settled, retryable forge failure, timeout with/without a snapshot, findings output, `--all` on settlement and timeout, GitLab review arrival |
| Webhook setup                      | Create, update, reconciliation, duplicate-hook refusal, protected stdin, secret redaction, invalid input                                                |
| Event-mode validation              | GitLab rejection, invalid gateway schemes/query strings                                                                                                 |
| Token input                        | Enroll/rotate reject multiline input before accessing Keychain                                                                                          |
| Webhook ingress                    | All eight supported events over HTTP, signature/payload validation, unsupported events, POST-only behavior                                              |
| Watcher protocol                   | Authentication, upgrades, registration, malformed/binary frames, re-registration after a new head                                                       |
| Routing and delivery               | PR/head/repository routing, repository isolation, durable deduplication, leading/trailing debounce                                                      |
| Reconnect                          | Ready before retained replay, future-cursor resync                                                                                                      |

Each CLI test gets a temporary directory with scripted `gh`, `glab`, and `git`
executables. Unexpected calls or unconsumed responses fail the test. The Rust
argument parser, subprocess invocation, JSON parsing, polling, rendering, and exit
codes run unchanged. Only external command responses are scripted; no live forge
mutations are possible through these stubs. They receive a minimal environment,
never the developer's authentication variables.

Gateway tests start the actual Worker with Wrangler/workerd, SQLite Durable
Objects, an ephemeral loopback port, and synthetic credentials. They use real
HTTP requests and authenticated WebSocket connections. Each test gets a distinct
repository; sockets, subprocesses, and temporary files are cleaned up. The test
Wrangler config has no deployment routes and does not read `gateway/.dev.vars`.
Round-trip registration barriers check excluded watchers without arbitrary sleeps.

## Boundaries covered by other suites

The existing Rust tests in `tests/credentials.rs`, `tests/event_wait.rs`, and
`tests/wait_loop.rs` cover token enroll/status/rotate/delete, event-assisted
refetching, reconnect/backoff, fatal authorization, cursor handling, and deadline
behavior with injected storage, socket, and clock boundaries. Gateway Vitest tests
cover retention expiry, Durable Object eviction, durable outbox recovery, and
failed-send retries. Run those alongside E2E:

```bash
cargo test --locked --all
pnpm --filter @babysit/gateway test
```

The automated suite does not access the real macOS Keychain, deployed Cloudflare
infrastructure, or live GitHub/GitLab accounts. A complete deployed
`babysit wait --events` smoke still needs those credentials and is documented in
[the gateway README](../gateway/README.md#live-smoke). The E2E suite exercises the
CLI and gateway as separate processes; it does not claim a live CLI-to-gateway
smoke or exhaustive coverage of all possible inputs.
