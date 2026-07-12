<p>
<img src="https://raw.githubusercontent.com/volker48/babysit/main/banner.png" alt="babysit" width="1100">
</p>

# babysit

`babysit` is a Rust CLI for watching pull requests and merge requests until CI
checks and selected bot reviews settle. It supports GitHub through `gh` and
GitLab through `glab`, then distills bot review threads into compact findings for
agents or humans to act on.

## Install

Release binaries require macOS. Install the archive for your Mac into a directory on `PATH`:

```bash
set -euo pipefail
version="$(gh release view --repo volker48/babysit --json tagName \
  --jq '.tagName | ltrimstr("v")')"
case "$(uname -m)" in
  arm64) target=aarch64-apple-darwin ;;
  x86_64) target=x86_64-apple-darwin ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac
archive="babysit-v${version}-${target}.tar.gz"
gh release download "v${version}" --repo volker48/babysit \
  --pattern "$archive" --pattern "$archive.sha256"
shasum -a 256 -c "$archive.sha256"
tar -xzf "$archive"
mkdir -p "$HOME/.local/bin"
install -m 755 babysit "$HOME/.local/bin/babysit"
```

Ensure `$HOME/.local/bin` is on `PATH`. Runtime prerequisites are `gh` authenticated for GitHub
repositories or `glab` authenticated for GitLab repositories.

To build from source, install Rust 1.85.1 or newer and run:

```bash
cargo build --locked --release
./target/release/babysit status 123 --repo owner/repo
```

## Add the companion skill

The canonical skill source lives in `skills/babysit/SKILL.md`, matching the top-level skill layout
used by the open agent skills ecosystem. I would add this globally so it is available in all your
repos. Feel free to adapt the skill to how you want to work.

```bash
npx skills add volker48/babysit
```

This repository also keeps thin `.claude/skills/babysit` and `.codex/skills/babysit` discovery
wrappers for local agent sessions. The Rust test suite checks that their trigger metadata stays in
sync with the canonical skill.

## Usage

```bash
babysit status [<pr-or-mr-number>] [options]
babysit findings [<pr-or-mr-number>] [options]
babysit wait [<pr-or-mr-number>] [options]
babysit gateway-token <enroll|status|delete|rotate>
babysit --help
babysit --version
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
-h, --help               Show help without contacting a forge
-V, --version            Show the installed version
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

## Release policy

Version tags matching the crate version (for example, `v1.0.0`) publish native archives for these
supported targets:

| Target | GitHub-hosted runner |
| --- | --- |
| Apple silicon (`aarch64-apple-darwin`) | `macos-15` |
| Intel (`x86_64-apple-darwin`) | `macos-15-intel` |

The GitHub repository owner (`volker48`) owns releases; `.github/workflows/release.yml` performs
build and publication. It uses locked Cargo dependencies, smoke-tests `--help` and `--version`,
and publishes per-archive SHA-256 files plus `SHA256SUMS`.

Initial releases are not code-signed or notarized and do not publish separate build provenance.
Checksums provide download-integrity verification, not publisher identity. Revisit signing and
provenance before expanding the supported platform matrix.
