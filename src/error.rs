use std::fmt;

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
