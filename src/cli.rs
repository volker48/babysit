use std::time::{Duration, Instant};

const MAX_WAIT_SECONDS: u64 = 30 * 24 * 60 * 60;

use crate::bots::DEFAULT_BOTS;
use crate::core::{
    Finding, PrSnapshot, SettleOptions, SettleResult, evaluate_settled, exit_code_for,
    render_findings, render_status, unresolved_findings,
};
use crate::credentials::{production_store, read_token};
use crate::event::EventWakeSource;
use crate::forge::{
    CliError, ForgeName, ForgeProvider, SnapshotFetchOptions, UsageError, auto_detect_forge,
};
use crate::github::create_github_provider;
use crate::gitlab::create_gitlab_provider;
use crate::wait::{PollingWakeSource, WaitOutcome, WakeSource, wait_until_settled};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandName {
    Status,
    Findings,
    Wait,
    GatewayToken,
    Help,
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayTokenAction {
    Enroll,
    Status,
    Delete,
    Rotate,
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
}

struct ParseState {
    opts: CliOptions,
    wait_flag_used: bool,
    interval_explicit: bool,
}

enum ValueFlag {
    Repo,
    Bots,
    TimeoutSecs,
    IntervalSecs,
    Forge,
    GatewayUrl,
}

pub fn parse_args(argv: &[String]) -> Result<CliOptions, UsageError> {
    if is_command_help(argv) {
        return Ok(default_options(CommandName::Help));
    }
    let command = parse_command(argv.first().map(String::as_str))?;
    if matches!(command, CommandName::Help | CommandName::Version) {
        if argv.len() > 1 {
            return Err(UsageError::new("unexpected arguments"));
        }
        return Ok(default_options(command));
    }
    if command == CommandName::GatewayToken {
        return parse_gateway_token_args(argv);
    }
    let mut state = ParseState {
        opts: default_options(command),
        wait_flag_used: false,
        interval_explicit: false,
    };
    let mut index = 1;
    while index < argv.len() {
        index = consume_token(argv, index, &mut state)?;
    }
    validate_wait_options(&mut state)?;
    Ok(state.opts)
}

fn parse_command(value: Option<&str>) -> Result<CommandName, UsageError> {
    match value {
        Some("status") => Ok(CommandName::Status),
        Some("findings") => Ok(CommandName::Findings),
        Some("wait") => Ok(CommandName::Wait),
        Some("gateway-token") => Ok(CommandName::GatewayToken),
        Some("--help" | "-h" | "help") => Ok(CommandName::Help),
        Some("--version" | "-V") => Ok(CommandName::Version),
        Some(value) => Err(UsageError::new(format!("unknown subcommand: {value}"))),
        None => Err(UsageError::new("missing subcommand")),
    }
}

fn is_command_help(argv: &[String]) -> bool {
    argv.len() == 2
        && matches!(argv[1].as_str(), "--help" | "-h")
        && matches!(
            argv[0].as_str(),
            "status" | "findings" | "wait" | "gateway-token"
        )
}

fn default_options(command: CommandName) -> CliOptions {
    CliOptions {
        command,
        pr: None,
        repo: None,
        bots: DEFAULT_BOTS.iter().map(|s| s.to_string()).collect(),
        forge: None,
        all: false,
        nitpicks: false,
        no_reviews: false,
        timeout_secs: 1800,
        interval_secs: 30,
        events: false,
        gateway_url: None,
        gateway_token_action: None,
    }
}

fn consume_token(
    argv: &[String],
    index: usize,
    state: &mut ParseState,
) -> Result<usize, UsageError> {
    let arg = &argv[index];
    if assign_bool_flag(arg, state) {
        return Ok(index + 1);
    }
    if let Some((flag, value)) = inline_value(arg) {
        if let Some(value_flag) = value_flag(flag) {
            assign_value(state, value_flag, value)?;
            return Ok(index + 1);
        }
    }
    if let Some(value_flag) = value_flag(arg) {
        assign_value(state, value_flag, &required_value(argv, index, arg)?)?;
        return Ok(index + 2);
    }
    if arg.starts_with('-') {
        return Err(UsageError::new(format!("unknown flag: {arg}")));
    }
    assign_pr(&mut state.opts, arg)?;
    Ok(index + 1)
}

fn assign_bool_flag(arg: &str, state: &mut ParseState) -> bool {
    match arg {
        "--all" => state.opts.all = true,
        "--nitpicks" => state.opts.nitpicks = true,
        "--no-reviews" => state.opts.no_reviews = true,
        "--events" => {
            state.opts.events = true;
            state.wait_flag_used = true;
        }
        _ => return false,
    }
    true
}

fn inline_value(arg: &str) -> Option<(&str, &str)> {
    if !arg.starts_with("--") {
        return None;
    }
    let index = arg.find('=')?;
    Some((&arg[..index], &arg[index + 1..]))
}

fn value_flag(flag: &str) -> Option<ValueFlag> {
    match flag {
        "--repo" | "-R" => Some(ValueFlag::Repo),
        "--bots" => Some(ValueFlag::Bots),
        "--forge" => Some(ValueFlag::Forge),
        "--timeout" => Some(ValueFlag::TimeoutSecs),
        "--interval" => Some(ValueFlag::IntervalSecs),
        "--gateway-url" => Some(ValueFlag::GatewayUrl),
        _ => None,
    }
}

fn required_value(argv: &[String], index: usize, flag: &str) -> Result<String, UsageError> {
    match argv.get(index + 1) {
        Some(value) if !value.starts_with('-') => Ok(value.clone()),
        _ => Err(UsageError::new(format!("{flag} requires a value"))),
    }
}

fn assign_pr(opts: &mut CliOptions, value: &str) -> Result<(), UsageError> {
    if !value.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(UsageError::new(format!("invalid PR number: {value}")));
    }
    if opts.pr.is_some() {
        return Err(UsageError::new(format!(
            "unexpected positional argument: {value}"
        )));
    }
    opts.pr = Some(value.to_string());
    Ok(())
}

