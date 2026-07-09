# Testing and CI

The test suite is fixture-heavy and focuses on deterministic parser/domain behavior rather than live calls to GitHub or GitLab.

## Test files

- `/tests/babysit_cli.rs` covers argument parsing, defaults, inline flags, validation errors, wait-only flag restrictions, duration bounds, and forge auto-detection from remote URLs.
- `/tests/babysit.rs` covers core domain behavior, rendering, settle/exit-code rules, bot Markdown distillation, GitHub parsing, GitLab parsing, pagination helpers, and nitpick behavior.
- `/tests/fixtures/` stores representative JSON and Markdown payloads from PR/MR views, bot comments, GitLab discussions/jobs, and review bodies.

Because `/src/lib.rs` publicly exposes the modules, integration tests can call parser and rendering functions directly.

## Verification commands

Use the same commands documented in `/README.md` and enforced by `/.gitlab-ci.yml`:

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all
cargo build --locked --release
```

During this OpenWiki initialization, `cargo` was not installed in the execution environment, so these commands could not be run live.

## CI pipeline

`/.gitlab-ci.yml` uses `rust:1.85.1-bookworm` and defines two stages:

- `verify`: `fmt`, `clippy`, and `test` jobs.
- `build`: `release` job that runs `cargo build --locked --release` and publishes `target/release/babysit` as a 7-day artifact.

The pipeline caches Cargo registry/git data and `target/`, keyed by `Cargo.lock`. Jobs install `rustfmt` and `clippy` in `before_script`.

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
