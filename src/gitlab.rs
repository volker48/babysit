use regex::Regex;
use serde_json::Value;

use crate::bots::adapter_for_login;
use crate::core::{BotReview, CheckState, PrCheck, PrSnapshot, ReviewThread, finding_from_thread};
use crate::forge::{
    CliError, ForgeProvider, SnapshotFetchOptions, parse_json_failure, run_json_deadline,
    run_json_pages,
};

#[derive(Debug, Clone)]
pub struct GitLabMrParseResult {
    pub snapshot: PrSnapshot,
    pub project_id: String,
    pub pipeline_id: Option<String>,
    pub host: String,
}

pub struct GitLabProvider;

impl ForgeProvider for GitLabProvider {
    fn fetch_snapshot(&self, opts: &SnapshotFetchOptions) -> Result<PrSnapshot, CliError> {
        let mr = parse_gitlab_mr_json(run_json_deadline(
            "glab",
            &mr_view_args(opts),
            "glab mr view",
            opts.deadline,
        )?)?;
        let checks = if let Some(pipeline_id) = &mr.pipeline_id {
            fetch_pipeline_jobs(&mr.project_id, pipeline_id, &mr.host, opts.deadline)?
        } else {
            Vec::new()
        };
        let discussions = run_json_pages(
            |page, per_page| {
                run_json_deadline(
                    "glab",
                    &discussions_args(&mr.project_id, mr.snapshot.number, &mr.host, page, per_page),
                    &format!("glab api discussions page {page}"),
                    opts.deadline,
                )
            },
            "glab api discussions",
            100,
        )?;
        let commit = run_json_deadline(
            "glab",
            &commit_args(&mr.project_id, &mr.snapshot.head_oid, &mr.host),
            "glab api commit",
            opts.deadline,
        )?;
        let mut snapshot = mr.snapshot;
        snapshot.head_committed_at = commit
            .get("committed_date")
            .and_then(Value::as_str)
            .map(str::to_string);
        snapshot.checks = checks;
        snapshot.bot_reviews = parse_gitlab_bot_reviews(&discussions, &opts.bots);
        snapshot.findings =
            parse_gitlab_findings_for_head(&discussions, &opts.bots, &snapshot.head_oid);
        Ok(snapshot)
    }
}

pub fn create_gitlab_provider() -> GitLabProvider {
    GitLabProvider
}

pub fn parse_gitlab_mr(raw: &Value) -> Result<GitLabMrParseResult, CliError> {
    let web_url = required_string(raw, "web_url")?;
    let project = parse_gitlab_project(&web_url)?;
    Ok(GitLabMrParseResult {
        project_id: required_id(raw, "project_id")?,
        pipeline_id: optional_pipeline_id(raw)?,
        host: project.0,
        snapshot: PrSnapshot {
            number: required_positive_u64(raw, "iid")?,
            title: required_string(raw, "title")?,
            state: gitlab_mr_state(&required_string(raw, "state")?),
            is_draft: raw.get("draft").and_then(Value::as_bool).unwrap_or(false),
            head_ref_name: required_string(raw, "source_branch")?,
            base_ref_name: required_string(raw, "target_branch")?,
            head_oid: required_string(raw, "sha")?,
            head_committed_at: None,
            owner: project.1,
            repo: project.2,
            checks: Vec::new(),
            bot_reviews: Vec::new(),
            findings: Vec::new(),
        },
    })
}

pub fn parse_gitlab_jobs(raw: &Value) -> Vec<PrCheck> {
    raw.as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|job| PrCheck {
            name: job
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            state: gitlab_job_state(job.get("status").and_then(Value::as_str).unwrap_or("")),
        })
        .collect()
}

pub fn parse_gitlab_findings(raw: &[Value], bots: &[String]) -> Vec<crate::core::Finding> {
    parse_gitlab_findings_for_head(raw, bots, "")
}

