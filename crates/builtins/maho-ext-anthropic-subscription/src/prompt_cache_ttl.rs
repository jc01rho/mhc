//! Port of senpi `anthropic-subscription/prompt-cache-ttl.ts` (pin 903b8336).
use crate::auth_lane::TokenInjection;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Prompt-cache lifetimes Claude Code accepts for `CLAUDE_CODE_PROMPT_CACHE_TTL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptCacheTtl {
    FiveMinutes,
    OneHour,
}

impl PromptCacheTtl {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FiveMinutes => "5m",
            Self::OneHour => "1h",
        }
    }
}

pub const PROMPT_CACHE_TTL_ENV: &str = "CLAUDE_CODE_PROMPT_CACHE_TTL";
/// Claude Code reads this before `CLAUDE_CODE_PROMPT_CACHE_TTL`: when set, every lifetime it writes is 5 minutes.
pub const FORCE_PROMPT_CACHING_5M_ENV: &str = "FORCE_PROMPT_CACHING_5M";

fn is_truthy(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        ["1", "true", "yes", "on"].contains(&value.trim().to_lowercase().as_str())
    })
}

fn parse_prompt_cache_ttl(value: Option<&str>) -> Option<PromptCacheTtl> {
    match value {
        Some("5m") => Some(PromptCacheTtl::FiveMinutes),
        Some("1h") => Some(PromptCacheTtl::OneHour),
        _ => None,
    }
}

fn option_env<'a>(options: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    options.get("env")?.get(key)?.as_str()
}

/// Pin Claude Code's prompt-cache lifetime for a one-shot query and return it, so the history
/// breakpoint uses the same lifetime (senpi#2982). `FORCE_PROMPT_CACHING_5M` wins; then a lifetime
/// from a loaded settings file, then an explicit `CLAUDE_CODE_PROMPT_CACHE_TTL` from the query env,
/// then from the process environment. Managed subscription lanes pin 1 hour; the ambient lane pins
/// nothing unless a lifetime is set explicitly, and the caller adds no breakpoint then.
///
/// `settings_env` is the effective `env` of the settings files this attempt loads (senpi#2988):
/// Claude Code writes each settings source's env over the process env, and launch-env keys are only
/// protected for host-orchestrated sessions, so a settings value beats the process env and
/// `options.env`, and beats the assumption about Claude Code's lane default.
pub fn pin_one_shot_prompt_cache_ttl(
    options: &mut Map<String, Value>,
    auth_lane: TokenInjection,
    environment: &BTreeMap<String, String>,
    settings_env: Option<&BTreeMap<String, String>>,
) -> Option<PromptCacheTtl> {
    let setting = |name: &str| {
        settings_env
            .and_then(|env| env.get(name))
            .map(String::as_str)
    };
    let forced_5m = is_truthy(option_env(options, FORCE_PROMPT_CACHING_5M_ENV))
        || is_truthy(
            environment
                .get(FORCE_PROMPT_CACHING_5M_ENV)
                .map(String::as_str),
        )
        || is_truthy(setting(FORCE_PROMPT_CACHING_5M_ENV));
    let ttl = (if forced_5m {
        Some(PromptCacheTtl::FiveMinutes)
    } else {
        None
    })
    .or_else(|| parse_prompt_cache_ttl(setting(PROMPT_CACHE_TTL_ENV)))
    .or_else(|| parse_prompt_cache_ttl(option_env(options, PROMPT_CACHE_TTL_ENV)))
    .or_else(|| parse_prompt_cache_ttl(environment.get(PROMPT_CACHE_TTL_ENV).map(String::as_str)))
    .or(if auth_lane == TokenInjection::Ambient {
        None
    } else {
        Some(PromptCacheTtl::OneHour)
    })?;
    let env = options
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()));
    if !env.is_object() {
        *env = Value::Object(Map::new());
    }
    env.as_object_mut().expect("env object").insert(
        PROMPT_CACHE_TTL_ENV.into(),
        Value::String(ttl.as_str().into()),
    );
    Some(ttl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect()
    }

    #[test]
    fn pins_lane_with_environment_for_both_claude_code_and_the_breakpoint() {
        let cases = [
            (TokenInjection::OAuthSlots, env(&[]), "1h"),
            (TokenInjection::ConfigDir, env(&[]), "1h"),
            (
                TokenInjection::OAuthSlots,
                env(&[(PROMPT_CACHE_TTL_ENV, "5m")]),
                "5m",
            ),
            (
                TokenInjection::Ambient,
                env(&[(PROMPT_CACHE_TTL_ENV, "1h")]),
                "1h",
            ),
            (
                TokenInjection::OAuthSlots,
                env(&[(FORCE_PROMPT_CACHING_5M_ENV, "1")]),
                "5m",
            ),
            (
                TokenInjection::ConfigDir,
                env(&[
                    (FORCE_PROMPT_CACHING_5M_ENV, "true"),
                    (PROMPT_CACHE_TTL_ENV, "1h"),
                ]),
                "5m",
            ),
            (
                TokenInjection::Ambient,
                env(&[(FORCE_PROMPT_CACHING_5M_ENV, "1")]),
                "5m",
            ),
        ];
        for (lane, environment, expected) in cases {
            let mut options = json!({"env":{"CLAUDE_CODE_TOTAL_TOKENS_REMINDER":"off"}})
                .as_object()
                .unwrap()
                .clone();
            let ttl = pin_one_shot_prompt_cache_ttl(&mut options, lane, &environment, None);
            assert_eq!(ttl.map(PromptCacheTtl::as_str), Some(expected));
            assert_eq!(
                options["env"],
                json!({"CLAUDE_CODE_TOTAL_TOKENS_REMINDER":"off","CLAUDE_CODE_PROMPT_CACHE_TTL":expected})
            );
        }
    }

    #[test]
    fn pins_nothing_on_the_ambient_lane() {
        let mut options = Map::new();
        let ttl = pin_one_shot_prompt_cache_ttl(
            &mut options,
            TokenInjection::Ambient,
            &env(&[(PROMPT_CACHE_TTL_ENV, "10m")]),
            None,
        );
        assert_eq!(ttl, None);
        assert!(options.get("env").is_none());
    }

    #[test]
    fn query_env_force_flag_wins_over_explicit_ttl() {
        let mut options =
            json!({"env":{"FORCE_PROMPT_CACHING_5M":" YES ","CLAUDE_CODE_PROMPT_CACHE_TTL":"1h"}})
                .as_object()
                .unwrap()
                .clone();
        let ttl = pin_one_shot_prompt_cache_ttl(
            &mut options,
            TokenInjection::OAuthSlots,
            &env(&[]),
            None,
        );
        assert_eq!(ttl, Some(PromptCacheTtl::FiveMinutes));
        assert_eq!(options["env"][PROMPT_CACHE_TTL_ENV], "5m");
    }

    fn pin_case(
        lane: TokenInjection,
        query_env: &[(&str, &str)],
        process_env: &[(&str, &str)],
        settings_env: &[(&str, &str)],
        expected: Option<&str>,
    ) {
        let mut options = Map::new();
        options.insert(
            "env".into(),
            Value::Object(
                query_env
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), json!(value)))
                    .collect(),
            ),
        );
        let before = options["env"].clone();
        let ttl = pin_one_shot_prompt_cache_ttl(
            &mut options,
            lane,
            &env(process_env),
            Some(&env(settings_env)),
        );
        assert_eq!(ttl.map(PromptCacheTtl::as_str), expected);
        match expected {
            None => assert_eq!(options["env"], before),
            Some(lifetime) => {
                let mut pinned = before.clone();
                pinned[PROMPT_CACHE_TTL_ENV] = json!(lifetime);
                assert_eq!(options["env"], pinned);
            }
        }
    }

    #[test]
    fn a_lifetime_set_in_a_loaded_settings_file_is_visible_to_the_pin() {
        let one_hour = (PROMPT_CACHE_TTL_ENV, "1h");
        let five_minutes = (PROMPT_CACHE_TTL_ENV, "5m");
        let force = (FORCE_PROMPT_CACHING_5M_ENV, "1");
        let oauth = TokenInjection::OAuthSlots;
        let ambient = TokenInjection::Ambient;
        pin_case(oauth, &[], &[], &[force], Some("5m"));
        pin_case(oauth, &[], &[], &[five_minutes], Some("5m"));
        pin_case(oauth, &[], &[one_hour], &[five_minutes], Some("5m"));
        pin_case(oauth, &[one_hour], &[], &[five_minutes], Some("5m"));
        pin_case(oauth, &[force], &[], &[one_hour], Some("5m"));
        pin_case(ambient, &[], &[], &[five_minutes], Some("5m"));
        pin_case(ambient, &[], &[], &[force], Some("5m"));
        pin_case(
            oauth,
            &[],
            &[],
            &[(PROMPT_CACHE_TTL_ENV, "10m")],
            Some("1h"),
        );
        pin_case(ambient, &[], &[], &[], None);
    }
}
