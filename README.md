# babysit

`babysit` is a Rust CLI for watching pull requests and merge requests until CI
checks and selected bot reviews settle. It supports GitHub through `gh` and
GitLab through `glab`, then distills bot review threads into compact findings for
agents or humans to act on.

## Install

Prerequisites:

- Rust 1.85.1 or newer
- `gh` authenticated for GitHub repositories
- `glab` authenticated for GitLab repositories

Build from source:

```bash
cargo build --release
./target/release/babysit status 123 --repo owner/repo
```

## Usage

```bash
babysit status|findings|wait [<pr-or-mr-number>] [options]
```

Options:

```text
-R, --repo <owner/repo>  Repository to inspect
--forge <github|gitlab>  Default: auto-detect from git origin
--bots <csv>             Bot logins to treat as reviewers
--all                    Include resolved and outdated findings
--nitpicks               Include CodeRabbit nitpick review-body findings
--no-reviews             Settle without waiting for a bot review
--timeout <secs>         wait only; default 1800
--interval <secs>        wait only; default 30
```

Default bots are `coderabbitai`, `chatgpt-codex-connector`, and `cursor`.

## Examples

Watch a GitHub PR until checks and CodeRabbit settle:

```bash
babysit wait 63 --repo volker48/agent-customization --bots coderabbitai
```

List unresolved findings from a GitLab merge request:

```bash
babysit findings 42 --repo group/project --forge gitlab
```

Print status without requiring a bot review:

```bash
babysit status 63 --repo volker48/agent-customization --no-reviews
```

## Exit codes

For `status` and `wait`:

| Code | Meaning |
| ---- | ------- |
| 0 | Settled cleanly |
| 1 | Settled with unresolved findings |
| 2 | Settled with failed checks |
| 3 | Pending or timed out |
| 4 | Usage or forge CLI error |

`findings` exits `0` when it can list findings successfully.

## Development

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all
cargo build --locked --release
```

GitLab CI runs the same format, lint, test, and release-build gates on branches
and merge requests.
