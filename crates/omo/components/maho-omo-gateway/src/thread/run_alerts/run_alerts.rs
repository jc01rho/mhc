use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omo_config_core::ResolvedErrorStreakSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::error_streak::{
    AgentEndFacts, ClockFn, ErrorStreakDetector, ErrorStreakDetectorOptions, ErrorStreakEvent,
    RunEnd, create_error_streak_detector, run_end_of,
};
use crate::thread::adapter::BoxFuture;
use crate::thread::tools::ThreadSendAs;

pub const ALERT_REQUEST_TIMEOUT_MS: u64 = 10_000;
pub const ALERT_WEBHOOK_RETRY_DELAY_MS: u64 = 2_000;
pub const ALERT_THREAD_RETRY_DELAY_MS: u64 = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookResponse {
    pub status: u16,
}

impl WebhookResponse {
    pub fn is_ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub type WebhookFetch =
    Arc<dyn Fn(String, String) -> BoxFuture<Result<WebhookResponse, String>> + Send + Sync>;
pub type DelayFn = Arc<dyn Fn(u64) -> BoxFuture<()> + Send + Sync>;
pub type DeviceIdProvider = Arc<dyn Fn() -> BoxFuture<Option<String>> + Send + Sync>;
pub type SettingsProvider = Arc<dyn Fn() -> ResolvedErrorStreakSettings + Send + Sync>;
pub type SessionNameProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;
pub type LogLineFn = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub struct RunAlertsLogger {
    pub info: LogLineFn,
    pub warn: LogLineFn,
}

#[derive(Clone, Default)]
pub struct OperatorAlertsTestSeams {
    pub env: Option<BTreeMap<String, String>>,
    pub now: Option<ClockFn>,
    pub device: Option<DeviceIdProvider>,
    pub fetch: Option<WebhookFetch>,
    pub delay: Option<DelayFn>,
    pub log: Option<RunAlertsLogger>,
}

pub struct RunAlertsOptions {
    pub settings: SettingsProvider,
    pub env: BTreeMap<String, String>,
    pub now: ClockFn,
    pub send_as: ThreadSendAs,
    pub session_name: SessionNameProvider,
    pub device: DeviceIdProvider,
    pub fetch: Option<WebhookFetch>,
    pub delay: Option<DelayFn>,
    pub log: RunAlertsLogger,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertPayload {
    #[serde(flatten)]
    pub event: ErrorStreakEvent,
    pub text: String,
    pub name: Option<String>,
    pub device: Option<String>,
}

pub fn load_user_error_streak_settings(
    env: &BTreeMap<String, String>,
) -> ResolvedErrorStreakSettings {
    let home = omo_config_core::resolve_home_dir(env);
    let loaded =
        maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions {
            cwd: Some(home),
            env: Some(env.clone()),
            file_system: None,
            harness: None,
            platform: None,
            profile: None,
        });
    omo_config_core::resolve_error_streak_settings(&loaded.config)
}

fn default_delay(ms: u64) -> BoxFuture<()> {
    Box::pin(async move {
        std::thread::sleep(Duration::from_millis(ms));
    })
}

fn default_webhook_post(url: String, body: String) -> BoxFuture<Result<WebhookResponse, String>> {
    Box::pin(async move {
        let timeout_secs = (ALERT_REQUEST_TIMEOUT_MS / 1_000).max(1).to_string();
        let mut child = Command::new("curl")
            .args([
                "-sS",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "-X",
                "POST",
                "-H",
                "content-type: application/json",
                "--max-time",
                &timeout_secs,
                "--data-binary",
                "@-",
                &url,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "spawn_error".to_owned())?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body.as_bytes());
        }
        let output = child
            .wait_with_output()
            .map_err(|_| "io_error".to_owned())?;
        if !output.status.success() {
            return Err("request_failed".to_owned());
        }
        let code_text = String::from_utf8_lossy(&output.stdout);
        let status: u16 = code_text
            .trim()
            .parse()
            .map_err(|_| "invalid_status".to_owned())?;
        Ok(WebhookResponse { status })
    })
}

struct DetectorState {
    settings: ResolvedErrorStreakSettings,
    detector: Arc<ErrorStreakDetector>,
}

pub struct RunAlerts {
    options: Arc<RunAlertsOptions>,
    last_end: Mutex<BTreeMap<String, RunEnd>>,
    state: Mutex<Option<DetectorState>>,
    missing_webhook_reported: Mutex<bool>,
}

