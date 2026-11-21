//! Port of `packages/omo-senpi/src/components/skill-pointers/pointer-text.ts`.

use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use std::sync::LazyLock;

pub struct SkillCompanion {
    pub skill_name: &'static str,
    pub custom_type: &'static str,
    pub expanded_block_pattern: LazyLock<Regex>,
    pub role: &'static str,
    pub instruction: &'static str,
}

pub struct SkillPointerTarget {
    pub skill_name: &'static str,
    pub custom_type: &'static str,
    pub pattern: LazyLock<FancyRegex>,
    pub expanded_block_pattern: LazyLock<Regex>,
    pub instruction: &'static str,
    pub extra: Option<fn(Option<&str>) -> String>,
    pub companions: &'static [&'static SkillCompanion],
    /// A target with no packaged skill: the pointer is this text instead of a SKILL.md read.
    pub pointer: Option<&'static str>,
}

/// The guard every conditional pointer ends with: a keyword proves a mention, so a mention that is
/// only talk is ignored.
pub fn ignore_when_only_discussed(subject: &str) -> String {
    format!(
        "If {subject} is only being discussed, quoted, or relayed from another session, ignore this pointer."
    )
}

/// A keyword proves a mention, not a request to run the workflow.
pub fn skill_pointer(
    target: &SkillPointerTarget,
    session_scope: Option<&str>,
    skills_root: &str,
) -> String {
    let extra = match target.extra {
        Some(f) => f(session_scope),
        None => String::new(),
    };
    let guard = ignore_when_only_discussed(target.skill_name);
    format!(
        "<omo-{}-pointer>This message mentions {}. If the user of this session is asking to run {}, read the {} skill at {}{}/SKILL.md with the read tool and follow it: {}. {}{}</omo-{}-pointer>",
        target.skill_name,
        target.skill_name,
        target.skill_name,
        target.skill_name,
        skills_root,
        target.skill_name,
        target.instruction,
        guard,
        extra,
        target.skill_name,
    )
}

pub fn companion_pointer(
    parent: &SkillPointerTarget,
    companion: &SkillCompanion,
    skills_root: &str,
) -> String {
    let guard = ignore_when_only_discussed(parent.skill_name);
    format!(
        "<omo-{}-pointer>{} {}. If the user of this session is asking to run {}: {} {}</omo-{}-pointer>",
        companion.skill_name,
        parent.skill_name,
        companion.role,
        parent.skill_name,
        companion_condition(parent, companion, skills_root),
        guard,
        companion.skill_name,
    )
}

/// The companion is read only after the parent's triage marks the run as external browser
/// collection; local-only, codebase-only, or search-only runs never load it (ULW-9).
pub fn companion_condition(
    parent: &SkillPointerTarget,
    companion: &SkillCompanion,
    skills_root: &str,
) -> String {
    format!(
        "Do not read the {} skill up front: read the {} skill first and write its triage line. Only when that triage says `External: yes` and `Collection: browser`, read the {} skill at {}{}/SKILL.md with the read tool and {}. For local-only, codebase-only, or search-only research, never read it.",
        companion.skill_name,
        parent.skill_name,
        companion.skill_name,
        skills_root,
        companion.skill_name,
        companion.instruction,
    )
}
