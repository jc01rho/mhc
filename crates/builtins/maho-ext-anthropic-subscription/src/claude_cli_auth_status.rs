//! `claude auth status` and `claude auth logout` for one account dir (senpi `claude-cli-auth-status.ts`, senpi#4437).
use crate::claude_cli_process::{
    ClaudeCliRun, DEFAULT_SHORT_COMMAND_TIMEOUT, run_claude_cli_to_exit,
};
use maho_ai::utils::abort::AbortSignal;
use serde_json::{Map, Value};
use std::time::Duration;

/// The local-only profile a CLI-store account shows; never logged or sent to telemetry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaudeCliProfile {
    pub email: Option<String>,
    pub org_name: Option<String>,
    pub subscription_type: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeCliRejection {
    AuthMethod,
    ApiProvider,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudeCliAuthStatus {
    SignedIn {
        profile: ClaudeCliProfile,
    },
    SignedOut,
    /// Logged in, but not as a first-party claude.ai subscription (an API key, Bedrock, Vertex...).
    Rejected {
        reason: ClaudeCliRejection,
    },
    Unreadable,
}

fn string_field(record: &Map<String, Value>, name: &str) -> Option<String> {
    record
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Reads `claude auth status --json` (2.1.306: `{loggedIn, authMethod, apiProvider, email, orgName,
/// subscriptionType, ...}`, exit 0 when logged in, 1 when not). Only a first-party claude.ai login with exit 0 is
/// signed in; an exit code and a `loggedIn` that disagree are unreadable, never trusted.
pub fn parse_claude_cli_auth_status(stdout: &str, exit_code: Option<i32>) -> ClaudeCliAuthStatus {
    let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(stdout) else {
        return ClaudeCliAuthStatus::Unreadable;
    };
    let Some(logged_in) = parsed.get("loggedIn").and_then(Value::as_bool) else {
        return ClaudeCliAuthStatus::Unreadable;
    };
    if !logged_in {
        return if exit_code == Some(0) {
            ClaudeCliAuthStatus::Unreadable
        } else {
            ClaudeCliAuthStatus::SignedOut
        };
    }
    if exit_code != Some(0) {
        return ClaudeCliAuthStatus::Unreadable;
    }
    if parsed.get("authMethod").and_then(Value::as_str) != Some("claude.ai") {
        return ClaudeCliAuthStatus::Rejected {
            reason: ClaudeCliRejection::AuthMethod,
        };
    }
    if parsed.get("apiProvider").and_then(Value::as_str) != Some("firstParty") {
        return ClaudeCliAuthStatus::Rejected {
            reason: ClaudeCliRejection::ApiProvider,
        };
    }
    ClaudeCliAuthStatus::SignedIn {
        profile: ClaudeCliProfile {
            email: string_field(&parsed, "email"),
            org_name: string_field(&parsed, "orgName"),
            subscription_type: string_field(&parsed, "subscriptionType"),
        },
    }
}

pub async fn read_claude_cli_auth_status(
    run: &ClaudeCliRun,
    timeout: Option<Duration>,
) -> ClaudeCliAuthStatus {
    let timeout = timeout.unwrap_or(DEFAULT_SHORT_COMMAND_TIMEOUT);
    match run_claude_cli_to_exit(run, &["auth", "status", "--json"], timeout, None).await {
        Some(exit) => parse_claude_cli_auth_status(&exit.stdout, exit.exit_code),
        None => ClaudeCliAuthStatus::Unreadable,
    }
}

/// `claude auth logout` for one account dir: deletes that dir's credential store entry.
pub async fn logout_claude_cli(
    run: &ClaudeCliRun,
    timeout: Option<Duration>,
    signal: Option<&AbortSignal>,
) -> bool {
    let timeout = timeout.unwrap_or(DEFAULT_SHORT_COMMAND_TIMEOUT);
    run_claude_cli_to_exit(run, &["auth", "logout"], timeout, signal)
        .await
        .is_some_and(|exit| exit.exit_code == Some(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_in(profile: ClaudeCliProfile) -> ClaudeCliAuthStatus {
        ClaudeCliAuthStatus::SignedIn { profile }
    }

    #[test]
    fn only_a_first_party_claude_ai_login_with_exit_zero_is_signed_in() {
        let claude_ai = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"user@example.com","orgName":"Fixture Org","subscriptionType":"max","orgId":"org-1"}"#;
        assert_eq!(
            parse_claude_cli_auth_status(claude_ai, Some(0)),
            signed_in(ClaudeCliProfile {
                email: Some("user@example.com".to_owned()),
                org_name: Some("Fixture Org".to_owned()),
                subscription_type: Some("max".to_owned()),
            })
        );
        let bare =
            r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":""}"#;
        assert_eq!(
            parse_claude_cli_auth_status(bare, Some(0)),
            signed_in(ClaudeCliProfile::default())
        );
    }

    #[test]
    fn a_login_that_is_not_a_claude_ai_subscription_is_rejected_with_the_reason() {
        let api_key = r#"{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty"}"#;
        assert_eq!(
            parse_claude_cli_auth_status(api_key, Some(0)),
            ClaudeCliAuthStatus::Rejected {
                reason: ClaudeCliRejection::AuthMethod
            }
        );
        let bedrock = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"bedrock"}"#;
        assert_eq!(
            parse_claude_cli_auth_status(bedrock, Some(0)),
            ClaudeCliAuthStatus::Rejected {
                reason: ClaudeCliRejection::ApiProvider
            }
        );
    }

    #[test]
    fn an_exit_code_and_a_logged_in_flag_that_disagree_are_unreadable_never_trusted() {
        let in_flag = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}"#;
        let out_flag = r#"{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}"#;
        assert_eq!(
            parse_claude_cli_auth_status(in_flag, Some(1)),
            ClaudeCliAuthStatus::Unreadable
        );
        assert_eq!(
            parse_claude_cli_auth_status(in_flag, None),
            ClaudeCliAuthStatus::Unreadable
        );
        assert_eq!(
            parse_claude_cli_auth_status(out_flag, Some(0)),
            ClaudeCliAuthStatus::Unreadable
        );
        assert_eq!(
            parse_claude_cli_auth_status(out_flag, Some(1)),
            ClaudeCliAuthStatus::SignedOut
        );
    }

    #[test]
    fn output_that_is_not_a_status_object_is_unreadable() {
        for stdout in ["not json\n", "[]", "{}", r#"{"loggedIn":"yes"}"#, ""] {
            assert_eq!(
                parse_claude_cli_auth_status(stdout, Some(0)),
                ClaudeCliAuthStatus::Unreadable,
                "{stdout:?}"
            );
        }
    }
}

#[cfg(all(test, unix))]
mod process_tests {
    use super::*;
    use crate::claude_cli_test_support::Fixture;

    async fn status_of(mode: &str) -> ClaudeCliAuthStatus {
        let fixture = Fixture::new(mode, &[]);
        std::fs::create_dir_all(&fixture.run.config_dir).expect("account dir");
        read_claude_cli_auth_status(&fixture.run, None).await
    }

    #[tokio::test]
    async fn the_status_is_read_from_the_cli_json_through_the_real_spawn_path() {
        assert_eq!(
            status_of("status-claudeai").await,
            ClaudeCliAuthStatus::SignedIn {
                profile: ClaudeCliProfile {
                    email: Some("user@example.com".to_owned()),
                    org_name: Some("Fixture Org".to_owned()),
                    subscription_type: Some("max".to_owned()),
                }
            }
        );
        assert_eq!(
            status_of("status-apikey").await,
            ClaudeCliAuthStatus::Rejected {
                reason: ClaudeCliRejection::AuthMethod
            }
        );
        assert_eq!(
            status_of("status-bedrock").await,
            ClaudeCliAuthStatus::Rejected {
                reason: ClaudeCliRejection::ApiProvider
            }
        );
        assert_eq!(
            status_of("status-loggedout").await,
            ClaudeCliAuthStatus::SignedOut
        );
        assert_eq!(
            status_of("status-garbage").await,
            ClaudeCliAuthStatus::Unreadable
        );
    }

    #[tokio::test]
    async fn the_status_is_unreadable_when_the_cli_cannot_start() {
        let mut fixture = Fixture::new("status-claudeai", &[]);
        fixture.run.executable = fixture.run.config_dir.join("missing-claude");
        assert_eq!(
            read_claude_cli_auth_status(&fixture.run, None).await,
            ClaudeCliAuthStatus::Unreadable
        );
    }

    #[tokio::test]
    async fn logout_reports_the_exit_code_and_gives_up_on_a_cli_that_never_exits() {
        let run_in = |mode: &str| {
            let fixture = Fixture::new(mode, &[]);
            std::fs::create_dir_all(&fixture.run.config_dir).expect("account dir");
            fixture
        };
        let signed_out = run_in("logout-ok");
        let refused = run_in("logout-fail");
        let hung = run_in("logout-hang");

        assert!(logout_claude_cli(&signed_out.run, None, None).await);
        assert!(!logout_claude_cli(&refused.run, None, None).await);
        assert!(!logout_claude_cli(&hung.run, Some(Duration::from_millis(200)), None).await);
    }

    #[tokio::test]
    async fn an_aborted_signal_ends_a_short_command_without_spawning_it() {
        let fixture = Fixture::new("logout-ok", &[]);
        std::fs::create_dir_all(&fixture.run.config_dir).expect("account dir");
        let controller = maho_ai::utils::abort::AbortController::new();
        controller.abort(None);
        let signal = controller.signal();

        assert!(!logout_claude_cli(&fixture.run, None, Some(&signal)).await);
        assert_eq!(fixture.recorded(), "");
    }
}
