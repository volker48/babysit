// Regex keeps the Rust bot markdown parsers aligned with the original TS regex behavior.
use std::sync::LazyLock;

use regex::Regex;

use crate::core::Finding;

pub const DEFAULT_BOTS: [&str; 3] = ["coderabbitai", "chatgpt-codex-connector", "cursor"];

static CODE_RABBIT_ACTIONABLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\*\*Actionable comments posted: (\d+)\*\*").expect("valid CodeRabbit count regex")
});
static BUGBOT_ACTIONABLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)Cursor Bugbot has reviewed.*?found\s+(\d+)\s+potential issues")
        .expect("valid Bugbot count regex")
});
static CODE_RABBIT_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*_[^_\n]+_(?: \| _[^_\n]+_)+").expect("valid CodeRabbit header regex")
});
static SEVERITY_WORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[^a-z])(critical|major|minor|trivial)(?:[^a-z]|$)")
        .expect("valid severity word regex")
});
static CODEX_SEVERITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[P(\d) Badge\]").expect("valid Codex severity regex"));
static MARKDOWN_IMAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[[^\]]*\]\([^)]*\)").expect("valid Markdown image regex"));
static BUGBOT_TITLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^###\s+(.+)$").expect("valid Bugbot title regex"));
static BUGBOT_SEVERITY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^\*\*(High|Medium|Low) Severity\*\*$").expect("valid Bugbot severity regex")
});
static BOLD_ONLY_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\*\*(.+?)\*\*$").expect("valid bold-only line regex"));
static FENCED_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```[^\n]*\n(.*?)```").expect("valid fenced block regex"));
static BUGBOT_DESCRIPTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<!-- DESCRIPTION START -->(.*?)<!-- DESCRIPTION END -->")
        .expect("valid Bugbot description regex")
});
static BUGBOT_CTA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<div>\s*<a href="https://cursor\.com/open\?.*?</div>"#)
        .expect("valid Bugbot CTA regex")
});
static BUGBOT_REVIEWED_BY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<sup>Reviewed by \[Cursor Bugbot\].*?</sup>")
        .expect("valid Bugbot reviewed-by regex")
});
static HTML_COMMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->").expect("valid HTML comment regex"));
static INLINE_HTML_TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"</?(?:sub|blockquote|summary|br)\s*/?>").expect("valid inline HTML tag regex")
});
static EXCESSIVE_BLANK_LINES_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n{3,}").expect("valid excessive blank lines regex"));
static NITPICK_FILE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<summary>([^<(]+?) \(\d+\)</summary>").expect("valid nitpick file regex")
});
static NITPICK_SECTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<summary>🧹 Nitpick comments \(\d+\)</summary>")
        .expect("valid nitpick section regex")
});
static NITPICK_ENTRY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`(\d+(?:-\d+)?)`:").expect("valid nitpick entry regex"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distilled {
    pub severity: Option<String>,
    pub title: String,
    pub detail: String,
}

#[derive(Clone)]
pub struct BotAdapter {
    pub login: String,
    pub short_name: String,
    pub check_names: Vec<String>,
}

impl BotAdapter {
    pub fn distill(&self, body: &str) -> Distilled {
        match self.login.as_str() {
            "coderabbitai" => distill_code_rabbit(body),
            "chatgpt-codex-connector" => distill_codex(body),
            "cursor" => distill_bugbot(body),
            _ => generic_distill(body),
        }
    }

    pub fn actionable_count(&self, body: &str) -> Option<u32> {
        match self.login.as_str() {
            "coderabbitai" => parse_code_rabbit_actionable_count(body),
            "cursor" => parse_bugbot_actionable_count(body),
            _ => None,
        }
    }

    pub fn review_body_findings(&self, body: &str) -> Vec<Finding> {
        if self.login == "coderabbitai" {
            parse_nitpicks(body, "coderabbit")
        } else {
            Vec::new()
        }
    }
}

pub fn normalize_bot_login(login: &str) -> String {
    let lower = login.to_lowercase();
    if lower.ends_with("[bot]") {
        lower[..lower.len() - 5].to_string()
    } else {
        lower
    }
}

