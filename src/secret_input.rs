use std::io::{self, IsTerminal, Read};

use crate::error::CliError;

/// Reads one secret line from piped stdin or a no-echo terminal prompt.
///
/// `label` names the secret in error messages; the final newline is trimmed so
/// callers can validate the value as a single line.
pub fn read_secret_line(prompt: &str, label: &str) -> Result<String, CliError> {
    let read_error =
        |error: io::Error| CliError::new(format!("could not read {label}: {error}"), false);
    let mut value = if io::stdin().is_terminal() {
        rpassword::prompt_password(prompt).map_err(read_error)?
    } else {
        let mut input = String::new();
        io::stdin().read_to_string(&mut input).map_err(read_error)?;
        input
    };
    trim_final_newline(&mut value);
    Ok(value)
}

fn trim_final_newline(value: &mut String) {
    if value.ends_with("\r\n") {
        value.truncate(value.len() - 2);
    } else if value.ends_with('\n') {
        value.pop();
    }
}
