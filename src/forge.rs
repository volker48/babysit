pub mod github;
pub mod gitlab;

use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle, sleep};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::core::PrSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeName {
    GitHub,
    GitLab,
}

impl ForgeName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GitHub => "github",
            Self::GitLab => "gitlab",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SnapshotFetchOptions {
    pub pr: Option<String>,
    pub repo: Option<String>,
    pub bots: Vec<String>,
    pub nitpicks: bool,
    pub deadline: Option<Instant>,
}

pub trait ForgeProvider {
    fn fetch_snapshot(&self, opts: &SnapshotFetchOptions) -> Result<PrSnapshot, CliError>;
}

#[derive(Debug, Clone)]
pub struct CliError {
    pub message: String,
    pub exit_code: i32,
    pub retryable: bool,
}

impl CliError {
    pub fn new(message: impl Into<String>, retryable: bool) -> Self {
        Self {
            message: message.into(),
            exit_code: 4,
            retryable,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CliError {}

#[derive(Debug, Clone)]
pub struct UsageError(pub CliError);

impl UsageError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(CliError::new(message, false))
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.message)
    }
}

impl std::error::Error for UsageError {}

pub fn detect_forge_from_remote_url(remote_url: Option<&str>) -> ForgeName {
    let host = remote_host(remote_url.unwrap_or(""));
    if host.to_lowercase().contains("gitlab") {
        ForgeName::GitLab
    } else {
        ForgeName::GitHub
    }
}

pub fn auto_detect_forge() -> ForgeName {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output();
    let Ok(output) = output else {
        return ForgeName::GitHub;
    };
    if !output.status.success() {
        return ForgeName::GitHub;
    }
    detect_forge_from_remote_url(Some(String::from_utf8_lossy(&output.stdout).trim()))
}

const CLI_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_JSON_PAGES: usize = 100;
type StreamReader = JoinHandle<io::Result<Vec<u8>>>;

struct ChildStreams {
    input_writer: Option<JoinHandle<io::Result<()>>>,
    stdout_reader: StreamReader,
    stderr_reader: StreamReader,
}

impl ChildStreams {
    fn is_finished(&self) -> bool {
        self.input_writer
            .as_ref()
            .is_none_or(JoinHandle::is_finished)
            && self.stdout_reader.is_finished()
            && self.stderr_reader.is_finished()
    }

    fn join(self, context: &str) -> Result<(Vec<u8>, Vec<u8>), CliError> {
        let input_error = self
            .input_writer
            .and_then(|handle| join_input_writer(handle, context).err());
        let stdout = join_stream(self.stdout_reader, context);
        let stderr = join_stream(self.stderr_reader, context);
        if let Some(error) = input_error {
            return Err(error);
        }
        Ok((stdout?, stderr?))
    }

    fn discard(self, context: &str) {
        if self.is_finished() {
            let _ = self.join(context);
        }
    }
}

pub fn run_json(command: &str, args: &[String], context: &str) -> Result<Value, CliError> {
    run_json_deadline(command, args, context, None)
}

pub fn run_json_deadline(
    command: &str,
    args: &[String],
    context: &str,
    deadline: Option<Instant>,
) -> Result<Value, CliError> {
    run_json_output(command_output(command, args, context, deadline)?, context)
}

pub fn run_json_with_stdin(
    command: &str,
    args: &[String],
    context: &str,
    input: &[u8],
) -> Result<Value, CliError> {
    run_json_with_stdin_deadline(command, args, context, input, None)
}

pub fn run_json_with_stdin_deadline(
    command: &str,
    args: &[String],
    context: &str,
    input: &[u8],
    deadline: Option<Instant>,
) -> Result<Value, CliError> {
    run_json_output(
        command_output_with_stdin(command, args, context, deadline, input)?,
        context,
    )
}

fn run_json_output(output: Output, context: &str) -> Result<Value, CliError> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if let Some(signal) = signal_text(&output) {
        return Err(cli_error(
            &format!("{context} terminated by {signal}"),
            &stderr,
            None,
            true,
        ));
    }
    if !output.status.success() {
        return Err(cli_error(context, &stderr, None, true));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        cli_error(
            &format!("{context} returned invalid JSON"),
            &stderr,
            Some(error.to_string()),
            false,
        )
    })
}

fn command_output(
    command: &str,
    args: &[String],
    context: &str,
    deadline: Option<Instant>,
) -> Result<Output, CliError> {
    command_output_inner(command, args, context, deadline, None)
}

