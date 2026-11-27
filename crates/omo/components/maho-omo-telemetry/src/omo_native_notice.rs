use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::omo_native_consent_marker::{
    ConsentAnswer, read_omo_native_consent, record_omo_native_consent,
};
use crate::omo_native_session::{TempDirDeps, not_automated};

const CONSENT_TITLE: &str = "Anonymous telemetry";

pub fn consent_question() -> String {
    format!(
        "mhc sends anonymous usage telemetry (no prompts, code, file paths, or account data) to {} to improve the product. Turn it off any time with DO_NOT_TRACK=1, or set telemetry.enabled to false in ~/.omo/omo.jsonc. Details: {}\n\nAllow anonymous usage telemetry?",
        telemetry_core::telemetry_destinations_phrase(),
        telemetry_core::TELEMETRY_PRIVACY_POLICY_URL
    )
}

fn may_ask(
    env: &telemetry_core::TelemetryEnv,
    enabled: &crate::omo_native_component::ConfigEnabled,
    cwd: &std::path::Path,
) -> bool {
    let product = crate::product_identity::create_omo_native_product_config();
    // SAFETY-DIVERGENCE(oh-my-openagent packages/omo-senpi/src/components/telemetry/omo-native-notice.ts): upstream asks every interactive machine; mhc telemetry stays opt-in (docs/divergences.md SD-6), so the question is put only to a user whose config already turned telemetry on.
    enabled(cwd)
        && not_automated(env, &TempDirDeps::default())
        && telemetry_core::is_telemetry_client_enabled(
            &telemetry_core::TelemetryClientEnabledInput::for_product(Some(env), &product),
        )
}

