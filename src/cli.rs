use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};

const DEFAULT_WAIT_SECONDS: u64 = 5 * 60;
const MAX_WAIT_SECONDS: u64 = 30 * 24 * 60 * 60;

use crate::bots::DEFAULT_BOTS;
use crate::core::{
    Finding, PrSnapshot, SettleOptions, SettleResult, evaluate_settled, exit_code_for,
    render_findings, render_status, unresolved_findings,
};
use crate::credentials::{production_store, read_token};
use crate::event::EventWakeSource;
use crate::forge::github::create_github_provider;
use crate::forge::gitlab::create_gitlab_provider;
use crate::forge::{
    CliError, ForgeName, ForgeProvider, SnapshotFetchOptions, UsageError, auto_detect_forge,
};
use crate::github_webhook::{
    ProcessGh, SetupAction, read_webhook_secret, setup_webhook, validate_repository,
};
use crate::wait::{PollingWakeSource, WaitOutcome, WakeSource, wait_until_settled};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandName {
    Status,
    Findings,
    Wait,
    GatewayToken,
    GatewayWebhook,
    Help,
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum GatewayTokenAction {
    /// Create a gateway token.
    Enroll,
    /// Report whether a gateway token is configured.
    Status,
    /// Remove the configured gateway token.
    Delete,
    /// Replace the gateway token.
    Rotate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayWebhookAction {
    Setup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOptions {
    pub command: CommandName,
    pub pr: Option<String>,
    pub repo: Option<String>,
    pub bots: Vec<String>,
    pub forge: Option<ForgeName>,
    pub all: bool,
    pub nitpicks: bool,
    pub no_reviews: bool,
    pub timeout_secs: u64,
    pub interval_secs: u64,
    pub events: bool,
    pub gateway_url: Option<String>,
    pub gateway_token_action: Option<GatewayTokenAction>,
    pub gateway_webhook_action: Option<GatewayWebhookAction>,
}

/// Watch pull requests and merge requests until checks and bot reviews settle.
#[derive(Debug, Parser)]
#[command(name = "babysit", version, subcommand_required = true)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Fetch and print the current status.
    Status(StatusArgs),
    /// Fetch and print unresolved review findings.
    Findings(FindingsArgs),
    /// Wait until checks and configured bot reviews settle.
    Wait(WaitArgs),
    /// Manage the gateway authentication token.
    #[command(name = "gateway-token")]
    GatewayToken {
        #[command(subcommand)]
        action: GatewayTokenAction,
    },
    /// Create or update the GitHub webhook used for event-assisted waits.
    #[command(name = "gateway-webhook")]
    GatewayWebhook {
        #[command(subcommand)]
        action: GatewayWebhookCommand,
    },
}

#[derive(Debug, Subcommand)]
enum GatewayWebhookCommand {
    /// Create or update the gateway webhook for a repository.
    Setup(GatewayWebhookSetupArgs),
}

#[derive(Debug, Args)]
struct GatewayWebhookSetupArgs {
    /// Repository to configure, in OWNER/REPOSITORY form.
    #[arg(long, value_parser = parse_webhook_repository)]
    repo: String,
}

#[derive(Debug, Args)]
struct CommonArgs {
    /// Pull request or merge request number.
    #[arg(value_name = "PR", value_parser = parse_pr)]
    pr: Option<String>,
    /// Repository to inspect, in OWNER/REPO form.
    #[arg(short = 'R', long, value_parser = parse_repo)]
    repo: Option<String>,
    /// Comma-separated bot logins to treat as reviewers.
    #[arg(long, value_parser = parse_bots_arg)]
    bots: Option<BotList>,
    /// Forge to use instead of auto-detection.
    #[arg(long, value_parser = parse_forge_arg)]
    forge: Option<ForgeName>,
}

#[derive(Debug, Args)]
struct StatusArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Include CodeRabbit nitpick review-body findings.
    #[arg(long)]
    nitpicks: bool,
    /// Settle without waiting for a matching bot review.
    #[arg(long)]
    no_reviews: bool,
}

#[derive(Debug, Args)]
struct FindingsArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Include resolved and outdated findings.
    #[arg(long)]
    all: bool,
    /// Include CodeRabbit nitpick review-body findings.
    #[arg(long)]
    nitpicks: bool,
}

