# Plan 006: Small-fixes batch — rand advisory bump, hoisted regexes, --repo validation

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. The three fixes are independent; if one hits a
> STOP condition, finish the others and report the blocked one. When done,
> update the status row for this plan in `plans/README.md` — unless a reviewer
> dispatched you and told you they maintain the index.
>
> **Drift check (run first)**: `git diff --stat 9b5cac3..HEAD -- Cargo.toml Cargo.lock src/github.rs src/gitlab.rs src/cli.rs tests/babysit_cli.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: security / perf / bug (three small independent fixes)
- **Planned at**: commit `9b5cac3`, 2026-07-11

## Why this matters

Three small, verified issues, batched because each is under an hour:

1. **RUSTSEC-2026-0097**: `rand` is pinned `=0.9.2` (`Cargo.toml:11`); the advisory (unsoundness, patched in ≥0.9.3) is currently an *allowed warning* in `cargo audit`. The crate is on the event-mode path (`jittered_delay`, `src/event.rs:380-383`). One-line bump.
2. **Per-fetch regex compilation**: `src/github.rs:94` and `src/gitlab.rs:247` compile a `Regex` inside functions called on every snapshot fetch — in `wait` mode that is every poll interval. The repo already has the right pattern (`static LazyLock<Regex>` in `src/bots/regexes.rs`); these two are stragglers. Minor perf, mostly consistency.
3. **`--repo=<value>` bypasses the leading-dash guard**: the space form (`--repo -x`) is rejected by `required_value` (`src/cli.rs:196-201`, refuses values starting with `-`), but the inline form (`--repo=-x`) routes through `inline_value` → `assign_value` → `non_empty`, which only rejects empty strings. A `-`-prefixed value then reaches `gh`/`glab` argument slots where it can be parsed as a flag. Same gap applies to `--gateway-url=-x`, but `GatewayConfig::parse` already rejects non-`wss://` values, so only `--repo` needs the guard.

## Current state

(All at commit `9b5cac3`.)

- `Cargo.toml:11` — `rand = "=0.9.2"` (all deps in this file use exact `=` pins — that is deliberate repo policy; keep the `=` style).
- `cargo audit` output at baseline ends with: `warning: 1 allowed warning found` for RUSTSEC-2026-0097 (rand 0.9.2, patched >=0.9.3).
- `src/github.rs:92-97`:
  ```rust
  pub fn parse_pr_view(raw: &Value) -> Result<PrSnapshot, String> {
      let url = raw.get("url").and_then(Value::as_str).unwrap_or("");
      let re = Regex::new(r"github\.com/([^/]+)/([^/]+)/pull/").unwrap();
      let caps = re
          .captures(url)
          .ok_or_else(|| format!("cannot derive owner/repo from PR url: {url}"))?;
  ```
- `src/gitlab.rs:246-250`:
  ```rust
  fn parse_gitlab_project(web_url: &str) -> Result<(String, String, String), CliError> {
      let re = Regex::new(r"^https?://([^/]+)/(.+?)/-/merge_requests/").unwrap();
      let Some(caps) = re.captures(web_url) else {
          return Err(parse_json_failure("glab mr view", "invalid web_url"));
      };
  ```
- The repo's exemplar pattern, `src/bots/regexes.rs:1-8`:
  ```rust
  use std::sync::LazyLock;

  use regex::Regex;

  pub(super) static CODE_RABBIT_ACTIONABLE_RE: LazyLock<Regex> = LazyLock::new(|| {
      Regex::new(r"\*\*Actionable comments posted: (\d+)\*\*").expect("valid CodeRabbit count regex")
  });
  ```
- `src/cli.rs` parsing paths:
  ```rust
  // src/cli.rs:176-182 — inline form, no dash guard
  fn inline_value(arg: &str) -> Option<(&str, &str)> {
      if !arg.starts_with("--") {
          return None;
      }
      let index = arg.find('=')?;
      Some((&arg[..index], &arg[index + 1..]))
  }
  ```
  ```rust
  // src/cli.rs:196-201 — space form, has the dash guard
  fn required_value(argv: &[String], index: usize, flag: &str) -> Result<String, UsageError> {
      match argv.get(index + 1) {
          Some(value) if !value.starts_with('-') => Ok(value.clone()),
          _ => Err(UsageError::new(format!("{flag} requires a value"))),
      }
  }
  ```
  ```rust
  // src/cli.rs:216-218 — where --repo is assigned
  fn assign_value(state: &mut ParseState, flag: ValueFlag, value: &str) -> Result<(), UsageError> {
      match flag {
          ValueFlag::Repo => state.opts.repo = Some(non_empty(value, "--repo")?),
  ```
  ```rust
  // src/cli.rs:272-279
  fn non_empty(value: &str, flag: &str) -> Result<String, UsageError> {
      let trimmed = value.trim();
      if trimmed.is_empty() {
          Err(UsageError::new(format!("{flag} requires a value")))
      } else {
          Ok(trimmed.to_string())
      }
  }
  ```
  **Important constraint**: do NOT validate `--repo` against an `owner/repo` shape — GitLab accepts multi-segment paths (`group/subgroup/repo`), and both CLIs accept URLs. Only the leading `-` is invalid.