pub fn adapter_for_login(login: &str, configured_bots: &[String]) -> Option<BotAdapter> {
    let normalized = normalize_bot_login(login);
    let configured: Vec<String> = if configured_bots.is_empty() {
        DEFAULT_BOTS.iter().map(|s| s.to_string()).collect()
    } else {
        configured_bots
            .iter()
            .map(|s| normalize_bot_login(s))
            .collect()
    };
    if configured.iter().any(|bot| bot == &normalized) {
        Some(known_or_generic_adapter(&normalized))
    } else {
        None
    }
}

pub fn adapters_for_bots(bots: &[String]) -> Vec<BotAdapter> {
    let configured: Vec<String> = if bots.is_empty() {
        DEFAULT_BOTS.iter().map(|s| s.to_string()).collect()
    } else {
        bots.to_vec()
    };
    configured
        .iter()
        .map(|login| known_or_generic_adapter(&normalize_bot_login(login)))
        .collect()
}

pub fn distill_comment(login: &str, body: &str) -> Distilled {
    adapter_for_login(login, &[])
        .map_or_else(|| generic_distill(body), |adapter| adapter.distill(body))
}

fn known_or_generic_adapter(login: &str) -> BotAdapter {
    match login {
        "coderabbitai" => BotAdapter {
            login: login.to_string(),
            short_name: "coderabbit".to_string(),
            check_names: vec!["coderabbitai".to_string(), "coderabbit".to_string()],
        },
        "chatgpt-codex-connector" => BotAdapter {
            login: login.to_string(),
            short_name: "codex".to_string(),
            check_names: vec!["chatgpt-codex-connector".to_string(), "codex".to_string()],
        },
        "cursor" => BotAdapter {
            login: login.to_string(),
            short_name: "bugbot".to_string(),
            check_names: vec!["cursor".to_string(), "bugbot".to_string()],
        },
        _ => BotAdapter {
            login: login.to_string(),
            short_name: login.to_string(),
            check_names: vec![login.to_string()],
        },
    }
}

fn generic_distill(body: &str) -> Distilled {
    Distilled {
        severity: None,
        title: first_prose_line(body),
        detail: strip_noise(body),
    }
}

fn parse_code_rabbit_actionable_count(body: &str) -> Option<u32> {
    CODE_RABBIT_ACTIONABLE_RE
        .captures(body)?
        .get(1)?
        .as_str()
        .parse()
        .ok()
}

fn parse_bugbot_actionable_count(body: &str) -> Option<u32> {
    BUGBOT_ACTIONABLE_RE
        .captures(body)?
        .get(1)?
        .as_str()
        .parse()
        .ok()
}

pub fn distill_code_rabbit(body: &str) -> Distilled {
    let mut severity = None;
    if let Some(header) = CODE_RABBIT_HEADER_RE.find(body) {
        for segment in header.as_str().split('|') {
            if let Some(caps) = SEVERITY_WORD_RE.captures(segment) {
                severity = caps.get(1).map(|m| m.as_str().to_lowercase());
            }
        }
    }
    let title = bold_only_line(body).unwrap_or_else(|| first_prose_line(body));
    let detail =
        extract_agent_prompt(body).unwrap_or_else(|| strip_noise(&strip_after_first_details(body)));
    Distilled {
        severity,
        title,
        detail,
    }
}

pub fn distill_codex(body: &str) -> Distilled {
    let severity = CODEX_SEVERITY_RE
        .captures(body)
        .and_then(|caps| caps.get(1).map(|m| format!("P{}", m.as_str())));
    let title = bold_only_line(body)
        .unwrap_or_default()
        .replace("<sub>", "")
        .replace("</sub>", "");
    let title = MARKDOWN_IMAGE_RE.replace_all(&title, "").trim().to_string();
    let detail = strip_noise(body)
        .lines()
        .filter(|line| !line.starts_with("**") && !line.starts_with("Useful? React with"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    Distilled {
        severity,
        title: if title.is_empty() {
            first_prose_line(body)
        } else {
            title
        },
        detail,
    }
}

pub fn distill_bugbot(body: &str) -> Distilled {
    let title = BUGBOT_TITLE_RE
        .captures(body)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().trim().to_string()))
        .unwrap_or_else(|| first_prose_line(body));
    let severity = BUGBOT_SEVERITY_RE
        .captures(body)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().to_lowercase()));
    let detail = extract_bugbot_description(body)
        .map(|text| strip_noise(&text))
        .unwrap_or_else(|| strip_bugbot_fallback(body));
    Distilled {
        severity,
        title,
        detail,
    }
}

