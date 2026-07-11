# Runtime architecture

`babysit` is organized as a small Rust library plus a thin binary wrapper. The binary in `/src/main.rs` collects process arguments, calls `babysit::cli::run`, and exits with the returned code. `/src/lib.rs` exposes the modules used by integration tests and the binary.

## Module responsibilities

- `/src/cli.rs` parses arguments, dispatches `status`, `findings`, and `wait`, calls the selected forge provider, renders output, and returns process exit codes.
- `/src/core.rs` defines the normalized domain model (`PrSnapshot`, `PrCheck`, `BotReview`, `Finding`, `ReviewThread`) and contains settle evaluation plus status/findings rendering.
- `/src/forge.rs` defines `ForgeProvider`, forge auto-detection, external command execution, JSON pagination helpers, and CLI error semantics.
- `/src/github.rs` implements `ForgeProvider` for GitHub through `gh`.
- `/src/gitlab.rs` implements `ForgeProvider` for GitLab through `glab`.
- `/src/bots.rs` maps bot logins to adapters and distills noisy bot Markdown into compact findings.
- `/src/wait.rs` owns the bounded authoritative-fetch loop and its `WakeSource` seam.
- `/src/event.rs` implements the opt-in GitHub WebSocket wake client; `/src/credentials.rs` owns
  its macOS Keychain token boundary.

## CLI execution flow

1. `/src/main.rs` passes arguments to `cli::run`.
2. `parse_args` in `/src/cli.rs` validates the subcommand and flags. `--timeout` and `--interval` are accepted only for `wait`; PR/MR numbers must be ASCII digits.
3. `fetch_snapshot` builds `SnapshotFetchOptions` and chooses GitHub or GitLab. If `--forge` is not supplied, `/src/forge.rs` reads `git remote get-url origin` and selects GitLab only when the remote host contains `gitlab`; otherwise it defaults to GitHub.
4. The selected provider returns a normalized `PrSnapshot`.
5. `evaluate_settled` in `/src/core.rs` decides whether the snapshot is settled.
6. `render_status` and/or `render_findings` produce the human/agent-readable output.
7. `exit_code_for` maps the settled snapshot to the CLI contract.

## Command behavior

- `status` fetches once, prints status, and returns the status exit code.
- `findings` fetches once, prints selected findings, and returns `0` if fetching/parsing succeeded.
- `wait` loops until settled or timed out. Retryable forge CLI failures are ignored until the
  deadline; non-retryable errors stop the command. The loop sleeps for the configured interval,
  capped by remaining timeout.
- `wait --events --gateway-url <wss-url>` retains that same loop but uses an event wake source.
  Event mode is GitHub-only and falls back every 300 seconds unless `--interval` is explicit. The
  manual deployment and operational boundary is documented in [Gateway operations](../operations/gateway.md).

## Event wake invariant

The gateway protocol is versioned JSON (`version: 1`) over an authenticated WebSocket opening
handshake. The client sends a `register` with forge, host, repository, PR number, head OID, and
last-seen cursor. It requires `ready(cursor)`, immediately fetches a new authoritative snapshot,
and ignores `wake` and `replay` notifications at or below that ready cursor. A `resync` always
requests a fresh snapshot, including when its cursor equals `ready`; later `wake` and `replay`
notifications also request a fetch. Their data is never used to decide settlement. A changed head
OID replaces the registration and repeats the ready/fetch ordering. Transient transport,
429, and 5xx failures use bounded reconnect delay while polling continues; malformed protocol and
401/403 failures stop with a configuration error.

## Settle logic

`evaluate_settled` in `/src/core.rs` is the central business rule:

- `checks_pending` counts checks whose state is `Pending`.
- `review_landed` is true when a configured bot review matches the snapshot head or when a configured bot check has completed.
- A review matches the head when its commit OID equals `head_oid`; if there is no direct OID match, the code compares ISO-like timestamps lexicographically against `head_committed_at` to preserve prior TypeScript behavior.
- A terminal PR/MR state (`state != "OPEN"`) is settled even if checks/reviews are not otherwise complete.
- Otherwise, settling requires zero pending checks and either a landed review or `--no-reviews`.

## Rendering and exit codes

`render_status` prints PR/MR identity, abbreviated head SHA, checks, bot reviews, and a summary line. `render_findings` hoists shared preambles across multiple findings so agent-oriented instructions are not repeated.

For `status` and `wait`, `/src/core.rs` returns:

- `0` when settled cleanly;
- `1` when settled with unresolved, non-outdated findings;
- `2` when settled with failed checks;
- `3` when still pending or timed out;
- `4` for usage or forge CLI errors, via `/src/cli.rs` and `/src/forge.rs`.

## Extension guidance

- Keep the forge providers responsible only for fetching and normalizing external data into `PrSnapshot`.
- Keep business decisions such as settling, unresolved finding selection, and rendering in `/src/core.rs` or `/src/cli.rs`.
- When adding a subcommand, update `CommandName`, `parse_command`, dispatch in `run_inner`, usage text, README usage, and `/tests/babysit_cli.rs`.
- When adding a new forge, implement `ForgeProvider`, add a new `ForgeName` variant, update CLI parsing/auto-detection, and add parser fixtures instead of depending on live external services in tests.