pub fn create_run_alerts(options: RunAlertsOptions) -> Arc<RunAlerts> {
    Arc::new(RunAlerts {
        options: Arc::new(options),
        last_end: Mutex::new(BTreeMap::new()),
        state: Mutex::new(None),
        missing_webhook_reported: Mutex::new(false),
    })
}

impl RunAlerts {
    fn detector(&self) -> (ResolvedErrorStreakSettings, Arc<ErrorStreakDetector>) {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = guard.get_or_insert_with(|| {
            let settings = (self.options.settings)();
            let detector = Arc::new(create_error_streak_detector(ErrorStreakDetectorOptions {
                threshold: settings.threshold,
                realert_ms: settings.realert_ms as i64,
                now: self.options.now.clone(),
            }));
            DetectorState { settings, detector }
        });
        (state.settings.clone(), state.detector.clone())
    }

    pub fn agent_end(&self, durable_id: &str, event: &AgentEndFacts) {
        self.last_end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(durable_id.to_owned(), run_end_of(event));
    }

    pub fn settled(self: &Arc<Self>, durable_id: &str) {
        let end = self
            .last_end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(durable_id);
        let Some(end) = end else {
            return;
        };
        let (settings, detector) = self.detector();
        let Some(event) = detector.observe(durable_id, &end) else {
            return;
        };
        let this = Arc::clone(self);
        std::thread::spawn(move || {
            futures::executor::block_on(this.dispatch(event, settings));
        });
    }

    pub fn ended(&self, durable_id: &str) {
        self.last_end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(durable_id);
        if let Some(state) = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            state.detector.forget(durable_id);
        }
    }

    async fn to_thread(&self, address: &str, payload: &AlertPayload) -> bool {
        let facts = payload.event.facts();
        let event_name = payload.event.event_name();
        let message = format!("{}\n{}", payload.text, without_text(payload));
        let idempotency_key = format!(
            "operator-alert:{event_name}:{}:{}:{}:{address}",
            facts.session_id, facts.first_at, facts.count
        );
        let args = json!({
            "thread": address,
            "message": message,
            "delivery": "auto",
            "all_scope": true,
            "idempotency_key": idempotency_key,
        });
        let delay = self
            .options
            .delay
            .clone()
            .unwrap_or_else(|| Arc::new(default_delay));
        let mut last_code = "unknown".to_owned();
        for attempt in 1..=2 {
            let result = (self.options.send_as)(facts.session_id.clone(), args.clone()).await;
            if result.get("kind").and_then(Value::as_str) == Some("ok") {
                (self.options.log.info)(&format!(
                    "operator alert: {event_name} for {} sent to thread {address}",
                    facts.session_id
                ));
                return true;
            }
            if let Some(code) = result
                .get("error")
                .and_then(|err| err.get("code"))
                .and_then(Value::as_str)
            {
                last_code = code.to_owned();
            }
            if attempt == 1 {
                delay(ALERT_THREAD_RETRY_DELAY_MS).await;
            }
        }
        (self.options.log.warn)(&format!(
            "operator alert: {event_name} for {} not sent to thread {address}: {last_code}",
            facts.session_id
        ));
        false
    }

    async fn to_webhook(&self, url: &str, payload: &AlertPayload) -> bool {
        let facts = payload.event.facts();
        let event_name = payload.event.event_name();
        let post = self
            .options
            .fetch
            .clone()
            .unwrap_or_else(|| Arc::new(default_webhook_post));
        let delay = self
            .options
            .delay
            .clone()
            .unwrap_or_else(|| Arc::new(default_delay));
        let body = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_owned());
        let mut failure = "unknown".to_owned();
        for attempt in 1..=2 {
            match post(url.to_owned(), body.clone()).await {
                Ok(response) if response.is_ok() => {
                    (self.options.log.info)(&format!(
                        "operator alert: {event_name} for {} sent to the webhook",
                        facts.session_id
                    ));
                    return true;
                }
                Ok(response) => {
                    failure = format!("HTTP {}", response.status);
                }
                Err(code) => {
                    failure = if code.trim().is_empty() {
                        "error".to_owned()
                    } else {
                        code
                    };
                }
            }
            if attempt == 1 {
                delay(ALERT_WEBHOOK_RETRY_DELAY_MS).await;
            }
        }
        (self.options.log.warn)(&format!(
            "operator alert: {event_name} for {} not sent to the webhook: {failure}",
            facts.session_id
        ));
        false
    }

    async fn dispatch(
        self: Arc<Self>,
        event: ErrorStreakEvent,
        settings: ResolvedErrorStreakSettings,
    ) {
        let name = (self.options.session_name)()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let device = (self.options.device)().await;
        let text = alert_line(&event, name.as_deref(), device.as_deref());
        let session_id = event.facts().session_id.clone();
        let event_name = event.event_name();
        let payload = AlertPayload {
            event,
            text,
            name,
            device,
        };
        (self.options.log.warn)(&payload.text);
        let url = settings
            .webhook_url_env
            .as_ref()
            .and_then(|key| self.options.env.get(key))
            .map(|val| val.trim().to_owned())
            .filter(|val| !val.is_empty());
        if let Some(env_name) = &settings.webhook_url_env
            && url.is_none()
        {
            let mut reported = self
                .missing_webhook_reported
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !*reported {
                *reported = true;
                (self.options.log.warn)(&format!(
                    "operator alert: operator_alerts.error_streak.webhook_url_env names {env_name}, which is not set; no webhook is called"
                ));
            }
        }
        let mut futures: Vec<BoxFuture<bool>> = settings
            .threads
            .iter()
            .map(|address| {
                let address = address.clone();
                let payload = payload.clone();
                let this = Arc::clone(&self);
                Box::pin(async move { this.to_thread(&address, &payload).await }) as BoxFuture<bool>
            })
            .collect();
        if let Some(url) = url {
            let payload = payload.clone();
            let this = Arc::clone(&self);
            futures.push(Box::pin(
                async move { this.to_webhook(&url, &payload).await },
            ));
        }
        let reached = futures::future::join_all(futures).await;
        let ok_count = reached.iter().filter(|ok| **ok).count();
        (self.options.log.info)(&format!(
            "operator alert: {event_name} for {session_id}: {ok_count} of {} target(s) reached",
            reached.len()
        ));
    }
}

