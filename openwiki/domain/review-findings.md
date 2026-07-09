# Review findings domain

The product purpose of `babysit` is to turn forge-specific PR/MR state and noisy automated review comments into a compact decision: are checks and review bots settled, and are there actionable findings left?

## Normalized snapshot model

`/src/core.rs` defines the forge-neutral model:

- `PrSnapshot`: PR/MR number, title, state, draft flag, source/target branches, head SHA, optional head commit timestamp, owner/repo, checks, bot reviews, and findings.
- `PrCheck`: check or job name plus normalized `CheckState` (`Pending`, `Passed`, `Failed`, `Skipped`).
- `BotReview`: bot short name, submission time, optional commit OID, and optional actionable count.
- `Finding`: path, line or line range, bot short name, optional severity, title, detail, and resolved/outdated flags.
- `ReviewThread`: intermediate representation for inline review discussions before bot-specific distillation.

GitHub and GitLab providers both normalize their external JSON into this model before the CLI evaluates anything.

## Configured bots

Default bot logins live in `/src/bots.rs`:

- `coderabbitai` → short name `coderabbit`; check names `coderabbitai`, `coderabbit`.
- `chatgpt-codex-connector` → short name `codex`; check names `chatgpt-codex-connector`, `codex`.
- `cursor` → short name `bugbot`; check names `cursor`, `bugbot`.

`normalize_bot_login` lowercases logins and strips a trailing `[bot]`. `--bots <csv>` replaces the configured list. Unknown configured bots still get a generic adapter that uses the login as both short name and check name.

## Distilling bot comments

Bot review comments contain HTML, badges, suggestions, details blocks, and other noise. `/src/bots.rs` owns the cleanup rules:

- CodeRabbit extracts severity from italic header metadata, prefers bold-only titles, and uses the `Prompt for AI Agents` code block when present. It also parses actionable-count text from review bodies.
- Codex extracts `P` severity badges, removes image badge markup and feedback prompts, and keeps a compact title/detail.
- Cursor Bugbot extracts `###` titles, `High|Medium|Low Severity`, HTML-comment description blocks, and actionable-count text from review bodies.
- Generic bots fall back to the first prose line plus noise-stripped detail.

`strip_noise` removes `<details>` blocks, HTML comments, selected tags, and excessive blank lines. Tests in `/tests/babysit.rs` use Markdown fixtures in `/tests/fixtures/` to lock this behavior.

## Findings and filtering

`finding_from_thread` converts a `ReviewThread` into a `Finding` only if the thread author matches the configured bots. Line ranges are rendered by `format_line_range`.

By default, CLI output selects `unresolved_findings`: findings that are neither `resolved` nor `outdated`. `--all` includes resolved and outdated findings. `render_findings` annotates resolved/outdated findings when they are included.

## Nitpicks

CodeRabbit nitpicks are review-body comments rather than normal inline review threads. `/src/bots.rs` can parse the `🧹 Nitpick comments` section into findings, but `/src/github.rs` includes those findings only when `--nitpicks` is passed.

A recent fix (`4204db2 Ignore stale CodeRabbit nitpicks`) ensures GitHub nitpicks are parsed only from reviews that match the current head commit or were submitted after the head commit timestamp. Preserve this guard when changing nitpick behavior; stale review-body suggestions should not reappear as current findings.

## GitHub vs GitLab review semantics

- GitHub review threads provide `isResolved`, `isOutdated`, path, line/startLine, and comments through GraphQL in `/src/github.rs`.
- GitHub bot reviews include `submittedAt`, body, and commit OID, so review/head matching can use the direct OID.
- GitLab discussions provide notes with `resolvable`, `resolved`, position path/line, and position `head_sha`; `/src/gitlab.rs` treats a discussion as outdated when the note head SHA differs from the current head SHA.
- GitLab top-level bot notes are used as bot reviews, while resolvable notes are converted to findings.

## Change guidance

- Add or update fixtures for every new bot Markdown shape; do not rely on live bot output in tests.
- Be careful with resolved/outdated semantics because they directly affect the default exit code `1` vs `0`.
- If a bot adds a new check name, update the adapter so `bot_check_landed` can still settle when the bot review itself is unavailable.
- Keep Markdown cleanup conservative. Removing too much can hide actionable detail; removing too little makes output noisy for agents.
