use babysit::bots::DEFAULT_BOTS;
use babysit::cli::{CliOptions, CommandName, parse_args};
use babysit::forge::{ForgeName, detect_forge_from_remote_url};

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn cli_options_remains_constructible_through_its_public_fields() {
    let options = CliOptions {
        command: CommandName::Status,
        pr: None,
        repo: None,
        bots: Vec::new(),
        forge: None,
        all: false,
        nitpicks: false,
        no_reviews: false,
        timeout_secs: 300,
        interval_secs: 30,
        events: false,
        gateway_url: None,
        gateway_token_action: None,
        gateway_webhook_action: None,
    };

    assert_eq!(options.command, CommandName::Status);
}

#[test]
fn parses_gateway_webhook_setup_with_required_repository() {
    let parsed = parse_args(&args(&[
        "gateway-webhook",
        "setup",
        "--repo",
        "example-org/example-repo",
    ]))
    .unwrap();
    assert_eq!(parsed.command, CommandName::GatewayWebhook);
    assert_eq!(parsed.repo.as_deref(), Some("example-org/example-repo"));
    assert_eq!(
        parsed.gateway_webhook_action,
        Some(babysit::cli::GatewayWebhookAction::Setup)
    );

    for values in [
        &["gateway-webhook", "setup"][..],
        &["gateway-webhook", "setup", "--repo", "owner"][..],
        &["gateway-webhook", "setup", "--repo", "owner/repo/extra"][..],
        &["gateway-webhook", "setup", "--repo", "owner/repo", "42"][..],
        &["gateway-webhook", "setup", "--repo", "owner/repo", "--all"][..],
    ] {
        assert!(parse_args(&args(values)).is_err(), "{values:?}");
    }
}

#[test]
fn rejects_dot_path_components_but_allows_dotted_repository_names() {
    for repo in ["owner/.", "owner/..", "./repo", "../repo"] {
        assert!(
            parse_args(&args(&["gateway-webhook", "setup", "--repo", repo])).is_err(),
            "{repo} should be rejected"
        );
    }
    assert!(
        parse_args(&args(&[
            "gateway-webhook",
            "setup",
            "--repo",
            "owner/repo.v2"
        ]))
        .is_ok()
    );
}

#[test]
fn gateway_webhook_setup_help_is_available() {
    assert_eq!(
        parse_args(&args(&["gateway-webhook", "setup", "--help"]))
            .unwrap()
            .command,
        CommandName::Help
    );
}

#[test]
fn event_wait_defaults_to_a_fallback_before_timeout_and_requires_a_gateway_url() {
    let parsed = parse_args(&args(&[
        "wait",
        "--events",
        "--gateway-url",
        "wss://gateway.example",
    ]))
    .unwrap();
    assert!(parsed.events);
    assert_eq!(parsed.gateway_url.as_deref(), Some("wss://gateway.example"));
    assert!(parsed.interval_secs < parsed.timeout_secs);

    let explicit = parse_args(&args(&[
        "wait",
        "--events",
        "--gateway-url=wss://gateway.example",
        "--interval=5",
    ]))
    .unwrap();
    assert_eq!(explicit.interval_secs, 5);
    assert!(parse_args(&args(&["wait", "--events"])).is_err());
    assert!(parse_args(&args(&["wait", "--gateway-url", "wss://gateway.example"])).is_err());
    assert!(
        parse_args(&args(&[
            "status",
            "--events",
            "--gateway-url",
            "wss://gateway.example"
        ]))
        .is_err()
    );
}

#[test]
fn rejects_gitlab_event_mode_before_fetching() {
    assert_eq!(
        babysit::cli::run(&args(&[
            "wait",
            "--forge",
            "gitlab",
            "--events",
            "--gateway-url",
            "wss://gateway.example",
        ])),
        4
    );
}

#[test]
fn parses_gateway_token_actions() {
    let parsed = parse_args(&args(&["gateway-token", "enroll"])).unwrap();
    assert_eq!(parsed.command, CommandName::GatewayToken);
    assert_eq!(
        parsed.gateway_token_action,
        Some(babysit::cli::GatewayTokenAction::Enroll)
    );
    assert!(parse_args(&args(&["gateway-token"])).is_err());
    assert!(parse_args(&args(&["gateway-token", "unknown"])).is_err());
}

#[test]
fn parses_status_defaults() {
    let parsed = parse_args(&args(&["status"])).unwrap();
    assert_eq!(parsed.command, CommandName::Status);
    assert_eq!(parsed.bots, DEFAULT_BOTS.map(str::to_string));
    assert!(!parsed.all);
    assert!(!parsed.nitpicks);
    assert!(!parsed.no_reviews);
    assert_eq!(parsed.timeout_secs, 300);
    assert_eq!(parsed.interval_secs, 30);
}

#[test]
fn wait_defaults_to_a_five_minute_timeout_and_thirty_second_polling() {
    let parsed = parse_args(&args(&["wait"])).unwrap();

    assert_eq!(parsed.timeout_secs, 300);
    assert_eq!(parsed.interval_secs, 30);
}

#[test]
fn parses_pr_number_repo_bot_list_and_findings_flags() {
    let parsed = parse_args(&args(&[
        "findings",
        "63",
        "-R",
        "example-org/example-repo",
        "--bots",
        "coderabbitai,chatgpt-codex-connector",
        "--all",
        "--nitpicks",
    ]))
    .unwrap();
    assert_eq!(parsed.command, CommandName::Findings);
    assert_eq!(parsed.pr.as_deref(), Some("63"));
    assert_eq!(parsed.repo.as_deref(), Some("example-org/example-repo"));
    assert_eq!(
        parsed.bots,
        ["coderabbitai", "chatgpt-codex-connector"].map(str::to_string)
    );
    assert!(parsed.all && parsed.nitpicks);
    assert!(!parsed.no_reviews);
}

