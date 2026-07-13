use std::collections::VecDeque;

use babysit::forge::CliError;
use babysit::github_webhook::{GhClient, SetupAction, WebhookSecret, setup_webhook};
use serde_json::{Value, json};

const WEBHOOK_URL: &str = "https://babysit.mindgoblin.pw/webhooks/github";

struct FakeGh {
    reads: VecDeque<Result<Value, CliError>>,
    mutations: Vec<Vec<u8>>,
    argv: Vec<Vec<String>>,
    mutation_result: Option<Result<Value, CliError>>,
}

impl GhClient for FakeGh {
    fn get_json(&mut self, args: &[String]) -> Result<Value, CliError> {
        self.argv.push(args.to_vec());
        self.reads.pop_front().expect("fake response")
    }

    fn mutate_json(&mut self, args: &[String], body: &[u8]) -> Result<Value, CliError> {
        self.argv.push(args.to_vec());
        self.mutations.push(body.to_vec());
        self.mutation_result
            .take()
            .unwrap_or_else(|| Ok(json!({"id": 7})))
    }
}

fn hook(id: u64, url: &str) -> Value {
    hook_with(id, url, "web", true, "json", json!("0"), standard_events())
}

fn hook_with(
    id: u64,
    url: &str,
    name: &str,
    active: bool,
    content_type: &str,
    insecure_ssl: Value,
    events: Value,
) -> Value {
    json!({
        "id": id,
        "name": name,
        "active": active,
        "config": {
            "url": url,
            "content_type": content_type,
            "insecure_ssl": insecure_ssl
        },
        "events": events
    })
}

fn standard_events() -> Value {
    json!([
        "check_run",
        "check_suite",
        "status",
        "pull_request",
        "pull_request_review",
        "pull_request_review_comment",
        "pull_request_review_thread",
        "issue_comment"
    ])
}