pub fn parse_gitlab_findings_for_head(
    raw: &[Value],
    bots: &[String],
    head_oid: &str,
) -> Vec<crate::core::Finding> {
    gitlab_threads(raw, head_oid)
        .iter()
        .filter_map(|thread| finding_from_thread(thread, bots))
        .collect()
}

pub fn parse_gitlab_bot_reviews(raw: &[Value], bots: &[String]) -> Vec<BotReview> {
    let mut latest: Vec<(String, BotReview)> = Vec::new();
    for note in top_level_bot_notes(raw, bots) {
        let login = note["author"]["username"].as_str().unwrap_or("");
        let Some(adapter) = adapter_for_login(login, bots) else {
            continue;
        };
        let body = note.get("body").and_then(Value::as_str).unwrap_or("");
        let actionable = adapter.actionable_count(body);
        let review = BotReview {
            bot: adapter.short_name,
            submitted_at: note
                .get("created_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            commit_oid: None,
            actionable,
        };
        upsert_latest(&mut latest, adapter.login, review);
    }
    latest.into_iter().map(|(_, review)| review).collect()
}

fn upsert_latest(latest: &mut Vec<(String, BotReview)>, login: String, review: BotReview) {
    if let Some((_, existing)) = latest.iter_mut().find(|(key, _)| key == &login) {
        if review.submitted_at > existing.submitted_at {
            *existing = review;
        }
        return;
    }
    latest.push((login, review));
}

fn gitlab_threads(discussions: &[Value], head_oid: &str) -> Vec<ReviewThread> {
    discussions
        .iter()
        .filter_map(|discussion| {
            let first = discussion["notes"].as_array()?.first()?;
            if !first
                .get("resolvable")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return None;
            }
            let position = &first["position"];
            let path = position["new_path"]
                .as_str()
                .or_else(|| position["old_path"].as_str())?;
            let line = position["new_line"]
                .as_u64()
                .or_else(|| position["old_line"].as_u64())?;
            Some(ReviewThread {
                path: path.to_string(),
                line: Some(line),
                start_line: None,
                author: first["author"]["username"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
                body: first
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                resolved: first
                    .get("resolved")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                outdated: gitlab_position_outdated(position, head_oid),
            })
        })
        .collect()
}

fn gitlab_position_outdated(position: &Value, head_oid: &str) -> bool {
    !head_oid.is_empty()
        && position
            .get("head_sha")
            .and_then(Value::as_str)
            .is_some_and(|note_head| note_head != head_oid)
}

fn top_level_bot_notes<'a>(discussions: &'a [Value], bots: &[String]) -> Vec<&'a Value> {
    discussions
        .iter()
        .filter_map(|discussion| {
            let first = discussion["notes"].as_array()?.first()?;
            let login = first["author"]["username"].as_str().unwrap_or("");
            if first
                .get("resolvable")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || adapter_for_login(login, bots).is_none()
            {
                None
            } else {
                Some(first)
            }
        })
        .collect()
}

fn gitlab_mr_state(state: &str) -> String {
    match state {
        "opened" => "OPEN".to_string(),
        "merged" => "MERGED".to_string(),
        "closed" => "CLOSED".to_string(),
        _ => state.to_uppercase(),
    }
}

fn gitlab_job_state(status: &str) -> CheckState {
    match status {
        "success" => CheckState::Passed,
        "skipped" | "manual" => CheckState::Skipped,
        "failed" | "canceled" => CheckState::Failed,
        _ => CheckState::Pending,
    }
}

fn parse_gitlab_project(web_url: &str) -> Result<(String, String, String), CliError> {
    let re = Regex::new(r"^https?://([^/]+)/(.+?)/-/merge_requests/").unwrap();
    let Some(caps) = re.captures(web_url) else {
        return Err(parse_json_failure("glab mr view", "invalid web_url"));
    };
    let parts: Vec<&str> = caps.get(2).unwrap().as_str().split('/').collect();
    let repo = parts.last().unwrap_or(&"").to_string();
    if repo.is_empty() || parts.len() < 2 {
        return Err(parse_json_failure("glab mr view", "invalid web_url"));
    }
    Ok((
        caps.get(1).unwrap().as_str().to_string(),
        parts[..parts.len() - 1].join("/"),
        repo,
    ))
}