fn assign_value(state: &mut ParseState, flag: ValueFlag, value: &str) -> Result<(), UsageError> {
    match flag {
        ValueFlag::Repo => state.opts.repo = Some(non_empty(value, "--repo")?),
        ValueFlag::Bots => state.opts.bots = parse_bots(value)?,
        ValueFlag::Forge => state.opts.forge = Some(parse_forge(value)?),
        ValueFlag::TimeoutSecs => {
            state.opts.timeout_secs = parse_seconds(state, value, "--timeout")?
        }
        ValueFlag::IntervalSecs => {
            state.opts.interval_secs = parse_seconds(state, value, "--interval")?;
            state.interval_explicit = true;
        }
        ValueFlag::GatewayUrl => {
            state.opts.gateway_url = Some(non_empty(value, "--gateway-url")?);
            state.wait_flag_used = true;
        }
    }
    Ok(())
}

fn parse_gateway_token_args(argv: &[String]) -> Result<CliOptions, UsageError> {
    if argv.len() != 2 {
        return Err(UsageError::new(
            "gateway-token requires exactly one action: enroll, status, delete, or rotate",
        ));
    }
    let action = match argv[1].as_str() {
        "enroll" => GatewayTokenAction::Enroll,
        "status" => GatewayTokenAction::Status,
        "delete" => GatewayTokenAction::Delete,
        "rotate" => GatewayTokenAction::Rotate,
        _ => return Err(UsageError::new("unknown gateway-token action")),
    };
    let mut options = default_options(CommandName::GatewayToken);
    options.gateway_token_action = Some(action);
    Ok(options)
}

fn validate_wait_options(state: &mut ParseState) -> Result<(), UsageError> {
    if state.wait_flag_used && state.opts.command != CommandName::Wait {
        return Err(UsageError::new(
            "--timeout, --interval, --events, and --gateway-url are only valid with wait",
        ));
    }
    if state.opts.gateway_url.is_some() && !state.opts.events {
        return Err(UsageError::new("--gateway-url requires --events"));
    }
    if state.opts.events && state.opts.gateway_url.is_none() {
        return Err(UsageError::new("--events requires --gateway-url <wss-url>"));
    }
    if state.opts.events && !state.interval_explicit {
        state.opts.interval_secs = 300;
    }
    Ok(())
}

fn non_empty(value: &str, flag: &str) -> Result<String, UsageError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(UsageError::new(format!("{flag} requires a value")))
    } else {
        Ok(trimmed.to_string())
    }
}

fn parse_bots(value: &str) -> Result<Vec<String>, UsageError> {
    let bots: Vec<String> = value
        .split(',')
        .map(str::trim)
        .filter(|bot| !bot.is_empty())
        .map(str::to_string)
        .collect();
    if bots.is_empty() {
        return Err(UsageError::new("--bots requires at least one bot"));
    }
    Ok(bots)
}

fn parse_forge(value: &str) -> Result<ForgeName, UsageError> {
    match value {
        "github" => Ok(ForgeName::GitHub),
        "gitlab" => Ok(ForgeName::GitLab),
        _ => Err(UsageError::new("--forge must be github or gitlab")),
    }
}

fn parse_seconds(state: &mut ParseState, value: &str, flag: &str) -> Result<u64, UsageError> {
    state.wait_flag_used = true;
    match value.parse::<u64>() {
        Ok(seconds) if seconds > 0 && seconds <= MAX_WAIT_SECONDS => Ok(seconds),
        Ok(_) => Err(UsageError::new(format!(
            "{flag} must be between 1 and {MAX_WAIT_SECONDS} seconds"
        ))),
        Err(_) => Err(UsageError::new(format!(
            "{flag} must be a positive integer number of seconds"
        ))),
    }
}

pub fn run(argv: &[String]) -> i32 {
    match run_inner(argv) {
        Ok(code) => code,
        Err(RunError::Usage(error)) => {
            eprintln!("{}\n{}", error.0.message, usage());
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
    let opts = parse_args(argv).map_err(RunError::Usage)?;
    match opts.command {
        CommandName::Status => run_status(&opts).map_err(RunError::Cli),
        CommandName::Findings => run_findings(&opts).map_err(RunError::Cli),
        CommandName::Wait => run_wait(&opts).map_err(RunError::Cli),
        CommandName::GatewayToken => run_gateway_token(&opts).map_err(RunError::Cli),
        CommandName::Help => {
            println!("{}", usage());
            Ok(0)
        }
        CommandName::Version => {
            println!("babysit {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
    }
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

pub fn usage() -> String {
    [
        "Usage: babysit status [<pr>] [options]",
        "       babysit findings [<pr>] [options]",
        "       babysit wait [<pr>] [options]",
        "       babysit gateway-token <enroll|status|delete|rotate>",
        "       babysit --help",
        "       babysit --version",
        "Options:",
        "  -R, --repo <owner/repo>",
        "  --forge <github|gitlab>  default: auto (origin host containing gitlab => gitlab)",
        "  --bots <csv>",
        "  --all",
        "  --nitpicks",
        "  --no-reviews",
        "  --timeout <secs>        wait only (default 1800)",
        "  --interval <secs>       wait only (default 30; event fallback default 300)",
        "  --events                 wait only; requires --gateway-url",
        "  --gateway-url <wss-url>  event mode only",
        "  -h, --help              show help",
        "  -V, --version           show version",
    ]
    .join("\n")
}
