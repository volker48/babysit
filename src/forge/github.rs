use std::collections::HashSet;

use regex::Regex;
use serde_json::Value;

use crate::bots::adapter_for_login;
use crate::core::{
    BotReview, CheckState, PrCheck, PrSnapshot, ReviewData, ReviewThread, findings_from_threads,
};
use crate::forge::{
    CliError, ForgeProvider, SnapshotFetchOptions, parse_json_failure, run_json_deadline,
};

const MAX_REVIEW_PAGES: usize = 100;

pub const REVIEW_QUERY: &str = r#"
query(
  $owner: String!,
  $name: String!,
  $number: Int!,
  $reviewsCursor: String,
  $reviewThreadsCursor: String
) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviews(first: 100, after: $reviewsCursor) {
        pageInfo {
          hasNextPage
          endCursor
        }
        nodes {
          author { login }
          state
          submittedAt
          body
          commit { oid }
        }
      }
      reviewThreads(first: 100, after: $reviewThreadsCursor) {
        pageInfo {
          hasNextPage
          endCursor
        }
        nodes {
          isResolved
          isOutdated
          path
          line
          startLine
          comments(first: 5) {
            nodes {
              author { login }
              body
            }
          }
        }
      }
    }
  }
}
"#;

pub struct GitHubProvider;

impl ForgeProvider for GitHubProvider {
    fn fetch_snapshot(&self, opts: &SnapshotFetchOptions) -> Result<PrSnapshot, CliError> {
        let mut snapshot = parse_pr_view_json(run_json_deadline(
            "gh",
            &pr_view_args(opts),
            "gh pr view",
            opts.deadline,
        )?)?;
        let reviews = fetch_review_data(&snapshot, &opts.bots, opts.deadline)?;
        snapshot.bot_reviews = reviews.bot_reviews;
        snapshot.findings = if opts.nitpicks {
            reviews
                .findings
                .into_iter()
                .chain(reviews.nitpicks)
                .collect()
        } else {
            reviews.findings
        };
        Ok(snapshot)
    }
}

pub fn create_github_provider() -> GitHubProvider {
    GitHubProvider
}

