# Testing and CI

The test suite is fixture-heavy and focuses on deterministic parser/domain behavior rather than live calls to GitHub or GitLab.

## Test files

- `/tests/babysit_cli.rs` covers argument parsing, defaults, inline flags, validation errors,
  wait-only flag restrictions, duration bounds, and forge auto-detection from remote URLs.
- `/tests/wait_loop.rs` covers polling cadence, deadline behavior, and immediate authoritative
  refetch requests from a wake source.
- `/tests/credentials.rs` covers token-store behavior with an in-memory fake.
- `/tests/event_wait.rs` uses scripted WebSocket and token-store boundaries to cover ready/cursor,
  wake/replay, re-registration, malformed configuration, and fatal authorization behavior.
- `/tests/babysit.rs` covers core domain behavior, rendering, settle/exit-code rules, bot Markdown distillation, GitHub parsing, GitLab parsing, pagination helpers, and nitpick behavior.
- `/tests/fixtures/` stores representative JSON and Markdown payloads from PR/MR views, bot comments, GitLab discussions/jobs, and review bodies.

Because `/src/lib.rs` publicly exposes the modules, integration tests can call parser and rendering functions directly.

## Verification commands

Use the same Rust commands documented in `/README.md` and enforced by `/.gitlab-ci.yml`:

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all
cargo build --locked --release
```

The same pipeline also verifies the Cloudflare gateway package:

```bash
pnpm --filter @babysit/gateway lint
pnpm --filter @babysit/gateway format:check
pnpm --filter @babysit/gateway typecheck
pnpm --filter @babysit/gateway test
```

Install the committed pnpm workspace lockfile before running the gateway commands. The gateway
Vitest suites cover signed webhook ingress, authentication, hibernatable WebSocket registration,
ready/cursor replay and resync, compact retention, deduplication, debounce, and durable outbox
retries. The deployed smoke is separate from these deterministic checks; see
[Gateway operations](operations/gateway.md) for its protected-credential prerequisites and the
historical #8 tracer evidence.

## CI pipeline

`/.gitlab-ci.yml` uses `rust:1.85.1-bookworm` and defines two stages:

- `verify`: Rust `fmt`, `clippy`, and `test` jobs plus gateway lint, format, typecheck, and Vitest jobs.
- `build`: `release` job that runs `cargo build --locked --release` and publishes `target/release/babysit` as a 7-day artifact.

The pipeline caches Cargo registry/git data and `target/`, keyed by `Cargo.lock`. Rust jobs install
`rustfmt` and `clippy` in `before_script`. Gateway jobs install the frozen pnpm lockfile with
lifecycle scripts disabled, then run `gateway_lint`, `gateway_format`, `gateway_typecheck`, and
`gateway_test`.

## Event and credential boundaries

Tests never connect to a live gateway, GitHub, Keychain, or clock. They script only the
network/credential boundaries and exercise `wait_until_settled`, so event frame data cannot become
an authority for settlement. The cfg-gated Keychain adapter can be checked manually on a macOS
host without accessing a real Keychain:

```bash
rustup target add x86_64-apple-darwin
cargo check --locked --all-targets --target x86_64-apple-darwin
```

This Apple target check is not part of the Linux CI pipeline; it requires a macOS host and its
native toolchain. Live webhook/gateway validation is an operational smoke, not a default test; see
[Gateway operations](operations/gateway.md).

## Fixture strategy

When changing parsing or bot distillation:

1. Add or update the smallest fixture under `/tests/fixtures/` that captures the real payload shape.
2. Add focused assertions in `/tests/babysit.rs` for the normalized result, not the entire raw payload.
3. Keep tests independent of authenticated `gh`/`glab`; live service behavior belongs in manual smoke tests, not the default suite.

Existing fixtures include CodeRabbit, Codex, and Cursor Bugbot inline/review-body Markdown, GitHub `pr-view.json`, GitLab discussions/jobs, and open/merged MR JSON.

## High-risk areas to test carefully

- Exit-code precedence in `/src/core.rs`: unsettled returns `3`, failed checks return `2`, unresolved findings return `1`, clean settled returns `0`.
- Review/head matching: direct commit OID matches first, then timestamp fallback.
- Resolved/outdated filtering: default output excludes both; `--all` includes both with flags.
- CodeRabbit nitpicks: `--nitpicks` should include current-head review-body nitpicks without reviving stale ones.
- Pagination: GitHub GraphQL review threads and GitLab API array pages should stop correctly and fail safely if page shape is invalid.
