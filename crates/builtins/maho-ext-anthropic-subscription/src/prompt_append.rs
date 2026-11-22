pub const REGION_START: &str = "<!--senpi:project-rules:1:start-->";
pub const REGION_END: &str = "<!--senpi:project-rules:1:end-->";
// PERF-DIVERGENCE(omo anthropic-subscription prompt-append.ts): upstream builds these config-dependent regexes per call; we memoize the three compiled regexes per config directory. Matches are identical.
fn config_regexes(config: &str) -> Option<std::sync::Arc<[regex::Regex; 3]>> {
    static CACHE: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<[regex::Regex; 3]>>>,
    > = std::sync::LazyLock::new(Default::default);
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(found) = cache.get(config) {
        return Some(found.clone());
    }
    let built = std::sync::Arc::new([
        regex::Regex::new(&format!(r"(?i)~/{config}\b")).ok()?,
        regex::Regex::new(&format!(r#"(^|[\s'"`])\.{config}/"#)).ok()?,
        regex::Regex::new(&format!(r"(?i)\b{config}\b")).ok()?,
    ]);
    cache.insert(config.to_owned(), built.clone());
    Some(built)
}
pub fn agents(
    cwd: &std::path::Path,
    agent_dir: &std::path::Path,
    config_directory: &str,
) -> Option<String> {
    let mut current = std::path::absolute(cwd).ok()?;
    let path = loop {
        let candidate = current.join("AGENTS.md");
        if candidate.exists() {
            break candidate;
        }
        if !current.pop() {
            break agent_dir.join("AGENTS.md");
        }
    };
    let content = std::fs::read_to_string(path).ok()?;
    let config = regex::escape(config_directory.trim_start_matches('.'));
    let regexes = config_regexes(&config)?;
    let [home, dot, bare] = &*regexes;
    let content = home.replace_all(content.trim(), "~/.claude");
    let content = dot.replace_all(&content, "${1}.claude/");
    let content = bare.replace_all(&content, "environment").into_owned();
    if content.is_empty() {
        None
    } else {
        Some(format!("# CLAUDE.md\n\n{content}"))
    }
}
pub fn project_rules(prompt: Option<&str>) -> Option<&str> {
    let prompt = prompt?;
    let mut search = 0;
    while search <= prompt.len() {
        let start = search + prompt[search..].find(REGION_START)? + REGION_START.len();
        let end = start + prompt[start..].find(REGION_END)?;
        let region = prompt[start..end].trim();
        if region.starts_with("<project_rules>\n## Project Instructions\n")
            && region.ends_with("\n</project_rules>")
        {
            return Some(region);
        }
        search = start;
    }
    None
}
fn claude_skills_path(
    path: &str,
    global_root: &std::path::Path,
    project_root: &std::path::Path,
) -> String {
    let real_source = std::fs::canonicalize(path).ok();
    let global_claude = global_root
        .parent()
        .and_then(|p| p.parent())
        .or_else(|| global_root.parent())
        .map(|h| h.join(".claude").join("skills"));
    let project_claude = project_root
        .parent()
        .and_then(|p| p.parent())
        .map(|c| c.join(".claude").join("skills"));
    for (root, claude_display, claude_fs) in [
        (global_root, "~/.claude/skills", global_claude),
        (project_root, ".claude/skills", project_claude),
    ] {
        let Ok(inside) = std::path::Path::new(path).strip_prefix(root) else {
            continue;
        };
        let suffix = inside
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches('/')
            .to_owned();
        if let (Some(real_src), Some(claude_base)) = (&real_source, &claude_fs) {
            let candidate_fs = if suffix.is_empty() {
                claude_base.clone()
            } else {
                claude_base.join(&suffix)
            };
            if std::fs::canonicalize(&candidate_fs).ok().as_ref() != Some(real_src) {
                return path.to_owned();
            }
        }
        return if suffix.is_empty() {
            claude_display.to_owned()
        } else {
            format!("{claude_display}/{suffix}")
        };
    }
    path.to_owned()
}
pub fn skills(
    prompt: Option<&str>,
    global_root: &std::path::Path,
    project_root: &std::path::Path,
) -> Option<String> {
    let prompt = prompt?;
    let start =
        prompt.find("The following skills provide specialized instructions for specific tasks.")?;
    let end = start + prompt[start..].find("</available_skills>")? + "</available_skills>".len();
    let block = prompt[start..end].trim();
    static ROOTS_TABLE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?s)<skill_roots>.*?</skill_roots>").expect("skill_roots regex")
    });
    static ROOT_ROW: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"<(r\d+)>([^<]+)</(r\d+)>").expect("root row regex")
    });
    static LOCATION: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"<location>([^<]+)</location>").expect("location regex")
    });
    let with_roots = ROOTS_TABLE.replace_all(block, |table: &regex::Captures<'_>| {
        ROOT_ROW
            .replace_all(&table[0], |row: &regex::Captures<'_>| {
                if row[1] != row[3] {
                    return row[0].to_owned();
                }
                let alias = &row[1];
                let rewritten = claude_skills_path(&row[2], global_root, project_root);
                format!("<{alias}>{rewritten}</{alias}>")
            })
            .into_owned()
    });
    Some(
        LOCATION
            .replace_all(&with_roots, |capture: &regex::Captures<'_>| {
                let rewritten = claude_skills_path(&capture[1], global_root, project_root);
                format!("<location>{rewritten}</location>")
            })
            .into_owned(),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearest_agents_wins_and_empty_file_does_not_fall_back() {
        let directory = tempfile::tempdir().expect("dir");
        let global = directory.path().join("global");
        let project = directory.path().join("project");
        let child = project.join("child");
        std::fs::create_dir_all(&global).expect("global");
        std::fs::create_dir_all(&child).expect("child");
        std::fs::write(global.join("AGENTS.md"), "global").expect("global file");
        std::fs::write(
            project.join("AGENTS.md"),
            " ~/omo/skills ~/.omo/skills .omo/file omo ",
        )
        .expect("project file");
        assert_eq!(
            agents(&child, &global, ".omo").expect("agents"),
            "# CLAUDE.md\n\n~/.claude/skills ~/.environment/skills .claude/file environment"
        );
        std::fs::write(child.join("AGENTS.md"), "  ").expect("empty");
        assert!(agents(&child, &global, ".omo").is_none());
    }
    #[test]
    fn reserved_region_rejects_false_candidate_and_unterminated_region() {
        let valid = "<project_rules>\n## Project Instructions\nrule\n</project_rules>";
        let prompt = format!(
            "{REGION_START}invalid{REGION_END} unrelated {REGION_START}{valid}{REGION_END} tail"
        );
        assert_eq!(project_rules(Some(&prompt)), Some(valid));
        assert_eq!(project_rules(Some(&format!("{REGION_START}{valid}"))), None);
    }
    #[test]
    fn skill_paths_rewrite_only_known_roots_and_require_end_marker() {
        let prompt = "The following skills provide specialized instructions for specific tasks.\n<available_skills><location>/global/skills/test</location><location>/project/.senpi/skills/local</location><location>/other/test</location></available_skills>tail";
        let result = skills(
            Some(prompt),
            std::path::Path::new("/global/skills"),
            std::path::Path::new("/project/.senpi/skills"),
        )
        .expect("skills");
        assert!(result.contains("<location>~/.claude/skills/test</location>"));
        assert!(result.contains("<location>.claude/skills/local</location>"));
        assert!(result.contains("<location>/other/test</location>"));
        assert!(!result.ends_with("tail"));
        assert!(
            skills(
                Some("The following skills provide specialized instructions for specific tasks."),
                std::path::Path::new("/global"),
                std::path::Path::new("/project")
            )
            .is_none()
        );
    }
    #[test]
    fn skill_roots_table_rewrites_only_when_target_is_not_a_divergent_real_file() {
        let prompt = "The following skills provide specialized instructions for specific tasks.\n<skill_roots>\n  <r0>/global/skills</r0>\n  <r1>/project/.senpi/skills</r1>\n</skill_roots>\n<available_skills><location>r0/demo/SKILL.md</location></available_skills>";
        let result = skills(
            Some(prompt),
            std::path::Path::new("/global/skills"),
            std::path::Path::new("/project/.senpi/skills"),
        )
        .expect("skills");
        assert!(result.contains("<r0>~/.claude/skills</r0>"));
        assert!(result.contains("<r1>.claude/skills</r1>"));

        let dir = tempfile::tempdir().expect("tempdir");
        let project_skills = dir.path().join(".omo").join("skills");
        std::fs::create_dir_all(&project_skills).expect("create project skills");
        let real_prompt = format!(
            "The following skills provide specialized instructions for specific tasks.\n<skill_roots>\n  <r0>{}</r0>\n</skill_roots>\n<available_skills><location>r0/a/SKILL.md</location></available_skills>",
            project_skills.display()
        );
        let kept = skills(
            Some(&real_prompt),
            std::path::Path::new("/nonexistent/global/skills"),
            &project_skills,
        )
        .expect("skills");
        assert!(kept.contains(&format!("<r0>{}</r0>", project_skills.display())));
    }
}