/// The first-run consent question: an interactive session is asked once and the answer is persisted before any
/// telemetry decision reads it. A session with no UI (headless, print, RPC) asks nothing and writes nothing, so
/// such runs count as not consented.
pub fn register_omo_native_notice(
    api: &mut maho_ext_api::ExtensionApi,
    env: telemetry_core::TelemetryEnv,
    state_dir: PathBuf,
    enabled: crate::omo_native_component::ConfigEnabled,
    diagnostics: Option<telemetry_core::TelemetryDiagnostics>,
) {
    let reported = Arc::new(AtomicBool::new(false));
    let report_once: Arc<dyn Fn(telemetry_core::TelemetryError) + Send + Sync> =
        Arc::new(move |error: telemetry_core::TelemetryError| {
            if reported.swap(true, Ordering::SeqCst) {
                return;
            }
            if let Some(diagnostics) = &diagnostics {
                diagnostics(&telemetry_core::TelemetryDiagnosticInput {
                    event: telemetry_core::TelemetryDiagnosticEvent::TelemetryCaptureFailed,
                    source: "omo-native-notice".into(),
                    error: Some(error),
                    error_kind: Some(telemetry_core::TelemetryDiagnosticErrorKind::Error),
                    entry_kind: None,
                    error_code: None,
                    count: None,
                });
            }
        });
    api.on(
        maho_ext_api::EventKind::SessionStart,
        Arc::new(move |_, ctx| {
            let env = env.clone();
            let state_dir = state_dir.clone();
            let enabled = Arc::clone(&enabled);
            let report_once = Arc::clone(&report_once);
            Box::pin(async move {
                if !ctx.has_ui
                    || read_omo_native_consent(&state_dir).is_some()
                    || !may_ask(&env, &enabled, &ctx.cwd)
                {
                    return Ok(maho_ext_api::EventResult::None);
                }
                let question = consent_question();
                let allowed = ctx
                    .ui
                    .confirm(
                        CONSENT_TITLE,
                        &question,
                        maho_ext_api::ExtensionUiDialogOptions::default(),
                    )
                    .await;
                let answer = if allowed {
                    ConsentAnswer::Accepted
                } else {
                    ConsentAnswer::Declined
                };
                if let Err(error) = record_omo_native_consent(&state_dir, answer) {
                    report_once(telemetry_core::TelemetryError::new(error.to_string()));
                }
                Ok(maho_ext_api::EventResult::None)
            })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry_test_support::*;

    fn start() -> maho_ext_api::ExtensionEvent {
        maho_ext_api::ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {
            sizes_only: false,
            reason: maho_ext_api::SessionReason::Startup,
            initial_model_provenance: None,
            previous_session_file: None,
            attended: None,
        })
    }

    fn registered(
        env: telemetry_core::TelemetryEnv,
        state_dir: PathBuf,
        enabled: bool,
        diagnostics: Option<telemetry_core::TelemetryDiagnostics>,
    ) -> maho_ext_api::ExtensionApi {
        let mut api = api();
        register_omo_native_notice(
            &mut api,
            env,
            state_dir,
            Arc::new(move |_| enabled),
            diagnostics,
        );
        api
    }

    #[test]
    fn no_audit_commands() {
        let t = tempfile::tempdir().unwrap();
        let api = registered(env(t.path()), t.path().join("notice"), true, None);
        assert!(api.registered.commands.is_empty());
    }

    #[tokio::test]
    async fn the_question_is_asked_once_and_the_answer_recorded() {
        let t = tempfile::tempdir().unwrap();
        let state = t.path().join("notice");
        let api = registered(env(t.path()), state.clone(), true, None);
        let (ctx, questions) = context_with_confirm(t.path(), "s", true);
        dispatch(&api, start(), &ctx).await;
        dispatch(&api, start(), &ctx).await;
        let questions = questions.lock().unwrap();
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].0, "Anonymous telemetry");
        assert!(questions[0].1.contains("https://t.omo.dev"));
        assert!(questions[0].1.contains("https://us.i.posthog.com"));
        assert_eq!(
            read_omo_native_consent(&state),
            Some(ConsentAnswer::Accepted)
        );
    }

    #[tokio::test]
    async fn a_refusal_is_recorded_and_never_asked_again() {
        let t = tempfile::tempdir().unwrap();
        let state = t.path().join("notice");
        let api = registered(env(t.path()), state.clone(), true, None);
        let (refusing, first) = context_with_confirm(t.path(), "s", false);
        let (agreeing, second) = context_with_confirm(t.path(), "s", true);
        dispatch(&api, start(), &refusing).await;
        dispatch(&api, start(), &agreeing).await;
        assert_eq!(first.lock().unwrap().len(), 1);
        assert!(second.lock().unwrap().is_empty());
        assert_eq!(
            read_omo_native_consent(&state),
            Some(ConsentAnswer::Declined)
        );
    }

    #[tokio::test]
    async fn a_session_without_a_ui_asks_and_records_nothing() {
        let t = tempfile::tempdir().unwrap();
        let state = t.path().join("notice");
        let api = registered(env(t.path()), state.clone(), true, None);
        let (mut ctx, questions) = context_with_confirm(t.path(), "s", true);
        ctx.has_ui = false;
        dispatch(&api, start(), &ctx).await;
        assert!(questions.lock().unwrap().is_empty());
        assert!(!state.exists());
    }

    #[tokio::test]
    async fn optout_asks_nothing_and_writes_no_marker() {
        let t = tempfile::tempdir().unwrap();
        let state = t.path().join("notice");
        let mut env = env(t.path());
        env.insert("DO_NOT_TRACK".into(), "1".into());
        let api = registered(env, state.clone(), true, None);
        let (ctx, questions) = context_with_confirm(t.path(), "s", true);
        dispatch(&api, start(), &ctx).await;
        assert!(questions.lock().unwrap().is_empty());
        assert!(!state.exists());
    }

    #[tokio::test]
    async fn disabled_config_asks_nothing_and_writes_no_marker() {
        let t = tempfile::tempdir().unwrap();
        let state = t.path().join("notice");
        let api = registered(env(t.path()), state.clone(), false, None);
        let (ctx, questions) = context_with_confirm(t.path(), "s", true);
        dispatch(&api, start(), &ctx).await;
        assert!(questions.lock().unwrap().is_empty());
        assert!(!state.exists());
    }

    #[tokio::test]
    async fn an_automated_run_asks_nothing() {
        let t = tempfile::tempdir().unwrap();
        let state = t.path().join("notice");
        let mut env = env(t.path());
        env.insert("CI".into(), "1".into());
        let api = registered(env, state.clone(), true, None);
        let (ctx, questions) = context_with_confirm(t.path(), "s", true);
        dispatch(&api, start(), &ctx).await;
        assert!(questions.lock().unwrap().is_empty());
        assert!(!state.exists());
    }

    #[tokio::test]
    async fn unconfigured_key_asks_nothing() {
        let t = tempfile::tempdir().unwrap();
        let state = t.path().join("notice");
        let mut env = env(t.path());
        env.insert(
            "POSTHOG_API_KEY".into(),
            telemetry_core::UNCONFIGURED_POSTHOG_API_KEY.into(),
        );
        let api = registered(env, state.clone(), true, None);
        let (ctx, questions) = context_with_confirm(t.path(), "s", true);
        dispatch(&api, start(), &ctx).await;
        assert!(questions.lock().unwrap().is_empty());
        assert!(!state.exists());
    }

    #[tokio::test]
    async fn a_blocked_state_directory_reports_one_diagnostic() {
        let t = tempfile::tempdir().unwrap();
        let blocked = t.path().join("blocked");
        std::fs::write(&blocked, "").unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = Arc::clone(&seen);
        let api = registered(
            env(t.path()),
            blocked,
            true,
            Some(Arc::new(
                move |input: &telemetry_core::TelemetryDiagnosticInput| {
                    observed.lock().unwrap().push(input.clone())
                },
            )),
        );
        let (ctx, questions) = context_with_confirm(t.path(), "s", true);
        dispatch(&api, start(), &ctx).await;
        dispatch(&api, start(), &ctx).await;
        assert_eq!(questions.lock().unwrap().len(), 2);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].source, "omo-native-notice");
    }
}
