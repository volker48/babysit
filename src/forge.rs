pub mod github;
pub mod gitlab;

use std::process::Command;
use std::time::Instant;

use crate::core::PrSnapshot;
use crate::error::CliError;

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
