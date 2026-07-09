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
- `--timeout <secs>` — `wait` only; default 1800
- `--interval <secs>` — `wait` only; default 30

## Workflow

1. Run `babysit wait <number>` after pushing. It blocks until checks and bot reviews settle.
   Run it in the background if other work should continue meanwhile.
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
