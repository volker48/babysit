use std::collections::VecDeque;

use babysit::forge::CliError;
use babysit::github_webhook::{GhClient, SetupAction, WebhookSecret, setup_webhook};
use serde_json::{Value, json};

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
    json!({
        "id": id,
        "name": "web",
        "active": true,
        "config": {
            "url": url,
            "content_type": "json",
            "insecure_ssl": "0"
        },
        "events": [
            "check_run", "check_suite", "status", "pull_request",
            "pull_request_review", "pull_request_review_comment",
            "pull_request_review_thread", "issue_comment"
        ]
    })
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
    assert_eq!(
        payload["events"],
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
    );
    assert!(gh.argv[1].contains(&"--input".to_string()));
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
fn mutation_errors_are_ambiguous_and_redact_the_secret() {
    let mut gh = FakeGh {
        reads: VecDeque::from([Ok(json!([]))]),
        mutations: Vec::new(),
        argv: Vec::new(),
        mutation_result: Some(Err(CliError::new("sentinel-secret: timeout", true))),
    };
    let secret = WebhookSecret::new("sentinel-secret".to_string()).unwrap();

    let error = setup_webhook("owner/repo", &secret, &mut gh).unwrap_err();

    assert!(error.message.contains("state may have changed"));
    assert!(error.message.contains("rerunning"));
    assert!(!error.message.contains("sentinel-secret"));
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
