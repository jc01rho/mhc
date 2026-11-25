//! The skills a created thread follows from its first turn.
//!
//! Upstream counterpart: `thread/initial-skills.ts` (`checkInitialSkills`). The session-side registration
//! (`registerInitialSkills`, its `before_agent_start` hook) is not ported.

pub const INITIAL_SKILLS_CONTEXT_KEY: &str = "initial_skills";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialSkillsCheck {
    Ok { names: Vec<String> },
    Invalid { names: Vec<String> },
}

fn is_skill_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    name.chars().count() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
}

/// Names only: a path, raw text or anything not shaped like a skill name is `Invalid`. Duplicates collapse, order
/// kept. Whether a name is installed is the new session's call, which applies the names its registry holds.
pub fn check_initial_skills(names: &[String]) -> InitialSkillsCheck {
    let mut unique: Vec<String> = Vec::new();
    for name in names {
        if !unique.contains(name) {
            unique.push(name.clone());
        }
    }
    let invalid: Vec<String> = unique
        .iter()
        .filter(|name| !is_skill_name(name))
        .cloned()
        .collect();
    if invalid.is_empty() {
        InitialSkillsCheck::Ok { names: unique }
    } else {
        InitialSkillsCheck::Invalid { names: invalid }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn skill_names_pass_and_duplicates_collapse_in_order() {
        assert_eq!(
            check_initial_skills(&names(&[
                "programming",
                "gateway-lead",
                "programming",
                "x1"
            ])),
            InitialSkillsCheck::Ok {
                names: names(&["programming", "gateway-lead", "x1"])
            }
        );
    }

    #[test]
    fn a_path_text_or_oversized_name_is_invalid_and_named() {
        let long = "a".repeat(65);
        let too_long = vec![long.clone()];
        assert_eq!(
            check_initial_skills(&too_long),
            InitialSkillsCheck::Invalid { names: too_long }
        );
        assert_eq!(
            check_initial_skills(&names(&[
                "ok",
                "../etc/passwd",
                "Upper",
                "has space",
                "-leading",
                ""
            ])),
            InitialSkillsCheck::Invalid {
                names: names(&["../etc/passwd", "Upper", "has space", "-leading", ""])
            }
        );
        assert!(matches!(
            check_initial_skills(&names(&[&"a".repeat(64)])),
            InitialSkillsCheck::Ok { .. }
        ));
    }
}