fn bold_only_line(body: &str) -> Option<String> {
    BOLD_ONLY_LINE_RE
        .captures(body)?
        .get(1)
        .map(|m| m.as_str().to_string())
}

fn extract_agent_prompt(body: &str) -> Option<String> {
    let marker = body.find("Prompt for AI Agents</summary>")?;
    FENCED_BLOCK_RE
        .captures(&body[marker..])?
        .get(1)
        .map(|m| m.as_str().trim().to_string())
}

fn extract_bugbot_description(body: &str) -> Option<String> {
    BUGBOT_DESCRIPTION_RE
        .captures(body)?
        .get(1)
        .map(|m| m.as_str().trim().to_string())
}

fn strip_after_first_details(body: &str) -> String {
    body.find("<details>")
        .map_or_else(|| body.to_string(), |index| body[..index].to_string())
}

fn strip_bugbot_fallback(body: &str) -> String {
    let text = strip_noise(body);
    let text = BUGBOT_CTA_RE.replace_all(&text, "").to_string();
    BUGBOT_REVIEWED_BY_RE
        .replace_all(&text, "")
        .replace("\n\n\n", "\n\n")
        .trim()
        .to_string()
}

pub fn strip_noise(body: &str) -> String {
    let mut text = body.to_string();
    while let Some(start) = text.find("<details") {
        let Some(end) = details_block_end(&text, start) else {
            break;
        };
        text.replace_range(start..end, "");
    }
    let text = HTML_COMMENT_RE.replace_all(&text, "").to_string();
    let text = INLINE_HTML_TAG_RE.replace_all(&text, "").to_string();
    EXCESSIVE_BLANK_LINES_RE
        .replace_all(&text, "\n\n")
        .trim()
        .to_string()
}

fn details_block_end(text: &str, start: usize) -> Option<usize> {
    let mut position = start;
    let mut depth = 0usize;
    loop {
        let next_open = text[position..]
            .find("<details")
            .map(|index| position + index);
        let next_close = text[position..]
            .find("</details>")
            .map(|index| position + index);
        match (next_open, next_close) {
            (Some(open), Some(close)) if open < close => {
                depth += 1;
                position = open + "<details".len();
            }
            (_, Some(close)) => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                position = close + "</details>".len();
                if depth == 0 {
                    return Some(position);
                }
            }
            _ => return None,
        }
    }
}

fn first_prose_line(body: &str) -> String {
    for line in strip_noise(body).lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return trimmed
                .trim_start_matches("**")
                .trim_end_matches("**")
                .to_string();
        }
    }
    "(no title)".to_string()
}

pub fn parse_nitpicks(review_body: &str, bot: &str) -> Vec<Finding> {
    let Some(section) = nitpick_section(review_body) else {
        return Vec::new();
    };
    let file_matches: Vec<_> = NITPICK_FILE_RE.captures_iter(section).collect();
    let mut findings = Vec::new();
    for (index, caps) in file_matches.iter().enumerate() {
        let full = caps.get(0).unwrap();
        let end = file_matches
            .get(index + 1)
            .and_then(|c| c.get(0))
            .map_or(section.len(), |m| m.start());
        let path = caps.get(1).unwrap().as_str().trim();
        findings.extend(parse_nitpick_entries(&section[full.end()..end], path, bot));
    }
    findings
}

fn nitpick_section(review_body: &str) -> Option<&str> {
    let summary = NITPICK_SECTION_RE.find(review_body)?;
    let details_start = review_body[..summary.start()].rfind("<details")?;
    let details_end = details_block_end(review_body, details_start)?;
    Some(&review_body[summary.end()..details_end])
}

fn parse_nitpick_entries(block: &str, path: &str, bot: &str) -> Vec<Finding> {
    let entries: Vec<_> = NITPICK_ENTRY_RE.captures_iter(block).collect();
    entries
        .iter()
        .enumerate()
        .map(|(index, caps)| {
            let full = caps.get(0).unwrap();
            let end = entries
                .get(index + 1)
                .and_then(|c| c.get(0))
                .map_or(block.len(), |m| m.start());
            let distilled = distill_code_rabbit(&block[full.end()..end]);
            Finding {
                path: path.to_string(),
                line: caps.get(1).unwrap().as_str().to_string(),
                bot: bot.to_string(),
                severity: distilled.severity,
                title: distilled.title,
                detail: distilled.detail,
                resolved: false,
                outdated: false,
            }
        })
        .collect()
}
