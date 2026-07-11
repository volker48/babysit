use std::fs;

const CANONICAL_SKILL: &str = "skills/babysit/SKILL.md";
const AGENT_SKILLS: [&str; 2] = [
    ".claude/skills/babysit/SKILL.md",
    ".codex/skills/babysit/SKILL.md",
];

#[derive(Debug, PartialEq, Eq)]
struct SkillMetadata {
    name: String,
    description: String,
}

#[test]
fn agent_skill_wrappers_match_canonical_metadata() {
    let canonical = read_skill_metadata(CANONICAL_SKILL);

    for path in AGENT_SKILLS {
        let metadata = read_skill_metadata(path);
        let content = fs::read_to_string(path).expect("agent skill wrapper should be readable");

        assert_eq!(
            metadata, canonical,
            "{path} metadata drifted from {CANONICAL_SKILL}"
        );
        assert!(
            content.contains(CANONICAL_SKILL),
            "{path} should point agents at {CANONICAL_SKILL}"
        );
    }
}

fn read_skill_metadata(path: &str) -> SkillMetadata {
    let content = fs::read_to_string(path).expect("skill file should be readable");
    parse_skill_metadata(path, &content)
}

fn parse_skill_metadata(path: &str, content: &str) -> SkillMetadata {
    let frontmatter = frontmatter_lines(path, content);
    let name = frontmatter_value(path, &frontmatter, "name");
    let description = frontmatter_value(path, &frontmatter, "description");

    SkillMetadata { name, description }
}

fn frontmatter_lines(path: &str, content: &str) -> Vec<String> {
    let mut lines = content.lines();
    assert_eq!(
        lines.next(),
        Some("---"),
        "{path} should start with YAML frontmatter"
    );

    let mut frontmatter = Vec::new();
    for line in lines {
        if line == "---" {
            return frontmatter;
        }
        frontmatter.push(line.to_owned());
    }

    panic!("{path} should close YAML frontmatter");
}

fn frontmatter_value(path: &str, lines: &[String], key: &str) -> String {
    let prefix = format!("{key}: ");
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("{path} should define `{key}` frontmatter"))
        .to_owned()
}
