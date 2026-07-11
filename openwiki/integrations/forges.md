# Forge integrations

`babysit` does not call GitHub or GitLab APIs directly with embedded credentials. It shells out to the authenticated forge CLIs and normalizes their JSON responses.

## Shared external-command layer

`/src/forge.rs` contains shared integration infrastructure:

- `ForgeName` distinguishes GitHub and GitLab.
- `ForgeProvider` is the trait implemented by each provider.
- `SnapshotFetchOptions` carries PR/MR number, repo, configured bots, and nitpick preference.
- `run_json` executes an external command, enforces a 30-second command timeout, captures stdout/stderr concurrently, rejects signaled or failed processes, and parses stdout as JSON.
- `run_json_pages` and `collect_json_pages` normalize paginated array responses, with a hard cap of 100 pages.
- `CliError` includes an exit code and a `retryable` flag; `wait` retries retryable provider failures until timeout.

Forge auto-detection reads `git remote get-url origin`. Hosts containing `gitlab` select GitLab; all other cases, including missing origin, default to GitHub.

## GitHub provider

`/src/github.rs` implements GitHub through `gh`:

1. `gh pr view [<pr>] [-R <repo>] --json ...` fetches base PR metadata, head SHA, commits, URL-derived owner/repo, and status checks.
2. `gh api graphql` runs `REVIEW_QUERY` to fetch recent reviews and review threads.
3. Review threads are paginated with `reviewThreads(first: 100, after: $reviewThreadsCursor)`, capped at 100 pages, and rejected if the cursor stops advancing.
4. Checks are normalized from both `CheckRun` and `StatusContext` payloads.
5. Bot reviews and review-thread findings are parsed with bot adapters from `/src/bots.rs`.
6. CodeRabbit nitpick review-body findings are appended only when `--nitpicks` is enabled and the review matches the current head.

Important tests in `/tests/babysit.rs` verify PR parsing, check-state mapping, GraphQL pagination shape, bot review parsing, and stale nitpick filtering.

## GitLab provider

`/src/gitlab.rs` implements GitLab through `glab`:

1. `glab mr view [<mr>] -F json [-R <repo>]` fetches MR metadata.
2. The provider parses required MR identity fields strictly, then derives host, group/owner, and repo from `web_url`; project IDs and pipeline IDs come from the MR JSON.
3. If a head pipeline exists, `glab api projects/{project_id}/pipelines/{pipeline_id}/jobs?...` fetches pipeline jobs as checks.
4. `glab api projects/{project_id}/merge_requests/{iid}/discussions?...` fetches MR discussions through the shared pagination helper.
5. `glab api projects/{project_id}/repository/commits/{sha}` fetches the head commit timestamp.
6. Resolvable discussion notes become findings; top-level non-resolvable bot notes become bot reviews.

GitLab job status mapping is intentionally simple: `success` passes, `skipped`/`manual` skip, `failed`/`canceled` fail, and all other statuses remain pending.

## Integration caveats

- The CLI depends on `gh`/`glab` authentication and output stability. Parser tests use fixtures to reduce dependency on live services.
- External command timeout is fixed at 30 seconds per command in `/src/forge.rs`; `wait --timeout` controls the overall wait loop, not each subprocess.
- GitHub derives owner/repo from the PR URL. GitLab derives host/group/repo from MR `web_url`.
- GitLab API calls include `--hostname` when a host was parsed, supporting non-default GitLab hosts.
- Pagination failures are non-retryable parse/integration errors; subprocess failures from `run_json` are marked retryable unless JSON parsing fails.

## Adding or changing integrations

- Prefer adding parser functions that accept `serde_json::Value` and can be tested with fixtures.
- Keep CLI command construction small and deterministic so tests can reason about it.
- Preserve normalized `PrSnapshot` semantics; avoid leaking forge-specific state into `/src/core.rs`.
- When adding fields to the snapshot model, update both providers and tests, even if one forge must use `None` or an empty vector.