fn command_output_with_stdin(
    command: &str,
    args: &[String],
    context: &str,
    deadline: Option<Instant>,
    input: &[u8],
) -> Result<Output, CliError> {
    command_output_inner(command, args, context, deadline, Some(input))
}

fn command_output_inner(
    command: &str,
    args: &[String],
    context: &str,
    deadline: Option<Instant>,
    input: Option<&[u8]>,
) -> Result<Output, CliError> {
    let deadline = command_deadline(Instant::now(), deadline)?;
    if Instant::now() >= deadline {
        return Err(timeout_error(context));
    }
    let mut child = Command::new(command)
        .args(args)
        .stdin(match input {
            Some(_) => Stdio::piped(),
            None => Stdio::inherit(),
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| cli_error(context, "", Some(error.to_string()), false))?;
    let streams = match spawn_child_streams(&mut child, context, input) {
        Ok(streams) => streams,
        Err(error) => {
            terminate_child(&mut child);
            return Err(error);
        }
    };
    wait_for_child(child, deadline, context, streams)
}

fn spawn_child_streams(
    child: &mut std::process::Child,
    context: &str,
    input: Option<&[u8]>,
) -> Result<ChildStreams, CliError> {
    let stdin = match input {
        Some(_) => Some(child.stdin.take().ok_or_else(|| {
            cli_error(context, "", Some("missing stdin pipe".to_string()), false)
        })?),
        None => None,
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| cli_error(context, "", Some("missing stdout pipe".to_string()), false))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| cli_error(context, "", Some("missing stderr pipe".to_string()), false))?;
    let stdout_reader = read_stream(stdout);
    let stderr_reader = read_stream(stderr);
    let input_writer = match (stdin, input) {
        (Some(mut stdin), Some(input)) => {
            let input = input.to_owned();
            Some(thread::spawn(move || stdin.write_all(&input)))
        }
        (None, None) => None,
        _ => unreachable!("stdin and input presence must match"),
    };
    Ok(ChildStreams {
        input_writer,
        stdout_reader,
        stderr_reader,
    })
}

fn wait_for_child(
    mut child: std::process::Child,
    deadline: Instant,
    context: &str,
    streams: ChildStreams,
) -> Result<Output, CliError> {
    let mut status = None;
    loop {
        if Instant::now() >= deadline {
            terminate_child(&mut child);
            streams.discard(context);
            return Err(timeout_error(context));
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exit_status)) => status = Some(exit_status),
                Ok(None) => {}
                Err(error) => {
                    terminate_child(&mut child);
                    streams.discard(context);
                    return Err(cli_error(context, "", Some(error.to_string()), false));
                }
            }
        }
        if streams.is_finished() {
            if let Some(status) = status {
                let (stdout, stderr) = streams.join(context)?;
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
        }
        sleep(Duration::from_millis(50));
    }
}

fn terminate_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn command_deadline(now: Instant, overall: Option<Instant>) -> Result<Instant, CliError> {
    let command = now
        .checked_add(CLI_TIMEOUT)
        .ok_or_else(|| CliError::new("command deadline is too large", false))?;
    Ok(overall.map_or(command, |deadline| deadline.min(command)))
}

fn timeout_error(context: &str) -> CliError {
    cli_error(context, "", Some("operation timed out".to_string()), true)
}

fn read_stream<R>(mut stream: R) -> JoinHandle<io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = Vec::new();
        stream.read_to_end(&mut output)?;
        Ok(output)
    })
}

fn join_stream(
    handle: JoinHandle<io::Result<Vec<u8>>>,
    context: &str,
) -> Result<Vec<u8>, CliError> {
    handle
        .join()
        .map_err(|_| {
            cli_error(
                context,
                "",
                Some("output reader panicked".to_string()),
                false,
            )
        })?
        .map_err(|error| cli_error(context, "", Some(error.to_string()), false))
}

fn join_input_writer(handle: JoinHandle<io::Result<()>>, context: &str) -> Result<(), CliError> {
    handle
        .join()
        .map_err(|_| {
            cli_error(
                context,
                "",
                Some("command input writer panicked".to_string()),
                false,
            )
        })?
        .map_err(|error| {
            cli_error(
                context,
                "",
                Some(format!("could not fully write command input: {error}")),
                false,
            )
        })
}

#[cfg(unix)]
fn signal_text(output: &std::process::Output) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    output.status.signal().map(|signal| signal.to_string())
}

