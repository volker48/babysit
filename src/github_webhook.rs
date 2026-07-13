use std::io::{self, IsTerminal, Read};

use serde_json::{Value, json};

use crate::forge::{CliError, run_json, run_json_pages, run_json_with_stdin};

pub const WEBHOOK_URL: &str = "https://babysit.mindgoblin.pw/webhooks/github";
const PAGE_SIZE: usize = 100;
const EVENTS: [&str; 8] = [
    "check_run",
    "check_suite",
    "status",
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
    "pull_request_review_thread",
    "issue_comment",
];

/// A protected Cloudflare webhook secret that is never formatted for display.
pub struct WebhookSecret(String);

impl WebhookSecret {
    pub fn new(value: String) -> Result<Self, CliError> {
        if value.is_empty() || value.contains(['\r', '\n']) {
            return Err(CliError::new(
                "Cloudflare webhook secret must be a nonempty single line",
                false,
            ));
        }
        Ok(Self(value))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

/// Reads the existing secret from protected stdin or a no-echo terminal prompt.
pub fn read_webhook_secret() -> Result<WebhookSecret, CliError> {
    let mut value = if io::stdin().is_terminal() {
        rpassword::prompt_password("Cloudflare WEBHOOK_SECRET: ").map_err(|error| {
            CliError::new(
                format!("could not read Cloudflare webhook secret: {error}"),
                false,
            )
        })?
    } else {
        let mut input = String::new();
        io::stdin().read_to_string(&mut input).map_err(|error| {
            CliError::new(
                format!("could not read Cloudflare webhook secret: {error}"),
                false,
            )
        })?;
        input
    };
    trim_final_newline(&mut value);
    WebhookSecret::new(value)
}

fn trim_final_newline(value: &mut String) {
    if value.ends_with("\r\n") {
        value.truncate(value.len() - 2);
    } else if value.ends_with('\n') {
        value.pop();
    }
}

/// Injectable boundary around authenticated local `gh` calls.
pub trait GhClient {
    fn get_json(&mut self, args: &[String]) -> Result<Value, CliError>;
    fn mutate_json(&mut self, args: &[String], body: &[u8]) -> Result<Value, CliError>;
}

pub struct ProcessGh;

impl GhClient for ProcessGh {
    fn get_json(&mut self, args: &[String]) -> Result<Value, CliError> {
        run_json("gh", args, "gh repository hooks")
    }

    fn mutate_json(&mut self, args: &[String], body: &[u8]) -> Result<Value, CliError> {
        run_json_with_stdin("gh", args, "gh webhook mutation", body)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupAction {
    Created,
    Updated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupResult {
    pub action: SetupAction,
}

/// Validates the public `OWNER/REPOSITORY` repository form.
pub fn validate_repository(repo: &str) -> Result<(), &'static str> {
    let mut parts = repo.split('/');
    let Some(owner) = parts.next() else {
        return Err("repository must use OWNER/REPOSITORY form");
    };
    let Some(name) = parts.next() else {
        return Err("repository must use OWNER/REPOSITORY form");
    };
    if parts.next().is_some()
        || owner.is_empty()
        || name.is_empty()
        || !valid_repository_part(owner)
        || !valid_repository_part(name)
    {
        return Err("repository must use OWNER/REPOSITORY form");
    }
    Ok(())
}

fn valid_repository_part(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

/// Idempotently creates or updates the fixed babysit GitHub webhook.
pub fn setup_webhook(
    repo: &str,
    secret: &WebhookSecret,
    gh: &mut dyn GhClient,
) -> Result<SetupResult, CliError> {
    validate_repository(repo).map_err(|message| CliError::new(message, false))?;
    let hooks = list_hooks(repo, gh)?;
    let action = select_action(&hooks)?;
    let body = webhook_body(secret)?;
    let mutation_args = mutation_args(repo, action);
    gh.mutate_json(&mutation_args, &body)
        .map_err(|error| mutation_error(error, secret))?;
    let reconciled = list_hooks(repo, gh)?;
    verify_reconciliation(&reconciled)?;
    Ok(SetupResult {
        action: action.kind(),
    })
}

fn list_hooks(repo: &str, gh: &mut dyn GhClient) -> Result<Vec<Hook>, CliError> {
    let values = run_json_pages(
        |page, size| {
            let path = format!("repos/{repo}/hooks?per_page={size}&page={page}");
            gh.get_json(&["api".to_string(), path])
        },
        "gh repository hooks",
        PAGE_SIZE,
    )?;
    values.into_iter().map(|value| parse_hook(&value)).collect()
}

fn parse_hook(value: &Value) -> Result<Hook, CliError> {
    let object = value
        .as_object()
        .ok_or_else(|| malformed_hook("hook is not an object"))?;
    let id = object
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| malformed_hook("hook id is missing or invalid"))?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed_hook("hook name is missing or invalid"))?;
    let active = object
        .get("active")
        .and_then(Value::as_bool)
        .ok_or_else(|| malformed_hook("hook active state is missing or invalid"))?;
    let config = object
        .get("config")
        .and_then(Value::as_object)
        .ok_or_else(|| malformed_hook("hook config is missing or invalid"))?;
    let url = config
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed_hook("hook URL is missing or invalid"))?;
    let content_type = config
        .get("content_type")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed_hook("hook content type is missing or invalid"))?;
    let insecure_ssl = config
        .get("insecure_ssl")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed_hook("hook insecure SSL value is missing or invalid"))?;
    let events = object
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed_hook("hook events are missing or invalid"))?
        .iter()
        .map(|event| {
            event
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| malformed_hook("hook event is invalid"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Hook {
        id,
        name: name.to_string(),
        active,
        url: url.to_string(),
        content_type: content_type.to_string(),
        insecure_ssl: insecure_ssl.to_string(),
        events,
    })
}

fn malformed_hook(message: &str) -> CliError {
    CliError::new(format!("malformed GitHub hook JSON: {message}"), false)
}

#[derive(Clone)]
struct Hook {
    id: u64,
    name: String,
    active: bool,
    url: String,
    content_type: String,
    insecure_ssl: String,
    events: Vec<String>,
}

#[derive(Clone, Copy)]
enum Mutation {
    Create,
    Update(u64),
}

impl Mutation {
    fn kind(&self) -> SetupAction {
        match self {
            Self::Create => SetupAction::Created,
            Self::Update(_) => SetupAction::Updated,
        }
    }
}

fn select_action(hooks: &[Hook]) -> Result<Mutation, CliError> {
    let matches: Vec<&Hook> = hooks
        .iter()
        .filter(|hook| hook.url == WEBHOOK_URL)
        .collect();
    match matches.as_slice() {
        [] => Ok(Mutation::Create),
        [hook] if hook.name == "web" => Ok(Mutation::Update(hook.id)),
        [hook] => Err(conflict_error(&format!(
            "unexpected hook name `{}`",
            hook.name
        ))),
        _ => Err(conflict_error("multiple matching hooks")),
    }
}

fn conflict_error(reason: &str) -> CliError {
    CliError::new(
        format!("GitHub webhook conflict: {reason}; no mutation performed"),
        false,
    )
}

fn webhook_body(secret: &WebhookSecret) -> Result<Vec<u8>, CliError> {
    serde_json::to_vec(&json!({
        "active": true,
        "config": {
            "content_type": "json",
            "insecure_ssl": "0",
            "url": WEBHOOK_URL,
            "secret": secret.expose(),
        },
        "events": EVENTS,
    }))
    .map_err(|error| CliError::new(format!("could not encode webhook JSON: {error}"), false))
}

fn mutation_args(repo: &str, mutation: Mutation) -> Vec<String> {
    let (method, path) = match mutation {
        Mutation::Create => ("POST", format!("repos/{repo}/hooks")),
        Mutation::Update(id) => ("PATCH", format!("repos/{repo}/hooks/{id}")),
    };
    vec![
        "api".to_string(),
        "--method".to_string(),
        method.to_string(),
        path,
        "--input".to_string(),
        "-".to_string(),
    ]
}

fn mutation_error(error: CliError, secret: &WebhookSecret) -> CliError {
    let detail = error.message.replace(secret.expose(), "[redacted]");
    CliError::new(
        format!(
            "gh webhook mutation failed: {detail}; state may have changed; rerunning this idempotent command is safe"
        ),
        true,
    )
}

fn verify_reconciliation(hooks: &[Hook]) -> Result<(), CliError> {
    let matches: Vec<&Hook> = hooks
        .iter()
        .filter(|hook| hook.url == WEBHOOK_URL)
        .collect();
    if matches.len() != 1 {
        return Err(reconciliation_error(
            "expected exactly one matching webhook",
        ));
    }
    let hook = matches[0];
    if hook.name != "web" || !expected_state(hook) {
        return Err(reconciliation_error(
            "matching webhook has unexpected state",
        ));
    }
    Ok(())
}

fn expected_state(hook: &Hook) -> bool {
    hook.active && hook.content_type == "json" && hook.insecure_ssl == "0" && hook.events == EVENTS
}

fn reconciliation_error(reason: &str) -> CliError {
    CliError::new(
        format!("GitHub webhook reconciliation failed: {reason}"),
        false,
    )
}