#[test]
fn creates_exact_payload_using_only_mutation_stdin_for_secret() {
    let mut gh = FakeGh {
        reads: VecDeque::from([
            Ok(json!([])),
            Ok(json!([hook(
                7,
                "https://babysit.mindgoblin.pw/webhooks/github"
            )])),
        ]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: None,
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    let result = setup_webhook("owner/repo", &secret, &mut gh).unwrap();

    assert_eq!(result.action, SetupAction::Created);
    assert_eq!(gh.mutations.len(), 1);
    let payload: Value = serde_json::from_slice(&gh.mutations[0]).unwrap();
    assert_eq!(payload["config"]["secret"], "sentinel-secret");
    assert_eq!(
        payload["config"]["url"],
        "https://babysit.mindgoblin.pw/webhooks/github"
    );
    assert_eq!(payload["config"]["content_type"], "json");
    assert_eq!(payload["config"]["insecure_ssl"], "0");
    assert_eq!(payload["events"], standard_events());
    assert_eq!(gh.argv[1][0], "api");
    assert_eq!(gh.argv[1][1], "--method");
    assert_eq!(gh.argv[1][2], "POST");
    assert_eq!(gh.argv[1][3], "repos/owner/repo/hooks");
    assert_eq!(gh.argv[1][4], "--input");
    assert_eq!(gh.argv[1][5], "-");
    assert!(
        gh.argv[1]
            .iter()
            .all(|argument| !argument.contains("sentinel-secret"))
    );
}

#[test]
fn accepts_numeric_insecure_ssl_zero_during_reconciliation() {
    let numeric_hook = hook_with(
        7,
        WEBHOOK_URL,
        "web",
        true,
        "json",
        json!(0),
        standard_events(),
    );
    let mut gh = FakeGh {
        reads: VecDeque::from([Ok(json!([])), Ok(json!([numeric_hook]))]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: None,
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    setup_webhook("owner/repo", &secret, &mut gh).unwrap();
}

#[test]
fn updates_the_single_matching_web_hook() {
    let url = "https://babysit.mindgoblin.pw/webhooks/github";
    let mut gh = FakeGh {
        reads: VecDeque::from([Ok(json!([hook(12, url)])), Ok(json!([hook(12, url)]))]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: None,
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    let result = setup_webhook("owner/repo", &secret, &mut gh).unwrap();

    assert_eq!(result.action, SetupAction::Updated);
    assert!(gh.argv[1].iter().any(|arg| arg == "PATCH"));
    assert!(
        gh.argv[1]
            .iter()
            .any(|arg| arg == "repos/owner/repo/hooks/12")
    );
}

#[test]
fn searches_a_second_page_after_a_full_unrelated_first_page() {
    let first_page = (0..100)
        .map(|id| hook(id, "https://example.com/other-hook"))
        .collect::<Vec<_>>();
    let matching = hook(101, WEBHOOK_URL);
    let mut gh = FakeGh {
        reads: VecDeque::from([
            Ok(Value::Array(first_page)),
            Ok(json!([matching.clone()])),
            Ok(json!([matching])),
        ]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: None,
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    setup_webhook("owner/repo", &secret, &mut gh).unwrap();

    assert_eq!(gh.argv[0][1], "repos/owner/repo/hooks?per_page=100&page=1");
    assert_eq!(gh.argv[1][1], "repos/owner/repo/hooks?per_page=100&page=2");
}

#[test]
fn reordered_events_are_accepted_during_reconciliation() {
    let mut gh = FakeGh {
        reads: VecDeque::from([
            Ok(json!([])),
            Ok(json!([hook_with(
                7,
                WEBHOOK_URL,
                "web",
                true,
                "json",
                json!("0"),
                json!([
                    "issue_comment",
                    "pull_request_review_thread",
                    "pull_request_review_comment",
                    "pull_request_review",
                    "pull_request",
                    "status",
                    "check_suite",
                    "check_run"
                ]),
            )])),
        ]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: None,
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    setup_webhook("owner/repo", &secret, &mut gh).unwrap();
}

#[test]
fn incorrect_event_membership_is_rejected_during_reconciliation() {
    let mut gh = FakeGh {
        reads: VecDeque::from([
            Ok(json!([])),
            Ok(json!([hook_with(
                7,
                WEBHOOK_URL,
                "web",
                true,
                "json",
                json!("0"),
                json!([
                    "check_run",
                    "check_suite",
                    "status",
                    "pull_request",
                    "pull_request_review",
                    "pull_request_review_comment",
                    "pull_request_review_thread",
                    "unexpected_event"
                ]),
            )])),
        ]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: None,
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    let error = setup_webhook("owner/repo", &secret, &mut gh).unwrap_err();

    assert!(error.message.contains("reconciliation failed"));
    assert_eq!(gh.mutations.len(), 1);
}

#[test]
fn duplicate_events_are_rejected_during_reconciliation() {
    let mut gh = FakeGh {
        reads: VecDeque::from([
            Ok(json!([])),
            Ok(json!([hook_with(
                7,
                WEBHOOK_URL,
                "web",
                true,
                "json",
                json!("0"),
                json!([
                    "check_run",
                    "check_run",
                    "status",
                    "pull_request",
                    "pull_request_review",
                    "pull_request_review_comment",
                    "pull_request_review_thread",
                    "issue_comment"
                ]),
            )])),
        ]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: None,
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    assert!(setup_webhook("owner/repo", &secret, &mut gh).is_err());
    assert_eq!(gh.mutations.len(), 1);
}

#[test]
fn unexpected_name_at_exact_url_conflicts_without_mutating() {
    let mut gh = FakeGh {
        reads: VecDeque::from([Ok(json!([hook_with(
            1,
            WEBHOOK_URL,
            "unexpected",
            true,
            "json",
            json!("0"),
            standard_events(),
        )]))]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: None,
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    let error = setup_webhook("owner/repo", &secret, &mut gh).unwrap_err();

    assert!(error.message.contains("unexpected hook name"));
    assert!(gh.mutations.is_empty());
}

#[test]
fn malformed_hook_json_fails_without_mutating() {
    let mut gh = FakeGh {
        reads: VecDeque::from([Ok(json!([{"id": 1}]))]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: None,
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    let error = setup_webhook("owner/repo", &secret, &mut gh).unwrap_err();

    assert!(error.message.contains("malformed GitHub hook JSON"));
    assert!(gh.mutations.is_empty());
}

#[test]
fn gh_auth_get_error_propagates_without_mutating() {
    let mut gh = FakeGh {
        reads: VecDeque::from([Err(CliError::new("gh auth login required", false))]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: None,
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    let error = setup_webhook("owner/repo", &secret, &mut gh).unwrap_err();

    assert_eq!(error.message, "gh auth login required");
    assert!(gh.mutations.is_empty());
}

#[test]
fn conflicts_do_not_mutate() {
    let url = "https://babysit.mindgoblin.pw/webhooks/github";
    let mut gh = FakeGh {
        reads: VecDeque::from([Ok(json!([hook(1, url), hook(2, url)]))]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: None,
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    let error = setup_webhook("owner/repo", &secret, &mut gh).unwrap_err();

    assert!(error.message.contains("conflict"));
    assert!(error.message.contains("no mutation"));
    assert!(gh.mutations.is_empty());
}

#[test]
fn mutation_errors_redact_raw_and_json_escaped_secrets() {
    let secret_value = r#"quote\"slash\\secret"#;
    let escaped_secret = serde_json::to_string(secret_value).unwrap();
    let detail = format!("raw={secret_value}; serialized={escaped_secret}; timeout");
    let mut gh = FakeGh {
        reads: VecDeque::from([Ok(json!([]))]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: Some(Err(CliError::new(detail, true))),
    };
    let secret = WebhookSecret::new(secret_value.to_string()).unwrap();

    let error = setup_webhook("owner/repo", &secret, &mut gh).unwrap_err();

    assert!(error.message.contains("state may have changed"));
    assert!(error.message.contains("rerunning"));
    assert!(!error.message.contains(secret_value));
    assert!(!error.message.contains(&escaped_secret));
    assert!(
        !error
            .message
            .contains(&escaped_secret[1..escaped_secret.len() - 1])
    );
    assert_eq!(gh.argv[1][0], "api");
    assert_eq!(gh.argv[1][1], "--method");
    assert!(
        gh.argv[1]
            .iter()
            .all(|argument| !argument.contains(secret_value))
    );
    assert!(
        gh.argv[1]
            .iter()
            .all(|argument| !argument.contains(&escaped_secret))
    );
}

#[test]
fn unexpected_non_secret_state_is_rejected_after_mutation() {
    let inactive_hook = hook_with(
        7,
        WEBHOOK_URL,
        "web",
        false,
        "json",
        json!("0"),
        standard_events(),
    );
    let mut gh = FakeGh {
        reads: VecDeque::from([Ok(json!([])), Ok(json!([inactive_hook]))]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: None,
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    let error = setup_webhook("owner/repo", &secret, &mut gh).unwrap_err();

    assert!(error.message.contains("unexpected state"));
    assert_eq!(gh.mutations.len(), 1);
}

#[test]
fn reconciliation_failure_is_reported_after_mutation() {
    let mut gh = FakeGh {
        reads: VecDeque::from([Ok(json!([])), Ok(json!([]))]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: None,
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    let error = setup_webhook("owner/repo", &secret, &mut gh).unwrap_err();

    assert!(error.message.contains("reconciliation failed"));
    assert_eq!(gh.mutations.len(), 1);
}