pub fn parse_pr_view(raw: &Value) -> Result<PrSnapshot, String> {
    let url = raw.get("url").and_then(Value::as_str).unwrap_or("");
    let re = Regex::new(r"github\.com/([^/]+)/([^/]+)/pull/").unwrap();
    let caps = re
        .captures(url)
        .ok_or_else(|| format!("cannot derive owner/repo from PR url: {url}"))?;
    let commits = raw
        .get("commits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let last_commit = commits
        .last()
        .and_then(|v| v.get("committedDate"))
        .and_then(Value::as_str);
    Ok(PrSnapshot {
        number: required_u64(raw, "number")?,
        title: required_str(raw, "title")?,
        state: required_str(raw, "state")?,
        is_draft: raw.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        head_ref_name: required_str(raw, "headRefName")?,
        base_ref_name: required_str(raw, "baseRefName")?,
        head_oid: required_str(raw, "headRefOid")?,
        head_committed_at: last_commit.map(str::to_string),
        owner: caps.get(1).unwrap().as_str().to_string(),
        repo: caps.get(2).unwrap().as_str().to_string(),
        checks: parse_github_checks(raw.get("statusCheckRollup").and_then(Value::as_array)),
        bot_reviews: Vec::new(),
        findings: Vec::new(),
    })
}

pub fn parse_review_data(raw: &Value, bots: &[String]) -> ReviewData {
    parse_review_data_for_head(raw, bots, "", None)
}

pub fn parse_review_data_for_head(
    raw: &Value,
    bots: &[String],
    head_oid: &str,
    head_committed_at: Option<&str>,
) -> ReviewData {
    let pr = &raw["data"]["repository"]["pullRequest"];
    let reviews = pr["reviews"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let bot_reviews = reviews
        .iter()
        .filter_map(|review| parse_bot_review(review, bots))
        .collect();
    let nitpicks = reviews
        .iter()
        .filter(|review| review_matches_current_head(review, head_oid, head_committed_at))
        .flat_map(|review| parse_review_nitpicks(review, bots))
        .collect();
    let findings = findings_from_threads(&github_threads(pr), bots);
    ReviewData {
        bot_reviews,
        findings,
        nitpicks,
    }
}

fn parse_bot_review(review: &Value, bots: &[String]) -> Option<BotReview> {
    let login = review["author"]["login"].as_str().unwrap_or("");
    let adapter = adapter_for_login(login, bots)?;
    let body = review.get("body").and_then(Value::as_str).unwrap_or("");
    let actionable = adapter.actionable_count(body);
    Some(BotReview {
        bot: adapter.short_name,
        submitted_at: review.get("submittedAt")?.as_str()?.to_string(),
        commit_oid: review["commit"]["oid"].as_str().map(str::to_string),
        actionable,
    })
}

fn parse_review_nitpicks(review: &Value, bots: &[String]) -> Vec<crate::core::Finding> {
    let login = review["author"]["login"].as_str().unwrap_or("");
    let Some(adapter) = adapter_for_login(login, bots) else {
        return Vec::new();
    };
    adapter.review_body_findings(review.get("body").and_then(Value::as_str).unwrap_or(""))
}

fn review_matches_current_head(
    review: &Value,
    head_oid: &str,
    head_committed_at: Option<&str>,
) -> bool {
    if head_oid.is_empty() && head_committed_at.is_none() {
        return true;
    }
    if review["commit"]["oid"].as_str() == Some(head_oid) {
        return true;
    }
    match (
        review.get("submittedAt").and_then(Value::as_str),
        head_committed_at,
    ) {
        (Some(submitted_at), Some(committed_at)) => submitted_at >= committed_at,
        _ => false,
    }
}

fn parse_github_checks(checks: Option<&Vec<Value>>) -> Vec<PrCheck> {
    checks
        .unwrap_or(&Vec::new())
        .iter()
        .map(|check| {
            if check.get("__typename").and_then(Value::as_str) == Some("StatusContext") {
                return PrCheck {
                    name: check
                        .get("context")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    state: status_context_state(
                        check.get("state").and_then(Value::as_str).unwrap_or(""),
                    ),
                };
            }
            PrCheck {
                name: check
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                state: check_run_state(
                    check.get("status").and_then(Value::as_str).unwrap_or(""),
                    check.get("conclusion").and_then(Value::as_str),
                ),
            }
        })
        .collect()
}

fn github_threads(pr: &Value) -> Vec<ReviewThread> {
    let nodes = pr["reviewThreads"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    nodes
        .iter()
        .filter_map(|thread| {
            let first = thread["comments"]["nodes"].as_array()?.first()?;
            Some(ReviewThread {
                path: thread.get("path")?.as_str()?.to_string(),
                line: thread.get("line").and_then(Value::as_u64),
                start_line: thread.get("startLine").and_then(Value::as_u64),
                author: first["author"]["login"].as_str().unwrap_or("").to_string(),
                body: first
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                resolved: thread
                    .get("isResolved")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                outdated: thread
                    .get("isOutdated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn check_run_state(status: &str, conclusion: Option<&str>) -> CheckState {
    if status != "COMPLETED" {
        return CheckState::Pending;
    }
    match conclusion {
        Some("SUCCESS" | "NEUTRAL") => CheckState::Passed,
        Some("SKIPPED") => CheckState::Skipped,
        _ => CheckState::Failed,
    }
}

fn status_context_state(state: &str) -> CheckState {
    match state {
        "SUCCESS" => CheckState::Passed,
        "PENDING" | "EXPECTED" => CheckState::Pending,
        _ => CheckState::Failed,
    }
}

fn pr_view_args(opts: &SnapshotFetchOptions) -> Vec<String> {
    let mut args = vec!["pr".to_string(), "view".to_string()];
    if let Some(pr) = &opts.pr {
        args.push(pr.clone());
    }
    if let Some(repo) = &opts.repo {
        args.extend(["-R".to_string(), repo.clone()]);
    }
    args.extend(["--json".to_string(), pr_view_fields().join(",")]);
    args
}

fn pr_view_fields() -> Vec<&'static str> {
    vec![
        "number",
        "title",
        "state",
        "isDraft",
        "headRefName",
        "baseRefName",
        "headRefOid",
        "statusCheckRollup",
        "commits",
        "url",
    ]
}

fn review_args(
    snapshot: &PrSnapshot,
    reviews_cursor: Option<&str>,
    review_threads_cursor: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "api".to_string(),
        "graphql".to_string(),
        "-f".to_string(),
        format!("query={}", REVIEW_QUERY.trim()),
        "-F".to_string(),
        format!("owner={}", snapshot.owner),
        "-F".to_string(),
        format!("name={}", snapshot.repo),
        "-F".to_string(),
        format!("number={}", snapshot.number),
    ];
    if let Some(cursor) = reviews_cursor {
        args.extend(["-F".to_string(), format!("reviewsCursor={cursor}")]);
    }
    if let Some(cursor) = review_threads_cursor {
        args.extend(["-F".to_string(), format!("reviewThreadsCursor={cursor}")]);
    }
    args
}

fn fetch_review_data(
    snapshot: &PrSnapshot,
    bots: &[String],
    deadline: Option<std::time::Instant>,
) -> Result<ReviewData, CliError> {
    let first = run_json_deadline(
        "gh",
        &review_args(snapshot, None, None),
        "gh api graphql",
        deadline,
    )?;
    let pages = collect_review_pages(first, |reviews_cursor, review_threads_cursor| {
        run_json_deadline(
            "gh",
            &review_args(snapshot, reviews_cursor, review_threads_cursor),
            "gh api graphql",
            deadline,
        )
    })?;
    Ok(parse_review_data_for_head(
        &pages,
        bots,
        &snapshot.head_oid,
        snapshot.head_committed_at.as_deref(),
    ))
}

/// Validates and combines paged GitHub review and review-thread GraphQL responses.
pub fn collect_review_pages<F>(mut first: Value, mut fetch_page: F) -> Result<Value, CliError>
where
    F: FnMut(Option<&str>, Option<&str>) -> Result<Value, CliError>,
{
    validate_review_response(&first, ReviewValidation::all())?;
    let mut reviews_pages = 1;
    let mut review_threads_pages = 1;
    let mut seen_review_cursors = HashSet::new();
    let mut seen_review_thread_cursors = HashSet::new();
    loop {
        let reviews_cursor =
            next_cursor(&first, "reviews", &mut seen_review_cursors, reviews_pages)?;
        let review_threads_cursor = next_cursor(
            &first,
            "reviewThreads",
            &mut seen_review_thread_cursors,
            review_threads_pages,
        )?;
        if reviews_cursor.is_none() && review_threads_cursor.is_none() {
            return Ok(first);
        }
        let next = fetch_page(reviews_cursor.as_deref(), review_threads_cursor.as_deref())?;
        let validation = ReviewValidation {
            reviews: reviews_cursor.is_some(),
            review_threads: review_threads_cursor.is_some(),
        };
        validate_review_response(&next, validation)?;
        append_review_connections(&mut first, &next, validation);
        reviews_pages += usize::from(validation.reviews);
        review_threads_pages += usize::from(validation.review_threads);
    }
}

#[derive(Clone, Copy)]
struct ReviewValidation {
    reviews: bool,
    review_threads: bool,
}

impl ReviewValidation {
    fn all() -> Self {
        Self {
            reviews: true,
            review_threads: true,
        }
    }
}

fn next_cursor(
    page: &Value,
    connection: &str,
    seen_cursors: &mut HashSet<String>,
    pages: usize,
) -> Result<Option<String>, CliError> {
    let (has_next_page, cursor) = review_page_info(page, connection)?;
    if !has_next_page {
        return Ok(None);
    }
    if pages >= MAX_REVIEW_PAGES {
        return Err(parse_json_failure(
            "gh api graphql pagination",
            format!("{connection} exceeded {MAX_REVIEW_PAGES} pages"),
        ));
    }
    let cursor = cursor.expect("advancing review page has a cursor");
    if !seen_cursors.insert(cursor.to_string()) {
        return Err(parse_json_failure(
            "gh api graphql pagination",
            format!("{connection} cursor did not advance"),
        ));
    }
    Ok(Some(cursor.to_string()))
}

fn review_page_info<'a>(
    page: &'a Value,
    connection: &str,
) -> Result<(bool, Option<&'a str>), CliError> {
    let page_info = review_connection_page_info(page, connection)?;
    let has_next_page = page_info
        .get("hasNextPage")
        .and_then(Value::as_bool)
        .ok_or_else(|| review_shape_error(&format!("{connection}.pageInfo.hasNextPage")))?;
    let cursor = page_info.get("endCursor").and_then(Value::as_str);
    if has_next_page && cursor.is_none_or(str::is_empty) {
        return Err(review_shape_error(&format!(
            "{connection}.pageInfo.endCursor"
        )));
    }
    Ok((has_next_page, cursor))
}

fn append_review_connections(first: &mut Value, next: &Value, validation: ReviewValidation) {
    if validation.reviews {
        append_review_connection(first, next, "reviews");
    }
    if validation.review_threads {
        append_review_connection(first, next, "reviewThreads");
    }
}

fn append_review_connection(first: &mut Value, next: &Value, connection: &str) {
    let dst = first["data"]["repository"]["pullRequest"][connection]["nodes"]
        .as_array_mut()
        .expect("review page was validated");
    let src = next["data"]["repository"]["pullRequest"][connection]["nodes"]
        .as_array()
        .expect("review page was validated");
    dst.extend(src.iter().cloned());
    first["data"]["repository"]["pullRequest"][connection]["pageInfo"] =
        next["data"]["repository"]["pullRequest"][connection]["pageInfo"].clone();
}

fn validate_review_response(raw: &Value, validation: ReviewValidation) -> Result<(), CliError> {
    let pr = review_pull_request(raw)?;
    if validation.reviews {
        validate_reviews(pr)?;
    }
    if validation.review_threads {
        validate_review_threads(pr)?;
    }
    Ok(())
}

fn validate_reviews(pr: &Value) -> Result<(), CliError> {
    let reviews = require_object(&pr["reviews"], "reviews")?;
    require_page_info(&reviews["pageInfo"], "reviews.pageInfo")?;
    require_array(&reviews["nodes"], "reviews.nodes")?;
    Ok(())
}

fn validate_review_threads(pr: &Value) -> Result<(), CliError> {
    let threads = require_object(&pr["reviewThreads"], "reviewThreads")?;
    require_page_info(&threads["pageInfo"], "reviewThreads.pageInfo")?;
    let nodes = require_array(&threads["nodes"], "reviewThreads.nodes")?;
    for (index, node) in nodes.iter().enumerate() {
        validate_review_thread(node, index)?;
    }
    Ok(())
}

fn review_pull_request(raw: &Value) -> Result<&Value, CliError> {
    let pr = &raw["data"]["repository"]["pullRequest"];
    require_object(pr, "data.repository.pullRequest")
}

fn review_connection_page_info<'a>(
    raw: &'a Value,
    connection: &str,
) -> Result<&'a Value, CliError> {
    let pr = review_pull_request(raw)?;
    require_object(
        &pr[connection]["pageInfo"],
        &format!("{connection}.pageInfo"),
    )
}

fn require_page_info(page_info: &Value, context: &str) -> Result<(), CliError> {
    require_object(page_info, context)?;
    let has_next = page_info["hasNextPage"]
        .as_bool()
        .ok_or_else(|| review_shape_error(&format!("{context}.hasNextPage")))?;
    match (has_next, page_info.get("endCursor")) {
        (true, Some(Value::String(cursor))) if !cursor.is_empty() => Ok(()),
        (false, Some(Value::String(_) | Value::Null)) => Ok(()),
        _ => Err(review_shape_error(&format!("{context}.endCursor"))),
    }
}

fn require_nullable_u64(value: Option<&Value>, field: &str) -> Result<(), CliError> {
    match value {
        Some(Value::Number(number)) if number.as_u64().is_some() => Ok(()),
        Some(Value::Null) => Ok(()),
        _ => Err(review_shape_error(field)),
    }
}

fn validate_review_thread(thread: &Value, index: usize) -> Result<(), CliError> {
    let context = format!("reviewThreads.nodes[{index}]");
    require_object(thread, &context)?;
    require_bool(&thread["isResolved"], &format!("{context}.isResolved"))?;
    require_bool(&thread["isOutdated"], &format!("{context}.isOutdated"))?;
    require_str(&thread["path"], &format!("{context}.path"))?;
    require_nullable_u64(thread.get("line"), &format!("{context}.line"))?;
    require_nullable_u64(thread.get("startLine"), &format!("{context}.startLine"))?;
    let comments = require_array(
        &thread["comments"]["nodes"],
        &format!("{context}.comments.nodes"),
    )?;
    for (comment_index, comment) in comments.iter().enumerate() {
        validate_review_comment(comment, &context, comment_index)?;
    }
    Ok(())
}

fn validate_review_comment(comment: &Value, context: &str, index: usize) -> Result<(), CliError> {
    let context = format!("{context}.comments.nodes[{index}]");
    require_object(comment, &context)?;
    require_str(
        &comment["author"]["login"],
        &format!("{context}.author.login"),
    )?;
    require_str(&comment["body"], &format!("{context}.body"))?;
    Ok(())
}

fn require_object<'a>(value: &'a Value, field: &str) -> Result<&'a Value, CliError> {
    if value.is_object() {
        Ok(value)
    } else {
        Err(review_shape_error(field))
    }
}

fn require_array<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>, CliError> {
    value.as_array().ok_or_else(|| review_shape_error(field))
}

fn require_bool(value: &Value, field: &str) -> Result<(), CliError> {
    value
        .as_bool()
        .map(|_| ())
        .ok_or_else(|| review_shape_error(field))
}

fn require_str(value: &Value, field: &str) -> Result<(), CliError> {
    value
        .as_str()
        .map(|_| ())
        .ok_or_else(|| review_shape_error(field))
}

fn review_shape_error(field: &str) -> CliError {
    parse_json_failure("gh api graphql", format!("missing or invalid {field}"))
}

fn parse_pr_view_json(raw: Value) -> Result<PrSnapshot, CliError> {
    parse_pr_view(&raw)
        .map_err(|error| parse_json_failure("could not parse gh pr view output", error))
}

fn required_str(raw: &Value, field: &str) -> Result<String, String> {
    raw.get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing {field}"))
}

fn required_u64(raw: &Value, field: &str) -> Result<u64, String> {
    raw.get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn review_page(cursor: &str, has_next_page: bool) -> Value {
        json!({
            "data": {"repository": {"pullRequest": {
                "reviews": {
                    "nodes": [],
                    "pageInfo": {
                        "hasNextPage": false,
                        "endCursor": null
                    }
                },
                "reviewThreads": {
                    "nodes": [],
                    "pageInfo": {
                        "hasNextPage": has_next_page,
                        "endCursor": cursor
                    }
                }
            }}}
        })
    }

    fn review_page_with_review_cursor(cursor: &str, has_next_page: bool) -> Value {
        let mut page = review_page("", false);
        page["data"]["repository"]["pullRequest"]["reviews"]["pageInfo"] = json!({
            "hasNextPage": has_next_page,
            "endCursor": cursor
        });
        page
    }

    fn review_thread(path: &str) -> Value {
        json!({
            "isResolved": false,
            "isOutdated": false,
            "path": path,
            "line": 1,
            "startLine": null,
            "comments": {"nodes": [{"author": {"login": "coderabbitai"}, "body": "body"}]}
        })
    }

    #[test]
    fn review_thread_pagination_aggregates_finite_pages() {
        let mut first = review_page("next", true);
        first["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"] =
            json!([review_thread("first")]);
        let pages = collect_review_pages(first, |reviews_cursor, review_threads_cursor| {
            assert!(reviews_cursor.is_none());
            assert_eq!(review_threads_cursor, Some("next"));
            let mut next = review_page("done", false);
            next["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"] =
                json!([review_thread("second")]);
            Ok(next)
        })
        .unwrap();

        let nodes = pages["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"]
            .as_array()
            .unwrap();
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn review_thread_pagination_rejects_malformed_pages() {
        let initial_error =
            collect_review_pages(json!({"bad": true}), |_, _| unreachable!()).unwrap_err();
        assert!(
            initial_error
                .to_string()
                .contains("data.repository.pullRequest")
        );

        let first = review_page("next", true);
        let next_error = collect_review_pages(first, |_, _| Ok(json!({"bad": true}))).unwrap_err();
        assert!(
            next_error
                .to_string()
                .contains("data.repository.pullRequest")
        );
    }

    #[test]
    fn review_thread_pagination_rejects_a_repeated_cursor() {
        let first = review_page("same", true);
        let error = collect_review_pages(first, |_, _| Ok(review_page("same", true))).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("reviewThreads cursor did not advance")
        );
    }

    #[test]
    fn review_thread_pagination_rejects_a_cursor_cycle() {
        let first = review_page("a", true);
        let mut cursors = ["b", "a"].into_iter();
        let error = collect_review_pages(first, |_, _| {
            Ok(review_page(cursors.next().unwrap_or("a"), true))
        })
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("reviewThreads cursor did not advance")
        );
    }

    #[test]
    fn review_thread_pagination_has_a_hard_page_cap() {
        let first = review_page("cursor-1", true);
        let mut page = 1;
        let error = collect_review_pages(first, |_, _| {
            page += 1;
            Ok(review_page(&format!("cursor-{page}"), true))
        })
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("reviewThreads exceeded 100 pages")
        );
        assert_eq!(page, 100);
    }

    #[test]
    fn review_pagination_rejects_a_cursor_cycle() {
        let first = review_page_with_review_cursor("a", true);
        let mut cursors = ["b", "a"].into_iter();
        let error = collect_review_pages(first, |reviews_cursor, review_threads_cursor| {
            assert!(reviews_cursor.is_some());
            assert!(review_threads_cursor.is_none());
            Ok(review_page_with_review_cursor(
                cursors.next().unwrap_or("a"),
                true,
            ))
        })
        .unwrap_err();

        assert!(error.to_string().contains("reviews cursor did not advance"));
    }

    #[test]
    fn review_pagination_has_a_hard_page_cap() {
        let first = review_page_with_review_cursor("cursor-1", true);
        let mut page = 1;
        let error = collect_review_pages(first, |reviews_cursor, review_threads_cursor| {
            assert!(reviews_cursor.is_some());
            assert!(review_threads_cursor.is_none());
            page += 1;
            Ok(review_page_with_review_cursor(
                &format!("cursor-{page}"),
                true,
            ))
        })
        .unwrap_err();

        assert!(error.to_string().contains("reviews exceeded 100 pages"));
        assert_eq!(page, 100);
    }
}
