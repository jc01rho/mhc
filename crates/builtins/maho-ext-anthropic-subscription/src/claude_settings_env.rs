//! The effective `env` block of the Claude Code settings files a query loads (senpi `claude-settings-env.ts`,
//! senpi#2988 item 4).
//!
//! senpi asks the Claude Agent SDK's `resolveSettings` merge engine. The native engine drives the CLI without the
//! SDK, so it reads the same files by the CLI's lookup rules: `user` is `$CLAUDE_CONFIG_DIR/settings.json` (default
//! `~/.claude`), `project` is `<cwd>/.claude/settings.json`, `local` is `<cwd>/.claude/settings.local.json`, and a
//! later source overrides an earlier one key by key whatever order `settingSources` lists them in. Managed policy
//! settings are not read.
use crate::settings::ProviderSettings;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The settings sources in the order the CLI merges them, lowest precedence first.
const SOURCES_LOW_TO_HIGH: [&str; 3] = ["user", "project", "local"];

fn settings_file(source: &str, cwd: &Path, config_dir: &Path) -> PathBuf {
    match source {
        "user" => config_dir.join("settings.json"),
        "project" => cwd.join(".claude").join("settings.json"),
        _ => cwd.join(".claude").join("settings.local.json"),
    }
}

/// Claude Code writes each `env` entry into the process environment, so a scalar counts as its text.
fn env_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// `resolveClaudeSettingsEnv`: the merged `env` of the settings files `sources` names, `None` when none of them sets
/// one. A file that cannot be read or parsed contributes nothing, so a broken settings file never fails a turn.
pub fn resolve_claude_settings_env(
    cwd: &Path,
    config_dir: &Path,
    sources: &[String],
) -> Option<BTreeMap<String, String>> {
    let mut merged = BTreeMap::new();
    for source in SOURCES_LOW_TO_HIGH {
        if !sources.iter().any(|candidate| candidate.as_str() == source) {
            continue;
        }
        let path = settings_file(source, cwd, config_dir);
        let Some(Value::Object(settings)) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        let Some(Value::Object(env)) = settings.get("env") else {
            continue;
        };
        for (name, value) in env {
            if let Some(text) = env_text(value) {
                merged.insert(name.clone(), text);
            }
        }
    }
    (!merged.is_empty()).then_some(merged)
}

/// `resolveQuerySettingsEnv`: the settings env for one stream's queries. An explicit `settingSources` list applies to
/// every lane; otherwise only a preset-append ambient attempt loads user/project settings (the rule
/// `options::setting_sources` uses), so anything else resolves to nothing and no file is read.
pub fn resolve_query_settings_env(
    cwd: &Path,
    settings: &ProviderSettings,
    host: &BTreeMap<String, String>,
) -> Option<BTreeMap<String, String>> {
    let sources: Vec<String> = match settings
        .values
        .get("settingSources")
        .and_then(Value::as_array)
    {
        Some(list) => list
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        None if crate::settings::resolve_prompt_mode(settings).mode == "preset-append" => {
            vec!["user".to_owned(), "project".to_owned()]
        }
        None => Vec::new(),
    };
    if sources.is_empty() {
        return None;
    }
    let config_dir = match host.get("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => dirs::home_dir()?.join(".claude"),
    };
    resolve_claude_settings_env(cwd, &config_dir, &sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(path: &Path, value: &Value) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        std::fs::write(path, value.to_string()).expect("settings file");
    }

    fn sources(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn a_later_source_overrides_an_earlier_one_key_by_key() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        write(
            &home.path().join("settings.json"),
            &json!({"env":{"CLAUDE_CODE_PROMPT_CACHE_TTL":"1h","KEPT":"user","COUNT":3}}),
        );
        write(
            &project.path().join(".claude/settings.json"),
            &json!({"env":{"CLAUDE_CODE_PROMPT_CACHE_TTL":"5m"}}),
        );
        write(
            &project.path().join(".claude/settings.local.json"),
            &json!({"env":{"FORCE_PROMPT_CACHING_5M":"1","FLAG":true,"IGNORED":[1]}}),
        );

        let env = resolve_claude_settings_env(
            project.path(),
            home.path(),
            &sources(&["local", "project", "user"]),
        )
        .expect("env");

        assert_eq!(
            env,
            BTreeMap::from([
                ("CLAUDE_CODE_PROMPT_CACHE_TTL".to_owned(), "5m".to_owned()),
                ("COUNT".to_owned(), "3".to_owned()),
                ("FLAG".to_owned(), "true".to_owned()),
                ("FORCE_PROMPT_CACHING_5M".to_owned(), "1".to_owned()),
                ("KEPT".to_owned(), "user".to_owned()),
            ])
        );
    }

    #[test]
    fn only_the_sources_the_query_loads_are_read() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        write(
            &home.path().join("settings.json"),
            &json!({"env":{"FROM":"user"}}),
        );
        write(
            &project.path().join(".claude/settings.json"),
            &json!({"env":{"FROM":"project"}}),
        );

        let user_only =
            resolve_claude_settings_env(project.path(), home.path(), &sources(&["user"]));
        let none = resolve_claude_settings_env(project.path(), home.path(), &[]);

        assert_eq!(
            user_only,
            Some(BTreeMap::from([("FROM".to_owned(), "user".to_owned())]))
        );
        assert_eq!(none, None);
    }

    #[test]
    fn no_env_block_and_unreadable_files_resolve_to_nothing_instead_of_failing() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        write(
            &home.path().join("settings.json"),
            &json!({"permissions":{}}),
        );
        std::fs::create_dir_all(project.path().join(".claude")).expect("directory");
        std::fs::write(project.path().join(".claude/settings.json"), "{ not json")
            .expect("broken file");
        std::fs::write(
            project.path().join(".claude/settings.local.json"),
            json!({"env":"not an object"}).to_string(),
        )
        .expect("env that is not an object");

        assert_eq!(
            resolve_claude_settings_env(
                project.path(),
                home.path(),
                &sources(&["user", "project", "local"])
            ),
            None
        );
    }

    #[test]
    fn the_stream_resolves_nothing_when_the_attempt_loads_no_settings_sources() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        write(
            &home.path().join("settings.json"),
            &json!({"env":{"CLAUDE_CODE_PROMPT_CACHE_TTL":"5m"}}),
        );
        let host = BTreeMap::from([(
            "CLAUDE_CONFIG_DIR".to_owned(),
            home.path().to_string_lossy().into_owned(),
        )]);
        let load = |provider: Value| {
            crate::settings::load(
                &json!({"anthropicSubscriptionProvider": provider}),
                &Value::Null,
                &BTreeMap::new(),
            )
        };

        let explicit_empty = load(json!({"settingSources":[]}));
        let default_mode = load(json!({}));
        let preset_append = load(json!({"systemPromptMode":"preset-append"}));
        let explicit_user = load(json!({"settingSources":["user"]}));

        assert_eq!(
            resolve_query_settings_env(project.path(), &explicit_empty, &host),
            None
        );
        assert_eq!(
            resolve_query_settings_env(project.path(), &default_mode, &host),
            None
        );
        let expected = Some(BTreeMap::from([(
            "CLAUDE_CODE_PROMPT_CACHE_TTL".to_owned(),
            "5m".to_owned(),
        )]));
        assert_eq!(
            resolve_query_settings_env(project.path(), &preset_append, &host),
            expected
        );
        assert_eq!(
            resolve_query_settings_env(project.path(), &explicit_user, &host),
            expected
        );
    }
}
