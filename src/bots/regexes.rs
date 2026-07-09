// Regex keeps the Rust bot markdown parsers aligned with the original TS regex behavior.
use std::sync::LazyLock;

use regex::Regex;

pub(super) static CODE_RABBIT_ACTIONABLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\*\*Actionable comments posted: (\d+)\*\*").expect("valid CodeRabbit count regex")
});
pub(super) static BUGBOT_ACTIONABLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)Cursor Bugbot has reviewed.*?found\s+(\d+)\s+potential issues")
        .expect("valid Bugbot count regex")
});
pub(super) static CODE_RABBIT_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*_[^_\n]+_(?: \| _[^_\n]+_)+").expect("valid CodeRabbit header regex")
});
pub(super) static SEVERITY_WORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[^a-z])(critical|major|minor|trivial)(?:[^a-z]|$)")
        .expect("valid severity word regex")
});
pub(super) static CODEX_SEVERITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[P(\d) Badge\]").expect("valid Codex severity regex"));
pub(super) static MARKDOWN_IMAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[[^\]]*\]\([^)]*\)").expect("valid Markdown image regex"));
pub(super) static BUGBOT_TITLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^###\s+(.+)$").expect("valid Bugbot title regex"));
pub(super) static BUGBOT_SEVERITY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^\*\*(High|Medium|Low) Severity\*\*$").expect("valid Bugbot severity regex")
});
pub(super) static BOLD_ONLY_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\*\*(.+?)\*\*$").expect("valid bold-only line regex"));
pub(super) static FENCED_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```[^\n]*\n(.*?)```").expect("valid fenced block regex"));
pub(super) static BUGBOT_DESCRIPTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<!-- DESCRIPTION START -->(.*?)<!-- DESCRIPTION END -->")
        .expect("valid Bugbot description regex")
});
pub(super) static BUGBOT_CTA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<div>\s*<a href="https://cursor\.com/open\?.*?</div>"#)
        .expect("valid Bugbot CTA regex")
});
pub(super) static BUGBOT_REVIEWED_BY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<sup>Reviewed by \[Cursor Bugbot\].*?</sup>")
        .expect("valid Bugbot reviewed-by regex")
});
pub(super) static HTML_COMMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->").expect("valid HTML comment regex"));
pub(super) static INLINE_HTML_TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"</?(?:sub|blockquote|summary|br)\s*/?>").expect("valid inline HTML tag regex")
});
pub(super) static EXCESSIVE_BLANK_LINES_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n{3,}").expect("valid excessive blank lines regex"));
pub(super) static NITPICK_FILE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<summary>([^<(]+?) \(\d+\)</summary>").expect("valid nitpick file regex")
});
pub(super) static NITPICK_SECTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<summary>🧹 Nitpick comments \(\d+\)</summary>")
        .expect("valid nitpick section regex")
});
pub(super) static NITPICK_ENTRY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`(\d+(?:-\d+)?)`:").expect("valid nitpick entry regex"));