- `tests/babysit_cli.rs` — CLI parse tests. Conventions: `args(&[...])` helper (lines 5-7); an existing `--repo` inline-form usage at line 110 (`"--repo=example-org/example-repo"`); an existing error-case assertion at line 196 (`parse_args(&args(&["status", "--repo"]))` expected to error).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Bump lockfile | `cargo update --package rand --precise 0.9.3` | exit 0, Cargo.lock updated |
| Audit | `cargo audit` | RUSTSEC-2026-0097 no longer reported |
| Format / lint | `cargo fmt --all --check` / `cargo clippy --locked --all-targets --all-features -- -D warnings` | exit 0 |
| Focused tests | `cargo test --locked --test babysit_cli` | all pass |
| Full tests | `cargo test --locked --all` | all pass |
| Release build | `cargo build --locked --release` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `Cargo.toml`, `Cargo.lock` (fix 1)
- `src/github.rs`, `src/gitlab.rs` (fix 2)
- `src/cli.rs`, `tests/babysit_cli.rs` (fix 3)

**Out of scope** (do NOT touch):
- Other dependency versions — bump `rand` only; no `cargo update` without `--package rand`.
- `src/bots/regexes.rs` — it is the exemplar, not a target.
- `--gateway-url` validation (covered by `GatewayConfig::parse`), `--bots`, `--forge` parsing.
- Any behavioral change to how `repo` is passed to `gh`/`glab`.

## Git workflow

- Branch: `advisor/006-small-fixes`
- One commit per fix (repo convention: one logical change per commit), imperative ≤72-char subjects, e.g.:
  1. `Bump rand to 0.9.3 for RUSTSEC-2026-0097`
  2. `Hoist forge URL regexes to LazyLock statics`
  3. `Reject dash-prefixed values for --repo`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Bump rand

Edit `Cargo.toml:11` to `rand = "=0.9.3"`, then run `cargo update --package rand --precise 0.9.3`.

**Verify**: `cargo audit` → no RUSTSEC-2026-0097 (a clean run or only *other* pre-existing warnings); `git diff Cargo.lock` touches only `rand` (and its checksum). Then `cargo test --locked --all` → all pass.

### Step 2: Hoist the two regexes

- `src/github.rs`: add at module scope (near the top, after imports; add `use std::sync::LazyLock;`):
  ```rust
  static PR_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
      Regex::new(r"github\.com/([^/]+)/([^/]+)/pull/").expect("valid PR url regex")
  });
  ```
  In `parse_pr_view`, delete the local `let re = ...` and use `PR_URL_RE.captures(url)`.
- `src/gitlab.rs`: same treatment for the line-247 regex, e.g. `MR_URL_RE`, used from `parse_gitlab_project`.
- Match the `.expect("...")` style of `src/bots/regexes.rs` (not `.unwrap()`).

**Verify**: `rg -n "Regex::new" src/github.rs src/gitlab.rs` → matches only inside `LazyLock::new` closures. `cargo test --locked --all` → all pass (existing parser tests cover both functions).

### Step 3: Guard inline `--repo` values — test first

In `tests/babysit_cli.rs`, add a test `rejects_dash_prefixed_repo_values` asserting **both** forms error:

```rust
assert!(parse_args(&args(&["status", "--repo=-malicious"])).is_err());
assert!(parse_args(&args(&["status", "--repo", "-malicious"])).is_err());
```

and that a normal inline value still parses (reuse the existing pattern from line 110). Run `cargo test --locked --test babysit_cli` → the new test FAILS on the inline case (space form already errors).

Then in `src/cli.rs` `assign_value`, guard the `Repo` arm before `non_empty`:

```rust
ValueFlag::Repo => {
    if value.trim_start().starts_with('-') {
        return Err(UsageError::new(format!("invalid --repo value: {value}")));
    }
    state.opts.repo = Some(non_empty(value, "--repo")?);
}
```

**Verify**: `cargo test --locked --test babysit_cli` → all pass, including the new test.

### Step 4: Full gate

**Verify**: `cargo fmt --all --check`, `cargo clippy --locked --all-targets --all-features -- -D warnings`, `cargo test --locked --all`, `cargo build --locked --release` → all exit 0.

## Test plan

- Fix 1: no new tests (dependency bump); gate is `cargo audit` + full suite.
- Fix 2: no new tests — `parse_pr_view` and `parse_gitlab_project` are already covered by existing snapshot-parsing tests; the refactor is behavior-preserving.
- Fix 3: `rejects_dash_prefixed_repo_values` in `tests/babysit_cli.rs`, verified failing before the guard and passing after.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `grep -n 'rand = "=0.9.3"' Cargo.toml` → 1 match; `cargo audit` does not mention RUSTSEC-2026-0097
- [ ] `rg -n "let re = Regex::new" src/github.rs src/gitlab.rs` → no matches
- [ ] `cargo test --locked --test babysit_cli` passes and includes `rejects_dash_prefixed_repo_values`
- [ ] `cargo fmt --all --check`, `cargo clippy ... -D warnings`, `cargo test --locked --all`, `cargo build --locked --release` all exit 0
- [ ] `git status --porcelain` shows only the six in-scope files (and `plans/README.md`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- `cargo update --package rand --precise 0.9.3` wants to change any crate other than `rand` in `Cargo.lock`, or 0.9.3 is yanked/unavailable — report; do not pick a different version yourself.
- `cargo audit` reports a NEW advisory after the bump.
- The `src/cli.rs` excerpts don't match (the parser has been refactored).
- Any existing test relies on a dash-prefixed `--repo=` value parsing successfully (none found at `9b5cac3`).

## Maintenance notes

- Reviewers: check the `rand` bump changes only `Cargo.toml`+`Cargo.lock`, and that the `--repo` guard rejects `-` values in *both* argument forms.
- If more per-call `Regex::new` sites appear later, follow the same `LazyLock` hoist; consider a clippy lint or grep in CI if it recurs.
- If an `allowed-warnings`/`audit.toml` entry exists for RUSTSEC-2026-0097 anywhere (none found in-repo at `9b5cac3`; the "allowed warning" is cargo-audit's default severity handling), remove it once the bump lands.