#[cfg(not(unix))]
fn signal_text(_output: &std::process::Output) -> Option<String> {
    None
}

pub fn run_json_pages<F>(
    mut fetch_page: F,
    context: &str,
    per_page: usize,
) -> Result<Vec<Value>, CliError>
where
    F: FnMut(usize, usize) -> Result<Value, CliError>,
{
    if per_page == 0 {
        return Err(parse_json_failure(
            context,
            "per_page must be greater than zero",
        ));
    }
    let mut results = Vec::new();
    for page in 1..=MAX_JSON_PAGES {
        let value = fetch_page(page, per_page)?;
        let items = json_array_page(value, context, page)?;
        let done = items.len() < per_page;
        results.extend(items);
        if done {
            return Ok(results);
        }
    }
    Err(parse_json_failure(
        context,
        format!("pagination exceeded {MAX_JSON_PAGES} pages"),
    ))
}

pub fn collect_json_pages<F>(
    mut fetch_page: F,
    context: &str,
    per_page: usize,
) -> Result<Vec<Value>, CliError>
where
    F: FnMut(usize, usize) -> Value,
{
    run_json_pages(|page, size| Ok(fetch_page(page, size)), context, per_page)
}

pub fn parse_json_failure(context: &str, cause: impl fmt::Display) -> CliError {
    cli_error(context, "", Some(cause.to_string()), false)
}

pub fn pagination_failure(context: &str) -> CliError {
    cli_error(context, "", None, false)
}

fn json_array_page(value: Value, context: &str, page: usize) -> Result<Vec<Value>, CliError> {
    match value {
        Value::Array(items) => Ok(items),
        _ => Err(parse_json_failure(
            &format!("{context} page {page}"),
            "returned a non-array JSON document",
        )),
    }
}

fn cli_error(context: &str, stderr: &str, cause: Option<String>, retryable: bool) -> CliError {
    let mut lines = vec![format!("{context} failed")];
    if !stderr.trim().is_empty() {
        lines.push(stderr.trim_end().to_string());
    }
    if let Some(cause) = cause {
        lines.push(cause);
    }
    CliError::new(lines.join("\n"), retryable)
}

fn remote_host(remote_url: &str) -> String {
    if remote_url.trim().is_empty() {
        return String::new();
    }
    if let Some(rest) = remote_url.split("://").nth(1) {
        return rest.split('/').next().unwrap_or("").to_string();
    }
    if let Some(index) = remote_url.find('@') {
        let rest = &remote_url[index + 1..];
        if let Some(colon) = rest.find(':') {
            return rest[..colon].to_string();
        }
    }
    remote_url
        .split([':', '/'])
        .next()
        .unwrap_or(remote_url)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_overall_deadline_still_caps_a_subprocess_at_cli_timeout() {
        let now = Instant::now();
        let overall = now.checked_add(Duration::from_secs(3600)).unwrap();

        assert_eq!(
            command_deadline(now, Some(overall)).unwrap(),
            now.checked_add(CLI_TIMEOUT).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn large_stdin_does_not_delay_deadline_timeout() {
        let args = ["-c".to_string(), "exec sleep 1".to_string()];
        let input = vec![b'x'; 1024 * 1024];
        let deadline = Instant::now() + Duration::from_millis(100);
        let started = Instant::now();

        let error = run_json_with_stdin_deadline(
            "sh",
            &args,
            "stdin deadline test",
            &input,
            Some(deadline),
        )
        .unwrap_err();

        assert!(
            started.elapsed() < Duration::from_millis(500),
            "deadline was delayed by stdin write: {error}"
        );
        assert!(error.retryable);
        assert!(error.message.contains("operation timed out"));
    }

    #[cfg(unix)]
    #[test]
    fn descendant_held_pipes_do_not_delay_cleanup_past_deadline() {
        for script in ["sleep 2 & exit 0", "sleep 2 & wait"] {
            let args = ["-c".to_string(), script.to_string()];
            let input = vec![b'x'; 1024 * 1024];
            let deadline = Instant::now() + Duration::from_millis(100);
            let started = Instant::now();

            let error = run_json_with_stdin_deadline(
                "sh",
                &args,
                "descendant pipe test",
                &input,
                Some(deadline),
            )
            .unwrap_err();

            assert!(
                started.elapsed() < Duration::from_secs(1),
                "cleanup exceeded the deadline for `{script}`: {error}"
            );
            assert!(error.retryable);
            assert!(error.message.contains("operation timed out"));
        }
    }
}