#[derive(Debug, Args)]
struct WaitArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Include resolved and outdated findings.
    #[arg(long)]
    all: bool,
    /// Include CodeRabbit nitpick review-body findings.
    #[arg(long)]
    nitpicks: bool,
    /// Settle without waiting for a matching bot review.
    #[arg(long)]
    no_reviews: bool,
    /// Overall wait deadline in seconds.
    #[arg(
        long = "timeout",
        default_value_t = DEFAULT_WAIT_SECONDS,
        value_parser = parse_seconds
    )]
    timeout_secs: u64,
    /// Polling interval in seconds; event mode defaults to 300.
    #[arg(long = "interval", value_parser = parse_seconds)]
    interval_secs: Option<u64>,
    /// Use GitHub gateway events instead of only polling.
    #[arg(long, requires = "gateway_url")]
    events: bool,
    /// Gateway WebSocket URL used by event mode.
    #[arg(long, requires = "events", value_parser = parse_gateway_url)]
    gateway_url: Option<String>,
}

enum ParseOutcome {
    Options(CliOptions),
    Display {
        command: CommandName,
        output: String,
    },
}

pub fn parse_args(argv: &[String]) -> Result<CliOptions, UsageError> {
    match parse_cli(argv)? {
        ParseOutcome::Options(options) => Ok(options),
        ParseOutcome::Display { command, .. } => Ok(default_options(command)),
    }
}

fn parse_cli(argv: &[String]) -> Result<ParseOutcome, UsageError> {
    let args = std::iter::once("babysit".to_string()).chain(argv.iter().cloned());
    match Cli::try_parse_from(args) {
        Ok(cli) => Ok(ParseOutcome::Options(cli.into_options())),
        Err(error) => match error.kind() {
            clap::error::ErrorKind::DisplayHelp => Ok(ParseOutcome::Display {
                command: CommandName::Help,
                output: error.to_string(),
            }),
            clap::error::ErrorKind::DisplayVersion => Ok(ParseOutcome::Display {
                command: CommandName::Version,
                output: error.to_string(),
            }),
            _ => Err(UsageError::new(error.to_string())),
        },
    }
}

impl Cli {
    fn into_options(self) -> CliOptions {
        match self.command {
            CliCommand::Status(args) => {
                let mut options = options_from_common(CommandName::Status, args.common);
                options.nitpicks = args.nitpicks;
                options.no_reviews = args.no_reviews;
                options
            }
            CliCommand::Findings(args) => {
                let mut options = options_from_common(CommandName::Findings, args.common);
                options.all = args.all;
                options.nitpicks = args.nitpicks;
                options
            }
            CliCommand::Wait(args) => {
                let mut options = options_from_common(CommandName::Wait, args.common);
                options.all = args.all;
                options.nitpicks = args.nitpicks;
                options.no_reviews = args.no_reviews;
                options.timeout_secs = args.timeout_secs;
                options.interval_secs =
                    args.interval_secs
                        .unwrap_or(if args.events { 300 } else { 30 });
                options.events = args.events;
                options.gateway_url = args.gateway_url;
                options
            }
            CliCommand::GatewayToken { action } => {
                let mut options = default_options(CommandName::GatewayToken);
                options.gateway_token_action = Some(action);
                options
            }
            CliCommand::GatewayWebhook {
                action: GatewayWebhookCommand::Setup(args),
            } => {
                let mut options = default_options(CommandName::GatewayWebhook);
                options.repo = Some(args.repo);
                options.gateway_webhook_action = Some(GatewayWebhookAction::Setup);
                options
            }
        }
    }
}

fn options_from_common(command: CommandName, common: CommonArgs) -> CliOptions {
    CliOptions {
        command,
        pr: common.pr,
        repo: common.repo,
        bots: common.bots.map(|bots| bots.0).unwrap_or_else(default_bots),
        forge: common.forge,
        all: false,
        nitpicks: false,
        no_reviews: false,
        timeout_secs: DEFAULT_WAIT_SECONDS,
        interval_secs: 30,
        events: false,
        gateway_url: None,
        gateway_token_action: None,
        gateway_webhook_action: None,
    }
}

fn default_options(command: CommandName) -> CliOptions {
    CliOptions {
        command,
        pr: None,
        repo: None,
        bots: default_bots(),
        forge: None,
        all: false,
        nitpicks: false,
        no_reviews: false,
        timeout_secs: DEFAULT_WAIT_SECONDS,
        interval_secs: 30,
        events: false,
        gateway_url: None,
        gateway_token_action: None,
        gateway_webhook_action: None,
    }
}

