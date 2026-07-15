---
name: babysit
description: Watch a GitHub pull request or GitLab merge request until CI checks and bot reviews (CodeRabbit, Codex, Cursor) settle, then report unresolved findings using the `babysit` CLI. PR and MR are interchangeable. Use when asked to "baby sit this PR", "babysit this MR", "watch this PR", "monitor this MR", "keep an eye on this PR", or otherwise wait on a pull/merge request's checks and reviews after pushing.
---

# babysit

`babysit` is a Rust CLI that polls a GitHub PR (via `gh`) or GitLab MR (via `glab`) until
CI checks and selected bot reviews settle, then distills bot review threads into compact
findings. Prefer it over hand-rolled `gh pr checks` / `glab` polling loops.

Requires `babysit` on PATH plus an authenticated `gh` (GitHub) or `glab` (GitLab).

## Usage

```bash
babysit status|findings|wait [<pr-or-mr-number>] [options]
```

The number can be omitted to use the current branch's open PR/MR.

Options:

- `-R, --repo <owner/repo>` — repository to inspect
- `--forge <github|gitlab>` — default: auto-detect from git origin
- `--bots <csv>` — bot logins to treat as reviewers (default: `coderabbitai`, `chatgpt-codex-connector`, `cursor`)
- `--all` — include resolved and outdated findings
- `--nitpicks` — include CodeRabbit nitpick review-body findings
- `--no-reviews` — settle without waiting for a bot review
- `--timeout <secs>` — `wait` only; default 300 (5 minutes)
- `--interval <secs>` — `wait` only; default 30 when polling, 300 as the event-mode fallback
- `--events` — `wait` only; use GitHub webhook events to wake the waiter
- `--gateway-url <wss-url>` — required with `--events`; the gateway's base WebSocket URL

## Event-assisted waits

For a GitHub repository that already has the babysit gateway webhook configured, prefer event mode:

```bash
babysit gateway-token status
babysit wait <number> --repo owner/repo --events \
  --gateway-url wss://babysit.mindgoblin.pw/watch
```

The gateway token must first be enrolled in the macOS Keychain with
`babysit gateway-token enroll`. Pass only the base `wss://` gateway URL: do not append a repository
path, query, fragment, or credential. Event mode is GitHub-only; use normal polling for GitLab or
for repositories without the webhook setup.

Webhook events are wake signals, not authoritative PR state. The CLI still fetches GitHub after a
wake and falls back to polling every 300 seconds by default. An explicit `--interval <secs>` changes
that fallback interval, and `--timeout <secs>` remains the overall deadline. Configuration and
authorization errors are fatal; transient disconnects retry while fallback polling continues.

## Workflow

1. Run `babysit wait <number>` after pushing. Add `--events --gateway-url <wss-url>` when the
   GitHub repository has the webhook gateway configured. It blocks until checks and bot reviews
   settle. Run it in the background if other work should continue meanwhile.
2. Check the exit code, then run `babysit findings <number>` to list unresolved findings.
3. Address findings, push fixes, and repeat until it settles cleanly.

## Exit codes (`status` and `wait`)

| Code | Meaning |
| ---- | ------- |
| 0 | Settled cleanly |
| 1 | Settled with unresolved findings |
| 2 | Settled with failed checks |
| 3 | Pending or timed out |
| 4 | Usage or forge CLI error |

`findings` exits `0` when it lists findings successfully.