fn without_text(payload: &AlertPayload) -> Value {
    let mut val = serde_json::to_value(payload).unwrap_or_else(|_| json!({}));
    if let Some(map) = val.as_object_mut() {
        map.remove("text");
    }
    val
}

pub fn alert_line(event: &ErrorStreakEvent, name: Option<&str>, device: Option<&str>) -> String {
    let facts = event.facts();
    let name_prefix = match name {
        Some(n) => format!("\"{n}\" "),
        None => String::new(),
    };
    let device_suffix = match device {
        Some(d) => format!("device {d}"),
        None => "this device".to_owned(),
    };
    let seat = format!(
        "session {name_prefix}({}) on {device_suffix}",
        facts.session_id
    );
    let classes = facts
        .classes
        .iter()
        .map(|(class, count)| format!("{} {count}", class.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let span = format!(
        "first {}, last {}",
        format_iso_ms(facts.first_at),
        format_iso_ms(facts.last_at)
    );
    match event {
        ErrorStreakEvent::ErrorStreakRecovered { recovered_at, .. } => format!(
            "omo alert: {seat} recovered at {} after {} runs in a row ended with an error ({classes}); {span}",
            format_iso_ms(*recovered_at),
            facts.count
        ),
        ErrorStreakEvent::ErrorStreak {
            provider,
            model,
            repeat,
            ..
        } => {
            let model_part = match model {
                Some(m) => {
                    let prov = match provider {
                        Some(p) => format!("{p}/"),
                        None => String::new(),
                    };
                    format!("; model {prov}{m}")
                }
                None => String::new(),
            };
            if *repeat {
                format!(
                    "omo alert: {seat} is still failing: {} runs in a row ended with an error ({classes}); {span}{model_part}",
                    facts.count
                )
            } else {
                format!(
                    "omo alert: {seat} ended {} runs in a row with an error ({classes}); {span}{model_part}",
                    facts.count
                )
            }
        }
    }
}

pub fn format_iso_ms(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let day_ms = ms.rem_euclid(86_400_000);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    let hour = day_ms / 3_600_000;
    let minute = (day_ms % 3_600_000) / 60_000;
    let second = (day_ms % 60_000) / 1_000;
    let millis = day_ms % 1_000;
    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}
