//! Port of senpi packages/coding-agent/src/core/skills-prompt.ts.
//!
//! The skills catalog rides on every request, so it is size-bounded: a large catalog is listed with
//! one-line descriptions, then names only, then truncated to whatever fits with an omitted count.
//! Wiring `skills::format_skills_for_prompt` (owned by the senpi-core-ext lane) to this builder is
//! that lane's change.

use std::sync::LazyLock;

use regex::Regex;

use crate::diagnostics::{ResourceDiagnostic, ResourceDiagnosticType};
use crate::skills::{FileReadTool, Skill};

pub const COMPACT_SKILL_CATALOG_COUNT: usize = 100;
pub const COMPACT_SKILL_CATALOG_DESCRIPTION_BYTES: usize = 16 * 1024;
pub const MAX_SKILL_CATALOG_BYTES: usize = 48 * 1024;
const ONE_LINE_DESCRIPTION_MAX_CHARS: usize = 160;
const TIGHT_ONE_LINE_DESCRIPTION_MAX_CHARS: usize = 100;
const MIN_FILL_CHARS: usize = 24;

static EXPLICIT_TRIGGER_CLAUSE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|[.!?;:)]\s+|\s+[-\u{2013}\u{2014}]\s+)(MUST (?:USE|READ|use|read)\b|USE ONLY WHEN\b|Use (?:when|whenever|for)\b|(?:Korean )?[Tt]riggers?(?::| -| on\b)|This skill should be used\b|Use this\b|When the user\b)",
    )
    .expect("explicit trigger clause regex")
});

static VERB_TRIGGER_CLAUSE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|[.!?;:)]\s+|\s+[-\u{2013}\u{2014}]\s+)((?:Use|Used|Read|Load|Invoke|Apply|Run|Call)\b[^.!?]{0,40}?\b(?:when|whenever|for|before|after|if|only)\b)",
    )
    .expect("verb trigger clause regex")
});

/// A catalog this large is listed by name plus a one-line description.
pub fn is_large_skill_catalog(skills: &[&Skill]) -> bool {
    skills.len() > COMPACT_SKILL_CATALOG_COUNT
        || skills
            .iter()
            .map(|skill| skill.description.len())
            .sum::<usize>()
            > COMPACT_SKILL_CATALOG_DESCRIPTION_BYTES
}

pub fn warn_if_large_skill_catalog(skills: &[Skill]) -> Vec<ResourceDiagnostic> {
    let catalog = build_skill_catalog(skills, FileReadTool::Read);
    if !catalog.compact && !catalog.names_only {
        return Vec::new();
    }
    let message = if catalog.names_only {
        format!(
            "The skills section is limited to {} KiB: descriptions were dropped and {} skills were omitted. Remove unused skills to shrink every request.",
            MAX_SKILL_CATALOG_BYTES / 1024,
            catalog.omitted
        )
    } else {
        format!(
            "The skills catalog uses one-line descriptions to limit request size (above {COMPACT_SKILL_CATALOG_COUNT} skills, {} KiB of descriptions, or {} KiB rendered). Remove unused skills to shrink every request.",
            COMPACT_SKILL_CATALOG_DESCRIPTION_BYTES / 1024,
            MAX_SKILL_CATALOG_BYTES / 1024
        )
    };
    vec![ResourceDiagnostic {
        diagnostic_type: ResourceDiagnosticType::Warning,
        category: Some("size-limit".to_owned()),
        message,
        path: None,
        collision: None,
    }]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillCatalog {
    pub prompt: String,
    pub compact: bool,
    pub names_only: bool,
    pub omitted: usize,
}

#[derive(Clone, Copy)]
struct RenderOptions {
    file_read_tool: FileReadTool,
    compact: bool,
    names_only: bool,
    omitted: usize,
    description_chars: usize,
}

pub fn build_skill_catalog(skills: &[Skill], file_read_tool: FileReadTool) -> SkillCatalog {
    let visible: Vec<&Skill> = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect();
    let compact = is_large_skill_catalog(&visible);
    let options = RenderOptions {
        file_read_tool,
        compact,
        names_only: false,
        omitted: 0,
        description_chars: ONE_LINE_DESCRIPTION_MAX_CHARS,
    };
    let prompt = render_skills(&visible, options);
    if prompt.len() <= MAX_SKILL_CATALOG_BYTES {
        return SkillCatalog {
            prompt,
            compact,
            names_only: false,
            omitted: 0,
        };
    }
    let compact_prompt = if compact {
        prompt
    } else {
        render_skills(
            &visible,
            RenderOptions {
                compact: true,
                ..options
            },
        )
    };
    if compact_prompt.len() <= MAX_SKILL_CATALOG_BYTES {
        return SkillCatalog {
            prompt: compact_prompt,
            compact: true,
            names_only: false,
            omitted: 0,
        };
    }
    let tight_prompt = render_skills(
        &visible,
        RenderOptions {
            compact: true,
            description_chars: TIGHT_ONE_LINE_DESCRIPTION_MAX_CHARS,
            ..options
        },
    );
    if tight_prompt.len() <= MAX_SKILL_CATALOG_BYTES {
        return SkillCatalog {
            prompt: tight_prompt,
            compact: true,
            names_only: false,
            omitted: 0,
        };
    }
    let minimal = RenderOptions {
        names_only: true,
        ..options
    };
    let names_prompt = render_skills(&visible, minimal);
    if names_prompt.len() <= MAX_SKILL_CATALOG_BYTES {
        return SkillCatalog {
            prompt: names_prompt,
            compact,
            names_only: true,
            omitted: 0,
        };
    }
    let (mut low, mut high) = (0usize, visible.len());
    while low < high {
        let count = (low + high).div_ceil(2);
        let candidate = render_skills(
            &visible[..count],
            RenderOptions {
                omitted: visible.len() - count,
                ..minimal
            },
        );
        if candidate.len() <= MAX_SKILL_CATALOG_BYTES {
            low = count;
        } else {
            high = count - 1;
        }
    }
    let omitted = visible.len() - low;
    SkillCatalog {
        prompt: render_skills(&visible[..low], RenderOptions { omitted, ..minimal }),
        compact,
        names_only: true,
        omitted,
    }
}

/// Format skills for inclusion in a system prompt (Agent Skills XML), within the catalog size budget.
pub fn format_skills_catalog_for_prompt(skills: &[Skill], file_read_tool: FileReadTool) -> String {
    build_skill_catalog(skills, file_read_tool).prompt
}

fn sentence_end_after(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        if matches!(bytes[i], b'.' | b'!' | b'?')
            && (i + 1 == bytes.len() || text[i + 1..].starts_with(char::is_whitespace))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    let cut = match head.rfind(' ') {
        Some(idx) if head[..idx].chars().count() > max_chars / 2 => &head[..idx],
        _ => head.as_str(),
    };
    let trimmed =
        cut.trim_end_matches(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';' | ':'));
    format!("{trimmed}\u{2026}")
}

