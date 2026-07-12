# Plan 001: Add GitHub Actions CI so PRs cannot merge unverified

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 9b5cac3..HEAD -- .gitlab-ci.yml .github/`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: dx
- **Planned at**: commit `9b5cac3`, 2026-07-11

## Why this matters

The repo is hosted on GitHub (`https://github.com/volker48/babysit`, PRs are merged there — see `git log`), but the only CI definition is `.gitlab-ci.yml`, and the maintainer has confirmed no GitLab mirror runs it. **GitHub PRs currently merge with zero automated verification.** Every other plan in this directory relies on `cargo test` / `pnpm test` gates; adding CI first means those later PRs get checked automatically. This plan mirrors the existing GitLab pipeline's jobs exactly — it adds no new checks and changes no code.

## Current state

- `.gitlab-ci.yml` — the only CI config. Full job list (verified at `9b5cac3`):
  - Rust jobs on image `rust:1.85.1-bookworm@sha256:bf7d8766...` with `before_script: rustup component add rustfmt clippy`:
    - `fmt`: `cargo fmt --all --check`
    - `clippy`: `cargo clippy --locked --all-targets --all-features -- -D warnings`
    - `test`: `cargo test --locked --all`
    - `release`: `cargo build --locked --release` (artifacts: `target/release/babysit`)
  - Gateway jobs on image `node:24-bookworm-slim@sha256:cb4e8f7c...` with before_script:
    ```yaml
    - corepack enable
    - pnpm config set minimumReleaseAge 1440
    - pnpm config set ignore-scripts true
    - pnpm install --frozen-lockfile --ignore-scripts
    ```
    running `pnpm --filter @babysit/gateway lint | format:check | typecheck | test`.
  - Cargo cache keyed on `Cargo.lock`, paths `.cargo/registry/`, `.cargo/git/`, `target/`. All jobs `interruptible: true`.
- `rust-toolchain.toml` — pins the toolchain; rustup reads this file automatically:
  ```toml
  [toolchain]
  channel = "1.85.1"
  components = ["rustfmt", "clippy"]
  ```