fn default_bots() -> Vec<String> {
    DEFAULT_BOTS.iter().map(|bot| (*bot).to_string()).collect()
}

fn parse_pr(value: &str) -> Result<String, String> {
    if !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit()) {
        Ok(value.to_string())
    } else {
        Err(format!("invalid PR number: {value}"))
    }
}

fn parse_repo(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        Err("--repo requires a non-empty value not starting with '-'".to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

fn parse_webhook_repository(value: &str) -> Result<String, String> {
    let repo = parse_repo(value)?;
    validate_repository(&repo).map_err(str::to_string)?;
    Ok(repo)
}

#[derive(Debug, Clone)]
struct BotList(Vec<String>);

fn parse_bots_arg(value: &str) -> Result<BotList, String> {
    parse_bots(value).map(BotList)
}

fn parse_bots(value: &str) -> Result<Vec<String>, String> {
    let bots: Vec<String> = value
        .split(',')
        .map(str::trim)
        .filter(|bot| !bot.is_empty())
        .map(str::to_string)
        .collect();
    if bots.is_empty() {
        Err("--bots requires at least one bot".to_string())
    } else {
        Ok(bots)
    }
}

fn parse_forge_arg(value: &str) -> Result<ForgeName, String> {
    match value {
        "github" => Ok(ForgeName::GitHub),
        "gitlab" => Ok(ForgeName::GitLab),
        _ => Err("--forge must be github or gitlab".to_string()),
    }
}

fn parse_seconds(value: &str) -> Result<u64, String> {
    match value.parse::<u64>() {
        Ok(seconds) if seconds > 0 && seconds <= MAX_WAIT_SECONDS => Ok(seconds),
        Ok(_) => Err(format!(
            "value must be between 1 and {MAX_WAIT_SECONDS} seconds"
        )),
        Err(_) => Err("value must be a positive integer number of seconds".to_string()),
    }
}

fn parse_gateway_url(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err("--gateway-url requires a value".to_string())
    } else {
        Ok(value.trim().to_string())
    }
}

pub fn run(argv: &[String]) -> i32 {
    match run_inner(argv) {
        Ok(code) => code,
        Err(RunError::Usage(error)) => {
            eprintln!("{}", error.0.message);
            error.0.exit_code
        }
        Err(RunError::Cli(error)) => {
            eprintln!("{}", error.message);
            error.exit_code
        }
    }
}

enum RunError {
    Usage(UsageError),
    Cli(CliError),
}

fn run_inner(argv: &[String]) -> Result<i32, RunError> {
    let opts = match parse_cli(argv).map_err(RunError::Usage)? {
        ParseOutcome::Options(options) => options,
        ParseOutcome::Display { output, .. } => {
            println!("{}", output.trim_end());
            return Ok(0);
        }
    };
    match opts.command {
        CommandName::Status => run_status(&opts).map_err(RunError::Cli),
        CommandName::Findings => run_findings(&opts).map_err(RunError::Cli),
        CommandName::Wait => run_wait(&opts).map_err(RunError::Cli),
        CommandName::GatewayToken => run_gateway_token(&opts).map_err(RunError::Cli),
        CommandName::GatewayWebhook => run_gateway_webhook(&opts).map_err(RunError::Cli),
        CommandName::Help | CommandName::Version => {
            unreachable!("display requests are handled while parsing")
        }
    }
}

fn run_gateway_webhook(opts: &CliOptions) -> Result<i32, CliError> {
    let secret = read_webhook_secret()?;
    let mut gh = ProcessGh;
    let repo = opts.repo.as_deref().expect("webhook repository was parsed");
    let result = setup_webhook(repo, &secret, &mut gh)?;
    let action = match result.action {
        SetupAction::Created => "created",
        SetupAction::Updated => "updated",
    };
    println!("GitHub webhook {action} for {repo}");
    Ok(0)
}

fn run_gateway_token(opts: &CliOptions) -> Result<i32, CliError> {
    let store = production_store();
    let action = opts
        .gateway_token_action
        .expect("gateway token action was parsed");
    println!(
        "{}",
        gateway_token_action(action, store.as_ref(), read_token)?
    );
    Ok(0)
}

/// Performs a token action through injected storage and input boundaries.
pub fn gateway_token_action<F>(
    action: GatewayTokenAction,
    store: &dyn crate::credentials::TokenStore,
    input: F,
) -> Result<&'static str, CliError>
where
    F: FnOnce() -> Result<crate::credentials::SecretToken, CliError>,
{
    match action {
        GatewayTokenAction::Status => Ok(if store.load()?.is_some() {
            "gateway token: configured"
        } else {
            "gateway token: not configured"
        }),
        GatewayTokenAction::Delete => {
            store.delete()?;
            Ok("gateway token deleted")
        }
        GatewayTokenAction::Enroll | GatewayTokenAction::Rotate => {
            store.save(&input()?)?;
            Ok("gateway token saved")
        }
    }
}

fn run_status(opts: &CliOptions) -> Result<i32, CliError> {
    let snapshot = fetch_snapshot(opts)?;
    let settle = evaluate_settled(&snapshot, &settle_options(opts));
    println!("{}", render_status(&snapshot, &settle, None));
    Ok(exit_code_for(&snapshot, &settle))
}

fn run_findings(opts: &CliOptions) -> Result<i32, CliError> {
    let snapshot = fetch_snapshot(opts)?;
    println!(
        "{}",
        render_findings(&selected_findings(&snapshot, opts), "findings")
    );
    Ok(0)
}

fn run_wait(opts: &CliOptions) -> Result<i32, CliError> {
    let forge = resolve_forge(opts);
    if opts.events && forge == ForgeName::GitLab {
        return Err(CliError::new(
            "--events is supported only for GitHub",
            false,
        ));
    }
    let mut wake_source: Box<dyn WakeSource> = if opts.events {
        Box::new(EventWakeSource::new(
            opts.gateway_url.as_deref().expect("gateway URL was parsed"),
        )?)
    } else {
        Box::new(PollingWakeSource)
    };
    let mut fetcher =
        |remaining| fetch_snapshot_for(opts, forge, Instant::now().checked_add(remaining));
    let outcome = wait_until_settled(
        &mut fetcher,
        &mut *wake_source,
        Duration::from_secs(opts.timeout_secs),
        Duration::from_secs(opts.interval_secs),
        &settle_options(opts),
    )?;
    match outcome {
        WaitOutcome::Settled { snapshot, settle } => finish_wait(&snapshot, &settle, opts),
        WaitOutcome::TimedOut { snapshot, settle } => {
            println!("{}", wait_output(&snapshot, &settle, opts, Some("TIMEOUT")));
            Ok(3)
        }
        WaitOutcome::TimedOutWithoutSnapshot => {
            println!("TIMEOUT: no authoritative snapshot was fetched");
            Ok(3)
        }
    }
}

fn finish_wait(
    snapshot: &PrSnapshot,
    settle: &SettleResult,
    opts: &CliOptions,
) -> Result<i32, CliError> {
    println!("{}", wait_output(snapshot, settle, opts, None));
    Ok(exit_code_for(snapshot, settle))
}

fn wait_output(
    snapshot: &PrSnapshot,
    settle: &SettleResult,
    opts: &CliOptions,
    label: Option<&str>,
) -> String {
    let findings = selected_findings(snapshot, opts);
    let mut blocks = vec![render_status(snapshot, settle, label)];
    if !unresolved_findings(snapshot).is_empty() {
        blocks.push(render_findings(&findings, "findings"));
    }
    blocks.join("\n\n")
}

fn selected_findings(snapshot: &PrSnapshot, opts: &CliOptions) -> Vec<Finding> {
    if opts.all {
        snapshot.findings.clone()
    } else {
        unresolved_findings(snapshot)
    }
}

fn settle_options(opts: &CliOptions) -> SettleOptions {
    SettleOptions {
        no_reviews: opts.no_reviews,
        bots: opts.bots.clone(),
    }
}

fn fetch_snapshot(opts: &CliOptions) -> Result<PrSnapshot, CliError> {
    fetch_snapshot_for(opts, resolve_forge(opts), None)
}

fn resolve_forge(opts: &CliOptions) -> ForgeName {
    opts.forge.unwrap_or_else(auto_detect_forge)
}

fn fetch_snapshot_for(
    opts: &CliOptions,
    forge: ForgeName,
    deadline: Option<Instant>,
) -> Result<PrSnapshot, CliError> {
    let fetch_opts = SnapshotFetchOptions {
        pr: opts.pr.clone(),
        repo: opts.repo.clone(),
        bots: opts.bots.clone(),
        nitpicks: opts.nitpicks,
        deadline,
    };
    match forge {
        ForgeName::GitLab => create_gitlab_provider().fetch_snapshot(&fetch_opts),
        ForgeName::GitHub => create_github_provider().fetch_snapshot(&fetch_opts),
    }
}