#[test]
fn supports_inline_value_flags() {
    let parsed = parse_args(&args(&[
        "wait",
        "--repo=example-org/example-repo",
        "--bots=coderabbitai",
        "--forge=gitlab",
        "--timeout=60",
        "--interval=5",
        "63",
    ]))
    .unwrap();
    assert_eq!(parsed.command, CommandName::Wait);
    assert_eq!(parsed.pr.as_deref(), Some("63"));
    assert_eq!(parsed.forge, Some(ForgeName::GitLab));
    assert_eq!(parsed.timeout_secs, 60);
    assert_eq!(parsed.interval_secs, 5);
}

#[test]
fn rejects_malicious_repo_values_and_meaningless_command_flags() {
    assert!(parse_args(&args(&["status", "--repo=-malicious"])).is_err());
    assert!(parse_args(&args(&["status", "--repo", "-malicious"])).is_err());
    assert!(parse_args(&args(&["status", "--all"])).is_err());
    assert!(parse_args(&args(&["findings", "--no-reviews"])).is_err());
}

#[test]
fn parses_help_and_version_without_contacting_a_forge() {
    for values in [
        &["--help"][..],
        &["-h"][..],
        &["help"][..],
        &["status", "--help"][..],
        &["help", "status"][..],
        &["gateway-token", "--help"][..],
        &["gateway-token", "help"][..],
    ] {
        assert_eq!(
            parse_args(&args(values)).unwrap().command,
            CommandName::Help
        );
        assert_eq!(babysit::cli::run(&args(values)), 0);
    }
    for values in [&["--version"][..], &["-V"][..]] {
        assert_eq!(
            parse_args(&args(values)).unwrap().command,
            CommandName::Version
        );
        assert_eq!(babysit::cli::run(&args(values)), 0);
    }
}

#[test]
fn rejects_unknown_or_missing_subcommands() {
    assert!(
        parse_args(&args(&[]))
            .unwrap_err()
            .to_string()
            .contains("Usage: babysit <COMMAND>")
    );
    assert!(
        parse_args(&args(&["checks"]))
            .unwrap_err()
            .to_string()
            .contains("unrecognized subcommand")
    );
}

#[test]
fn parses_and_rejects_forge_flags() {
    assert_eq!(
        parse_args(&args(&["status", "--forge", "github"]))
            .unwrap()
            .forge,
        Some(ForgeName::GitHub)
    );
    assert_eq!(
        parse_args(&args(&["status", "--forge=gitlab"]))
            .unwrap()
            .forge,
        Some(ForgeName::GitLab)
    );
    assert!(
        parse_args(&args(&["status", "--forge", "bitbucket"]))
            .unwrap_err()
            .to_string()
            .contains("github or gitlab")
    );
}

#[test]
fn rejects_unknown_flags_and_missing_flag_values() {
    assert!(
        parse_args(&args(&["status", "--bad"]))
            .unwrap_err()
            .to_string()
            .contains("unexpected argument")
    );
    assert!(
        parse_args(&args(&["status", "--repo"]))
            .unwrap_err()
            .to_string()
            .contains("a value is required")
    );
}

#[test]
fn rejects_invalid_or_duplicate_pr_numbers() {
    assert!(
        parse_args(&args(&["status", "feature-branch"]))
            .unwrap_err()
            .to_string()
            .contains("invalid PR number")
    );
    assert!(
        parse_args(&args(&["status", "63", "64"]))
            .unwrap_err()
            .to_string()
            .contains("unexpected argument")
    );
}

#[test]
fn rejects_wait_only_flags_outside_wait() {
    assert!(
        parse_args(&args(&["status", "--timeout", "60"]))
            .unwrap_err()
            .to_string()
            .contains("unexpected argument")
    );
    assert!(
        parse_args(&args(&["findings", "--interval=5"]))
            .unwrap_err()
            .to_string()
            .contains("unexpected argument")
    );
}

#[test]
fn rejects_empty_bot_lists_and_non_positive_intervals() {
    assert!(
        parse_args(&args(&["status", "--bots="]))
            .unwrap_err()
            .to_string()
            .contains("at least one bot")
    );
    assert!(
        parse_args(&args(&["wait", "--interval", "0"]))
            .unwrap_err()
            .to_string()
            .contains("between 1")
    );
}

#[test]
fn rejects_oversized_wait_durations() {
    assert!(
        parse_args(&args(&["wait", "--timeout", "18446744073709551615"]))
            .unwrap_err()
            .to_string()
            .contains("between 1")
    );
}

#[test]
fn selects_gitlab_when_origin_host_contains_gitlab() {
    assert_eq!(
        detect_forge_from_remote_url(Some("https://gitlab.com/group/project.git")),
        ForgeName::GitLab
    );
    assert_eq!(
        detect_forge_from_remote_url(Some("git@gitlab.example.com:group/project.git")),
        ForgeName::GitLab
    );
}

#[test]
fn defaults_to_github_for_non_gitlab_or_missing_origins() {
    assert_eq!(
        detect_forge_from_remote_url(Some("https://github.com/org/repo.git")),
        ForgeName::GitHub
    );
    assert_eq!(detect_forge_from_remote_url(None), ForgeName::GitHub);
}
