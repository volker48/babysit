<p>
<img src="https://raw.githubusercontent.com/volker48/babysit/main/banner.png" alt="babysit" width="1100">
</p>

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

## Add the companion skill

I would add this globally so its available in all your repos. Feel free to adapt the skill to
how you want to work.

```bash
npx skills add volker48/babysit
```

## Usage

```bash
babysit status|findings|wait [<pr-or-mr-number>] [options]
babysit gateway-token <enroll|status|delete|rotate>
```

Options:

```text
-R, --repo <owner/repo>  Repository to inspect
--forge <github|gitlab>  Default: auto-detect from git origin
--bots <csv>             Bot logins to treat as reviewers
--all                    Include resolved and outdated findings
--nitpicks               Include CodeRabbit nitpick review-body findings
--no-reviews             Settle without waiting for a bot review
--timeout <secs>         wait only; overall deadline, default 1800
--interval <secs>        wait only; default 30 (event fallback default 300)
--events                 wait only; opt in to GitHub event-assisted wakes
--gateway-url <wss-url>  required with --events; exact non-secret wss://host/watch base URL
```

Default bots are `coderabbitai`, `chatgpt-codex-connector`, and `cursor`.

## Examples

Watch a GitHub PR until checks and CodeRabbit settle:

```bash
babysit wait 63 --repo example-org/example-repo --bots coderabbitai
```

List unresolved findings from a GitLab merge request:

```bash
babysit findings 42 --repo group/project --forge gitlab
```

Print status without requiring a bot review:

```bash
babysit status 63 --repo example-org/example-repo --no-reviews
```

### Event-assisted waits

Polling is the default. To opt in to GitHub event-assisted wake signals, store an
operator-provided gateway bearer token in the macOS Keychain, then provide the non-secret gateway
URL:

```bash
babysit gateway-token enroll
babysit wait 63 --repo example-org/example-repo --events \
  --gateway-url wss://gateway.example/watch
```

Enrollment prompts for the token without echoing it. For protected automation, provide it through
piped stdin from a credential manager; do not put it in an environment variable, command argument,
or logs. `gateway-token rotate` also accepts protected stdin or a no-echo terminal prompt. `status`
reports only whether it is configured; `delete` removes it. Tokens use Keychain service `babysit`
and account `gateway-bearer-token`, are never read from environment variables or files, and macOS
Keychain support is required for event mode.

The gateway URL is required and must be exactly the `wss://host/watch` base URL, with no repository,
query, fragment, or extra path. babysit appends percent-encoded owner and repository path segments
from its authoritative snapshot. GitLab event mode is unavailable. An event is only a wake signal:
babysit performs an authoritative GitHub fetch after gateway ready,
wake, replay, resync, and fallback ticks. Without an explicit `--interval`, event mode uses a
300-second fallback poll; an explicit interval wins. This client does not provision a gateway,
webhook, Worker, or token server-side. For the manual Cloudflare deployment, webhook, Keychain,
rotation, and troubleshooting procedure, see the [gateway operations runbook](openwiki/operations/gateway.md).

## Exit codes

For `status` and `wait`:

| Code | Meaning                          |
| ---- | -------------------------------- |
| 0    | Settled cleanly                  |
| 1    | Settled with unresolved findings |
| 2    | Settled with failed checks       |
| 3    | Pending or timed out             |
| 4    | Usage or forge CLI error         |

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