fn mr_view_args(opts: &SnapshotFetchOptions) -> Vec<String> {
    let mut args = vec!["mr".to_string(), "view".to_string()];
    if let Some(pr) = &opts.pr {
        args.push(pr.clone());
    }
    args.extend(["-F".to_string(), "json".to_string()]);
    if let Some(repo) = &opts.repo {
        args.extend(["-R".to_string(), repo.clone()]);
    }
    args
}

fn fetch_pipeline_jobs(
    project_id: &str,
    pipeline_id: &str,
    host: &str,
    deadline: Option<std::time::Instant>,
) -> Result<Vec<PrCheck>, CliError> {
    let values = run_json_pages(
        |page, per_page| {
            run_json_deadline(
                "glab",
                &jobs_args(project_id, pipeline_id, host, page, per_page),
                &format!("glab api pipeline jobs page {page}"),
                deadline,
            )
        },
        "glab api pipeline jobs",
        100,
    )?;
    Ok(parse_gitlab_jobs(&Value::Array(values)))
}

fn jobs_args(
    project_id: &str,
    pipeline_id: &str,
    host: &str,
    page: usize,
    per_page: usize,
) -> Vec<String> {
    glab_api_args(
        &format!(
            "projects/{project_id}/pipelines/{pipeline_id}/jobs?per_page={per_page}&page={page}"
        ),
        host,
    )
}

fn discussions_args(
    project_id: &str,
    iid: u64,
    host: &str,
    page: usize,
    per_page: usize,
) -> Vec<String> {
    glab_api_args(
        &format!(
            "projects/{project_id}/merge_requests/{iid}/discussions?per_page={per_page}&page={page}"
        ),
        host,
    )
}

fn commit_args(project_id: &str, sha: &str, host: &str) -> Vec<String> {
    glab_api_args(
        &format!("projects/{project_id}/repository/commits/{sha}"),
        host,
    )
}

fn glab_api_args(path: &str, host: &str) -> Vec<String> {
    let mut args = vec!["api".to_string()];
    if !host.is_empty() {
        args.extend(["--hostname".to_string(), host.to_string()]);
    }
    args.push(path.to_string());
    args
}

fn parse_gitlab_mr_json(raw: Value) -> Result<GitLabMrParseResult, CliError> {
    parse_gitlab_mr(&raw)
}

fn required_string(raw: &Value, field: &str) -> Result<String, CliError> {
    raw.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| parse_json_failure("glab mr view", format!("missing or invalid {field}")))
}

fn required_positive_u64(raw: &Value, field: &str) -> Result<u64, CliError> {
    raw.get(field)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| parse_json_failure("glab mr view", format!("missing or invalid {field}")))
}

fn required_id(raw: &Value, field: &str) -> Result<String, CliError> {
    let value = raw
        .get(field)
        .ok_or_else(|| parse_json_failure("glab mr view", format!("missing or invalid {field}")))?;
    match value {
        Value::String(value) if !value.is_empty() => Ok(value.clone()),
        Value::Number(value) if value.as_u64().is_some_and(|id| id > 0) => Ok(value.to_string()),
        _ => Err(parse_json_failure(
            "glab mr view",
            format!("missing or invalid {field}"),
        )),
    }
}

fn optional_pipeline_id(raw: &Value) -> Result<Option<String>, CliError> {
    match raw.get("head_pipeline") {
        None | Some(Value::Null) => Ok(None),
        Some(pipeline) => required_id(pipeline, "id")
            .map(Some)
            .map_err(|_| parse_json_failure("glab mr view", "missing or invalid head_pipeline.id")),
    }
}
