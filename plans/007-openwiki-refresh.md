# Plan 007: Fix stale paths and claims in the OpenWiki docs

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 9b5cac3..HEAD -- openwiki/ src/lib.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live files before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW (docs only)
- **Depends on**: none
- **Category**: docs
- **Planned at**: commit `9b5cac3`, 2026-07-11

## Why this matters

`openwiki/` is the maintainer/agent-facing map of the codebase, generated at commit `4204db2` (see `openwiki/.last-update.json`) — *before* the event subsystem landed and before `src/bots.rs` was split into a directory module. It now points agents at a file that does not exist (`/src/bots.rs`, referenced 7 times), omits three public modules (`credentials`, `event`, `wait`) from the module list, and carries a stale initialization caveat ("cargo was not available"). Agents following these docs waste time on dead paths. This is a surgical correction of verified-stale facts, not a rewrite.

## Current state

Ground truth (verified at `9b5cac3`):

- `src/lib.rs` exposes: `bots`, `cli`, `core`, `credentials`, `event`, `forge`, `github`, `gitlab`, `wait`.
- The bots module is a directory: `src/bots/mod.rs` (adapters, default bot list, distillation) and `src/bots/regexes.rs` (shared regex statics). `src/bots.rs` does not exist (`ls src/bots.rs` → no such file).
- All verification gates run fine locally (`cargo test --locked --all` → 119 pass), so the quickstart's "cargo was not available" caveat is stale.

Stale references to fix (each verified by reading the file):

1. `openwiki/quickstart.md:12` — "Public modules: `/src/lib.rs` exposes `bots`, `cli`, `core`, `forge`, `github`, and `gitlab`." → missing `credentials`, `event`, `wait`.
2. `openwiki/quickstart.md:47` — "Default bots are defined in `/src/bots.rs`: ..." → path.
3. `openwiki/quickstart.md:93` — "Note: during this OpenWiki initialization, `cargo` was not available in the execution environment, so these checks could not be run live." → delete the whole line (and any resulting double blank line).
4. `openwiki/quickstart.md:99` — "Bot parsing or supported bot behavior: start in `/src/bots.rs`, ..." → path.
5. `openwiki/architecture/runtime.md:12` — "`/src/bots.rs` maps bot logins to adapters and distills noisy bot Markdown into compact findings." → path.
6. `openwiki/integrations/forges.md:26` — "Bot reviews and review-thread findings are parsed with bot adapters from `/src/bots.rs`." → path.
7. `openwiki/domain/review-findings.md:19` — "Default bot logins live in `/src/bots.rs`:" → path.
8. `openwiki/domain/review-findings.md:29` — "`/src/bots.rs` owns the cleanup rules:" → path.
9. `openwiki/domain/review-findings.md:46` — "`/src/bots.rs` can parse the `🧹 Nitpick comments` section into findings, ..." → path.

Replacement wording:

- For the module list (item 1): "Public modules: `/src/lib.rs` exposes `bots`, `cli`, `core`, `credentials`, `event`, `forge`, `github`, `gitlab`, and `wait`."
- For every `/src/bots.rs` path reference (items 2, 4-9): replace the path with `/src/bots/mod.rs`. In item 4 only (the "where to start" entry), use "`/src/bots/mod.rs` (regex helpers in `/src/bots/regexes.rs`)" so the split is discoverable.
- `openwiki/.last-update.json` currently records `"gitHead": "4204db2..."` — **leave this file alone**; it is tool-written metadata (see Out of scope).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Find stale refs | `rg -n "src/bots\.rs" openwiki/` | lists remaining stale paths (0 matches when done) |
| Verify module list | `rg -n "^pub mod" src/lib.rs` | the 9 modules named above |
| Sanity: repo untouched | `git status --porcelain` | only openwiki files (and `plans/README.md`) |

## Scope

**In scope** (the only files you should modify):
- `openwiki/quickstart.md`
- `openwiki/architecture/runtime.md`
- `openwiki/integrations/forges.md`
- `openwiki/domain/review-findings.md`

**Out of scope** (do NOT touch):
- `openwiki/.last-update.json` — tool-managed metadata; hand-editing would fake a tool run. A future OpenWiki update run refreshes it.
- `openwiki/operations/gateway.md`, `openwiki/testing.md` — no stale claims found in them at `9b5cac3`.
- `README.md`, `skills/` — separate docs, not audited as stale.
- Rewriting or expanding any doc's content beyond the nine listed corrections (e.g. do NOT write new sections about `event`/`wait` internals — that is a full doc-refresh job for the OpenWiki tool, deliberately not this plan).

## Git workflow

- Branch: `advisor/007-openwiki-refresh`
- Single commit, imperative ≤72-char subject, e.g. `Fix stale module paths in OpenWiki docs`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Apply the nine corrections

Edit the four files exactly as listed in "Current state" (items 1-9). Keep surrounding prose untouched.

**Verify**: `rg -n "src/bots\.rs" openwiki/` → 0 matches. `rg -n "cargo\` was not available" openwiki/` → 0 matches. `rg -n "credentials" openwiki/quickstart.md` → the updated module list line.

### Step 2: Read back each changed hunk

Run `git diff openwiki/` and confirm every hunk is one of the nine corrections — no drive-by rewording.

**Verify**: `git diff --stat openwiki/` → exactly 4 files changed; `git status --porcelain` shows nothing outside `openwiki/` (and `plans/README.md`).

## Test plan

Docs-only; no code tests. The verification greps in Steps 1-2 are the gate. Optionally run `cargo test --locked --all` to confirm nothing else was touched (should pass trivially).

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `rg -n "src/bots\.rs" openwiki/` → 0 matches
- [ ] `rg -c "src/bots/mod.rs" openwiki/quickstart.md openwiki/architecture/runtime.md openwiki/integrations/forges.md openwiki/domain/review-findings.md` → every file ≥1
- [ ] `openwiki/quickstart.md` module list names all of: bots, cli, core, credentials, event, forge, github, gitlab, wait
- [ ] `grep -n "was not available" openwiki/quickstart.md` → 0 matches
- [ ] `git diff --name-only` (vs the branch base) lists only the 4 in-scope files (and `plans/README.md`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Any of the nine cited lines no longer contains the quoted stale text (the docs were refreshed since `9b5cac3` — reconcile instead of blind-editing).
- `src/lib.rs`'s module list differs from the nine modules above (update the replacement wording only if the difference is verified against `src/lib.rs`, and note it in your report).
- You feel the urge to fix other inaccuracies you notice in `openwiki/` — list them in your report instead; expanding scope here breaks the "surgical correction" contract.

## Maintenance notes

- The proper long-term fix is re-running the OpenWiki update tool against current HEAD (which will also refresh `.last-update.json` and document `event`/`wait`/`credentials` properly). This plan just stops the docs from pointing at nonexistent files until then.
- `openwiki/quickstart.md:18` ("Recent git history shows...") describes history as of `4204db2`; left as-is because it is dated context, not a broken pointer — the full refresh will rewrite it.
