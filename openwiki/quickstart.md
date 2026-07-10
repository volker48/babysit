# babysit OpenWiki quickstart

`babysit` is a Rust command-line tool for watching GitHub pull requests and GitLab merge requests until CI checks and selected automated review bots have settled. Once settled, it reports whether there are unresolved bot findings so a human or coding agent can decide what to fix next.

The canonical user-facing README is `/README.md`; this OpenWiki adds a change-oriented map of the codebase for maintainers and future agents.

## Repository at a glance

- Language/runtime: Rust 2024 edition, pinned to Rust `1.85.1` in `/rust-toolchain.toml`.
- Package: `/Cargo.toml` defines the `babysit` binary/library crate with `regex` and `serde_json` as runtime dependencies.
- Binary entrypoint: `/src/main.rs` forwards process arguments to `babysit::cli::run`.
- Public modules: `/src/lib.rs` exposes `bots`, `cli`, `core`, `forge`, `github`, and `gitlab`.
- External integrations: GitHub uses the authenticated `gh` CLI; GitLab uses the authenticated `glab` CLI.
- CI: `/.gitlab-ci.yml` runs formatting, clippy, tests, and a release build.

Recent git history shows the current code is a compact extracted Rust CLI (`652b9bb Extract babysit CLI`) with follow-up fixes for CodeRabbit/GitLab review handling (`a1c1fa7`, `4204db2`).

## Main commands

From `/README.md` and `/src/cli.rs`:

```bash
babysit status|findings|wait [<pr-or-mr-number>] [options]
```

Important options:

- `-R, --repo <owner/repo>` selects the repository for `gh` or `glab`.
- `--forge <github|gitlab>` overrides auto-detection from `git remote get-url origin`.
- `--bots <csv>` replaces the default bot list.
- `--all` includes resolved and outdated findings.
- `--nitpicks` includes CodeRabbit nitpick review-body findings.
- `--no-reviews` allows settling without a matching bot review.
- `--timeout <secs>` and `--interval <secs>` are valid only with `wait`.
- `--events --gateway-url <wss-url>` opts GitHub waits into gateway wake signals. Polling is the
  default; event mode requires the non-secret `wss://` URL and uses a 300-second fallback unless
  `--interval` is explicit. GitLab events are rejected.
- `gateway-token enroll|status|delete|rotate` manages the gateway bearer token in the macOS
  Keychain; status never prints it.

Default bots are defined in `/src/bots.rs`: `coderabbitai`, `chatgpt-codex-connector`, and `cursor`.

## How the tool decides status

The CLI fetches a normalized `PrSnapshot` from the selected forge provider, then `/src/core.rs` evaluates whether it is settled:

- terminal PR/MR states are settled immediately;
- otherwise all checks must be non-pending;
- a configured bot review must match the current head, or a configured bot check must have landed;
- `--no-reviews` skips the bot-review requirement.

For `status` and `wait`, exit codes are part of the product contract: `0` clean, `1` unresolved findings, `2` failed checks, `3` pending/timed out, and `4` usage or forge CLI errors.

## Documentation map

- [Runtime architecture](architecture/runtime.md) explains the binary/library layout, CLI flow, provider abstraction, settle logic, rendering, and exit codes.
- [Review findings domain](domain/review-findings.md) explains snapshots, checks, bot reviews, findings, bot-specific Markdown distillation, filtering, and nitpicks.
- [Forge integrations](integrations/forges.md) explains how `gh` and `glab` are invoked and how JSON is normalized.
- [Testing and CI](testing.md) explains tests, fixtures, verification commands, and CI gates.

## Setup for development

Prerequisites from `/README.md`:

- Rust 1.85.1 or newer.
- Authenticated `gh` for GitHub repositories.
- Authenticated `glab` for GitLab repositories.

Build and run locally:

```bash
cargo build --release
./target/release/babysit status 123 --repo owner/repo
```

Recommended checks before submitting changes:

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all
cargo build --locked --release
```

Note: during this OpenWiki initialization, `cargo` was not available in the execution environment, so these checks could not be run live.

## Where to start when changing code

- CLI syntax or exit behavior: start in `/src/cli.rs`, then update `/tests/babysit_cli.rs` and README usage if user-visible.
- Settling rules, status output, or findings output: start in `/src/core.rs`, then update `/tests/babysit.rs`.
- Bot parsing or supported bot behavior: start in `/src/bots.rs`, add/update Markdown fixtures in `/tests/fixtures/`, and cover distillation edge cases in `/tests/babysit.rs`.
- GitHub data fetching: start in `/src/github.rs`; check GraphQL pagination and `gh pr view` JSON fields.
- GitLab data fetching: start in `/src/gitlab.rs`; check MR, pipeline jobs, discussions, commit timestamp, and host/project parsing.
- Shared external-command behavior: start in `/src/forge.rs`; preserve timeout, pagination, and retryability semantics.
- Event wake behavior: start in `/src/event.rs` and `/src/wait.rs`; preserve authoritative
  snapshot-only settlement and ready/cursor ordering.