- `package.json` (repo root) — `"packageManager": "pnpm@10.32.1"` (corepack reads this pin).
- `pnpm-workspace.yaml` — single package: `gateway`.
- There is no `.github/` directory at `9b5cac3`.
- **Repo convention (from the user's standing instructions): GitHub Actions must be pinned to full commit SHAs with a version comment.** The SHAs below were verified against `gh api repos/<owner>/<repo>/tags` on 2026-07-11:
  - `actions/checkout` v7.0.0 → `9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0`
  - `actions/setup-node` v6.4.0 → `48b55a011bda9f5d6aeb4c2d9c7362e8dae4041e`
  - `actions/cache` v6.1.0 → `55cc8345863c7cc4c66a329aec7e433d2d1c52a9`

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Workflow lint | `actionlint .github/workflows/ci.yml` | exit 0, no output |
| Workflow security lint | `zizmor .github/workflows/ci.yml` | exit 0 (or only findings you can fix, e.g. add `persist-credentials: false`) |
| Local Rust gates (sanity) | `cargo fmt --all --check && cargo test --locked --all` | exit 0, 119+ tests pass |
| Local gateway gates (sanity) | `pnpm --filter @babysit/gateway test` | 63+ tests pass |

`actionlint` and `zizmor` are installed on the author's machine (`/opt/homebrew/bin/actionlint`, `~/.local/bin/zizmor`). If they are missing in your environment, note that in your report instead of installing anything.

## Scope

**In scope** (the only files you should modify):
- `.github/workflows/ci.yml` (create)

**Out of scope** (do NOT touch, even though they look related):
- `.gitlab-ci.yml` — keep it; it documents the canonical gates and may be used later.
- Any source code, `Cargo.toml`, `package.json`.
- Scheduled/cron workflows — the repo owner's standards forbid scheduled CI without code changes.
- Artifact upload of the release binary — deliberately deferred (see Maintenance notes).

## Git workflow

- Branch: `advisor/001-github-actions-ci`
- Commit style: imperative mood, ≤72-char subject (matching e.g. `30e26e8 Move babysit skill to top-level skills`). Suggested: `Add GitHub Actions CI mirroring GitLab gates`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Create `.github/workflows/ci.yml`

Create the file with exactly this content:

```yaml
name: CI

on:
  pull_request:
  push:
    branches: [main]

permissions:
  contents: read

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}

jobs:
  rust:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7.0.0
        with:
          persist-credentials: false
      - uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
            target
          key: rust-${{ runner.os }}-${{ hashFiles('Cargo.lock', 'rust-toolchain.toml') }}
      - name: Install pinned toolchain from rust-toolchain.toml
        run: rustup toolchain install
      - run: cargo fmt --all --check
      - run: cargo clippy --locked --all-targets --all-features -- -D warnings
      - run: cargo test --locked --all
      - run: cargo build --locked --release

  gateway:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7.0.0
        with:
          persist-credentials: false
      - uses: actions/setup-node@48b55a011bda9f5d6aeb4c2d9c7362e8dae4041e # v6.4.0
        with:
          node-version: "24"
      - run: corepack enable
      - run: pnpm config set minimumReleaseAge 1440
      - run: pnpm config set ignore-scripts true
      - run: pnpm install --frozen-lockfile --ignore-scripts
      - run: pnpm --filter @babysit/gateway lint
      - run: pnpm --filter @babysit/gateway format:check
      - run: pnpm --filter @babysit/gateway typecheck
      - run: pnpm --filter @babysit/gateway test
```

Notes on why the file looks like this (do not "improve" these):
- `rustup toolchain install` with no arguments installs the channel and components from `rust-toolchain.toml` (rustup ≥1.28 behavior). Do not hardcode `1.85.1` in the workflow — the toolchain file is the single source of truth.
- `corepack enable` + the root `package.json` `packageManager` field pins pnpm to 10.32.1 — do not add a third-party pnpm setup action.
- The three `pnpm config` / install lines mirror `.gitlab-ci.yml` verbatim.
- `concurrency` mirrors GitLab's `interruptible: true`.

**Verify**: `actionlint .github/workflows/ci.yml` → exit 0, no output.

### Step 2: Run the security linter

**Verify**: `zizmor .github/workflows/ci.yml` → exit 0. If it reports findings, fix only within `ci.yml` (e.g. it should already be satisfied by `persist-credentials: false` and the `permissions` block). If a finding would require changing anything outside `ci.yml`, STOP.

### Step 3: Confirm the mirrored commands really pass locally

Run the four Rust commands and four gateway commands from the workflow locally, in order.

**Verify**: all exit 0. (`cargo test --locked --all` → 119+ tests pass; `pnpm --filter @babysit/gateway test` → 63+ tests pass.)

## Test plan

No unit tests — the deliverable is the workflow file itself. Verification is `actionlint` + `zizmor` + local execution of every mirrored command (Step 3). The real end-to-end check happens on the first PR that runs it; note this in your report.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `.github/workflows/ci.yml` exists and `actionlint .github/workflows/ci.yml` exits 0
- [ ] `zizmor .github/workflows/ci.yml` exits 0
- [ ] Every `run:` command in the workflow exits 0 when run locally
- [ ] `grep -c '@[0-9a-f]\{40\}' .github/workflows/ci.yml` ≥ 3 (all actions SHA-pinned)
- [ ] `git status --porcelain` shows only `.github/workflows/ci.yml` (and `plans/README.md`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Any of the three action SHAs no longer resolves (e.g. `git ls-remote https://github.com/actions/checkout 9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0` finds nothing) — do NOT substitute an unverified SHA.
- A mirrored command fails locally at the baseline commit (the gates were all green at `9b5cac3`; a failure means drift).
- `zizmor` reports a finding whose fix requires repo settings or files outside `ci.yml`.

## Maintenance notes

- When `.gitlab-ci.yml` gains or loses a job, mirror the change here (and vice versa). Consider consolidating to one CI system later.
- Deliberately deferred: uploading the release binary as a workflow artifact (GitLab's `release` job does this; add `actions/upload-artifact` — SHA-pinned — if release artifacts from GitHub are ever needed).
- Reviewers should check that no action is tag-pinned; Dependabot can bump the SHAs if enabled later.