fn one_line_description(description: &str, max_chars: usize) -> String {
    let text = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max_chars {
        return text;
    }
    let trigger_start = EXPLICIT_TRIGGER_CLAUSE
        .captures(&text)
        .and_then(|captures| captures.get(1))
        .or_else(|| {
            VERB_TRIGGER_CLAUSE
                .captures(&text)
                .and_then(|captures| captures.get(1))
        })
        .map(|matched| matched.start());
    let Some(trigger_start) = trigger_start else {
        return truncate(&text, max_chars);
    };
    let clause_end = sentence_end_after(&text, trigger_start)
        .map(|idx| idx + 1)
        .unwrap_or(text.len());
    let clause = &text[trigger_start..clause_end];
    let clause_chars = clause.chars().count();
    if clause_chars >= max_chars {
        return truncate(clause, max_chars);
    }
    let mut room = max_chars - clause_chars;
    let mut fill = |part: &str| -> String {
        if part.is_empty() || room < MIN_FILL_CHARS + 1 {
            return String::new();
        }
        let kept = truncate(part, room - 1);
        room = room.saturating_sub(kept.chars().count() + 1);
        kept
    };
    let lead = fill(text[..trigger_start].trim());
    let rest = fill(text[clause_end..].trim());
    [lead.as_str(), clause, rest.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn compact_skill_line(
    skill: &Skill,
    roots: &[(String, String)],
    description_chars: Option<usize>,
) -> String {
    let dir = skill_root_dir(&skill.file_path);
    let alias = roots
        .iter()
        .find(|(path, _)| *path == dir)
        .map(|(_, alias)| alias.as_str());
    let relative = skill
        .file_path
        .strip_prefix(&format!("{dir}/"))
        .unwrap_or(&skill.file_path);
    let location = if relative == format!("{}/SKILL.md", skill.name) {
        alias.unwrap_or(relative).to_owned()
    } else {
        match alias {
            Some(alias) => format!("{alias}/{relative}"),
            None => relative.to_owned(),
        }
    };
    let head = format!("- {} [{}]", escape_xml(&skill.name), escape_xml(&location));
    match description_chars {
        None => head,
        Some(max_chars) => format!(
            "{head}: {}",
            escape_xml(&one_line_description(&skill.description, max_chars))
        ),
    }
}

fn skill_root_dir(file_path: &str) -> String {
    let parts: Vec<&str> = file_path.split('/').collect();
    parts[..parts.len().saturating_sub(2)].join("/")
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn render_skills(visible: &[&Skill], options: RenderOptions) -> String {
    if visible.is_empty() && options.omitted == 0 {
        return String::new();
    }
    let mut roots: Vec<(String, String)> = Vec::new();
    for skill in visible {
        let dir = skill_root_dir(&skill.file_path);
        if !roots.iter().any(|(path, _)| *path == dir) {
            let alias = format!("r{}", roots.len());
            roots.push((dir, alias));
        }
    }
    let tool_rule = match options.file_read_tool {
        FileReadTool::Read => {
            "Use the read tool to load a skill's file when its description matches the task and its instructions would change the work; keyword overlap or mere availability is not a reason."
        }
        FileReadTool::Bash => {
            "Use bash to load a skill's file when its description matches the task and its instructions would change the work; keyword overlap or mere availability is not a reason."
        }
    };
    let mut lines: Vec<String> = vec![
        "\n\nThe following skills provide specialized instructions for specific tasks.".to_owned(),
        tool_rule.to_owned(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_owned(),
        String::new(),
        "<skill_roots>".to_owned(),
    ];
    for (dir, alias) in &roots {
        lines.push(format!("  <{alias}>{}</{alias}>", escape_xml(dir)));
    }
    let one_line = options.compact || options.names_only;
    lines.push("</skill_roots>".to_owned());
    lines.push(if one_line {
        format!(
            "Each line is `- name [location]{}`. A location `rN` means the file `rN/<name>/SKILL.md`; a longer location is the file's path under that root.",
            if options.names_only {
                ""
            } else {
                ": when to use it"
            }
        )
    } else {
        "A location's `rN/` prefix expands to the matching root above.".to_owned()
    });
    lines.push(String::new());
    lines.push("<available_skills>".to_owned());
    for skill in visible {
        if one_line {
            lines.push(compact_skill_line(
                skill,
                &roots,
                (!options.names_only).then_some(options.description_chars),
            ));
            continue;
        }
        let dir = skill_root_dir(&skill.file_path);
        let alias = roots
            .iter()
            .find(|(path, _)| *path == dir)
            .map(|(_, alias)| alias.as_str())
            .unwrap_or_default();
        let relative = skill
            .file_path
            .strip_prefix(&format!("{dir}/"))
            .unwrap_or(&skill.file_path);
        lines.push("  <skill>".to_owned());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        if !options.names_only {
            lines.push(format!(
                "    <description>{}</description>",
                escape_xml(&skill.description)
            ));
        }
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&format!("{alias}/{relative}"))
        ));
        lines.push("  </skill>".to_owned());
    }
    if options.omitted > 0 {
        lines.push(format!(
            "  <omitted_skills>{}</omitted_skills>",
            options.omitted
        ));
    }
    lines.push("</available_skills>".to_owned());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, description: &str) -> Skill {
        Skill {
            name: name.to_owned(),
            description: description.to_owned(),
            file_path: format!("/skills/{name}/SKILL.md"),
            base_dir: format!("/skills/{name}"),
            source_info: crate::source_info::SourceInfo::default(),
            disable_model_invocation: false,
            provenance: None,
            allowed_set: None,
        }
    }

    #[test]
    fn a_small_catalog_keeps_full_descriptions_and_no_warning() {
        let skills = vec![skill("a", "first line\nsecond line")];
        let catalog = build_skill_catalog(&skills, FileReadTool::Read);
        assert!(!catalog.compact && !catalog.names_only);
        assert!(
            catalog
                .prompt
                .contains("<description>first line\nsecond line</description>")
        );
        assert!(catalog.prompt.contains("<r0>/skills</r0>"));
        assert!(
            catalog
                .prompt
                .contains("<location>r0/a/SKILL.md</location>")
        );
        assert!(warn_if_large_skill_catalog(&skills).is_empty());
        assert_eq!(build_skill_catalog(&[], FileReadTool::Bash).prompt, "");
    }

    #[test]
    fn a_large_catalog_goes_compact_then_names_only_with_an_omitted_count() {
        let many: Vec<Skill> = (0..101)
            .map(|i| skill(&format!("s{i}"), "one\ntwo"))
            .collect();
        let catalog = build_skill_catalog(&many, FileReadTool::Read);
        assert!(catalog.compact && !catalog.names_only);
        assert!(catalog.prompt.contains("- s0 [r0]: one two"));
        assert_eq!(warn_if_large_skill_catalog(&many).len(), 1);

        let huge: Vec<Skill> = (0..3000)
            .map(|i| skill(&format!("skill-with-a-long-name-{i}"), "d"))
            .collect();
        let catalog = build_skill_catalog(&huge, FileReadTool::Read);
        assert!(catalog.names_only);
        assert!(catalog.omitted > 0);
        assert!(catalog.prompt.len() <= MAX_SKILL_CATALOG_BYTES);
        assert!(catalog.prompt.contains(&format!(
            "<omitted_skills>{}</omitted_skills>",
            catalog.omitted
        )));
        assert!(!catalog.prompt.contains("<description>"));
    }

    #[test]
    fn one_line_descriptions_are_cut_with_an_ellipsis_and_keep_explicit_trigger_clauses() {
        let long = "x".repeat(200);
        let cut = one_line_description(&long, ONE_LINE_DESCRIPTION_MAX_CHARS);
        assert_eq!(cut.chars().count(), ONE_LINE_DESCRIPTION_MAX_CHARS);
        assert!(cut.ends_with('\u{2026}'));

        let with_imperative_and_trigger = "Builds release artifacts and packages binaries across targets. Run the formatter for each package before tagging. Verify checksums after upload. Use when asked to cut a release or build native packages.";
        let summary = one_line_description(
            with_imperative_and_trigger,
            TIGHT_ONE_LINE_DESCRIPTION_MAX_CHARS,
        );
        assert!(
            summary.contains("Use when asked to cut a release or build native packages."),
            "got: {summary}"
        );
    }
}
