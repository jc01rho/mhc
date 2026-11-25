pub mod error_streak;
#[allow(clippy::module_inception)]
pub mod run_alerts;

pub use error_streak::{
    AgentEndFacts, ClockFn, ErrorStreakDetector, ErrorStreakDetectorOptions, ErrorStreakEvent,
    RunEnd, RunErrorClass, StreakFacts, agent_end_facts_from_messages, classify_run_error,
    create_error_streak_detector, run_end_of,
};
pub use run_alerts::{
    ALERT_REQUEST_TIMEOUT_MS, ALERT_THREAD_RETRY_DELAY_MS, ALERT_WEBHOOK_RETRY_DELAY_MS,
    AlertPayload, DelayFn, DeviceIdProvider, LogLineFn, OperatorAlertsTestSeams, RunAlerts,
    RunAlertsLogger, RunAlertsOptions, SessionNameProvider, SettingsProvider, WebhookFetch,
    WebhookResponse, alert_line, create_run_alerts, format_iso_ms, load_user_error_streak_settings,
};
