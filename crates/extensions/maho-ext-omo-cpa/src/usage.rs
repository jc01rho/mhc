//! Port of omo-cpa `src/usage.ts`. CPA account usage via the management API.
//!
//! The management API is protected by a key separate from the inference key omo uses. Without it
//! every endpoint answers 401, so this module reports "unsupported" with the reason instead of
//! guessing at numbers.

use crate::types::{UsageAccount, UsageAccountStatus, UsageModelQuota, UsageResult, UsageWindow};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::time::Duration;

pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;

pub async fn fetch_usage(
    client: &reqwest::Client,
    root: &str,
    management_key: Option<&str>,
    timeout_ms: u64,
) -> UsageResult {
    let Some(management_key) = management_key else {
        return UsageResult::Unsupported {
            reason:
                "Management 키 없음 · OMO_CPA_MANAGEMENT_KEY를 설정하면 계정별 사용량이 켜집니다"
                    .to_owned(),
        };
    };

    let response = client
        .get(format!("{root}/v0/management/auth-files"))
        .header("Authorization", format!("Bearer {management_key}"))
        .timeout(Duration::from_millis(timeout_ms))
        .send()
        .await;

    let response = match response {
        Ok(response) => response,
        Err(error) => {
            let reason = if error.is_timeout() {
                format!("관리 API 시간 초과 ({timeout_ms}ms)")
            } else {
                format!("관리 API 오류: {error}")
            };
            return UsageResult::Unsupported { reason };
        }
    };

    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return UsageResult::Unsupported {
            reason: format!("Management 키가 거부됨 (HTTP {status})"),
        };
    }
    if !response.status().is_success() {
        return UsageResult::Unsupported {
            reason: format!("관리 API 오류 (HTTP {status})"),
        };
    }

    let body: Value = match response.json().await {
        Ok(body) => body,
        Err(_) => {
            return UsageResult::Unsupported {
                reason: "관리 API 응답을 해석할 수 없음".to_owned(),
            };
        }
    };

    match parse_auth_files(&body) {
        Some(accounts) => UsageResult::Supported { accounts },
        None => UsageResult::Unsupported {
            reason:
                "관리 API 응답 형식을 알 수 없어 사용량을 표시하지 않음 (files/auth_files/data 배열 없음)"
                    .to_owned(),
        },
    }
}

/// Parse the auth-files payload into account rows. `None` when the shape is unrecognised, so the
/// caller can say so rather than render an empty-but-confident table.
pub fn parse_auth_files(body: &Value) -> Option<Vec<UsageAccount>> {
    let files = auth_file_list(body)?;

    let mut out = Vec::new();
    for item in files {
        let Some(record) = item.as_object() else {
            continue;
        };
        let observed_at = observation_time(record.get("quota"));
        let signals = signal_map(record.get("quota"));
        // The server reports `active`, `disabled`, or `error`. `status_message` is empty for
        // healthy credentials, so any non-empty one is the error detail.
        let status = str_at(record, "status")
            .map(|value| value.to_lowercase())
            .unwrap_or_default();
        let disabled = record.get("disabled") == Some(&Value::Bool(true))
            || record.get("unavailable") == Some(&Value::Bool(true))
            || status == "disabled";
        let detail = if disabled {
            None
        } else {
            str_at(record, "status_message")
        };
        let windows = derive_windows(&signals, observed_at);
        let models = parse_model_quotas(record.get("model_quotas"));
        let status = if disabled {
            UsageAccountStatus::Off
        } else if status == "error" || detail.is_some() {
            UsageAccountStatus::Error
        } else {
            UsageAccountStatus::Ok
        };
        let mut times: Vec<Option<i64>> = Vec::new();
        if !windows.is_empty() {
            times.push(observed_at);
        }
        for model in &models {
            if !model.windows.is_empty() {
                times.push(model.observed_at);
            }
        }
        out.push(UsageAccount {
            provider: str_at(record, "provider")
                .or_else(|| str_at(record, "type"))
                .unwrap_or_else(|| "unknown".to_owned()),
            label: str_at(record, "label")
                .or_else(|| str_at(record, "email"))
                .or_else(|| str_at(record, "account"))
                .or_else(|| str_at(record, "name"))
                .unwrap_or_else(|| "(이름 없음)".to_owned()),
            status,
            detail,
            meta: account_meta(&signals),
            windows,
            models,
            observed_at: stalest_of(&times),
        });
    }
    Some(out)
}

/// Snapshot instant of one `{observed_at, signals}` container.
fn observation_time(container: Option<&Value>) -> Option<i64> {
    let raw = container?
        .as_object()?
        .get("observed_at")
        .and_then(Value::as_str)?;
    parse_instant(raw)
}

/// Accepts ISO 8601 and bare epoch milliseconds, matching the `Date.parse` the TS relied on for
/// the shapes this API actually emits.
fn parse_instant(raw: &str) -> Option<i64> {
    if let Ok(ms) = raw.trim().parse::<f64>()
        && ms.is_finite()
    {
        return Some(ms as i64);
    }
    chrono::DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|instant| instant.timestamp_millis())
}

fn stalest_of(times: &[Option<i64>]) -> Option<i64> {
    times.iter().flatten().copied().min()
}

/// `{files:[…]}` is the documented envelope; the bare-array and `data` shapes are kept for older
/// builds.
fn auth_file_list(body: &Value) -> Option<Vec<Value>> {
    if let Some(array) = body.as_array() {
        return Some(array.clone());
    }
    let record = body.as_object()?;
    for key in ["files", "auth_files", "data"] {
        if let Some(array) = record.get(key).and_then(Value::as_array) {
            return Some(array.clone());
        }
    }
    None
}

/// Reads the `{signals:{…}}` envelope, keeping only string values.
fn signal_map(container: Option<&Value>) -> BTreeMap<String, String> {
    let Some(signals) = container
        .and_then(Value::as_object)
        .and_then(|record| record.get("signals"))
        .and_then(Value::as_object)
    else {
        return BTreeMap::new();
    };
    signals
        .iter()
        .filter_map(|(name, value)| value.as_str().map(|text| (name.clone(), text.to_owned())))
        .collect()
}

fn parse_model_quotas(raw: Option<&Value>) -> Vec<UsageModelQuota> {
    let Some(quotas) = raw.and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, value) in quotas {
        let observed_at = observation_time(Some(value));
        let windows = derive_windows(&signal_map(Some(value)), observed_at);
        if !windows.is_empty() {
            out.push(UsageModelQuota {
                id: id.clone(),
                windows,
                observed_at,
            });
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowField {
    UsedPercent,
    Utilization,
    WindowMinutes,
    ResetAt,
    ResetAfterSeconds,
    Reset,
    LimitName,
    DisabledReason,
    LimitReached,
    Allowed,
    Status,
}

/// Longest suffix first, so `-reset-at` is not read as a window named `…-reset`.
const WINDOW_FIELDS: &[(&str, WindowField)] = &[
    ("used-percent", WindowField::UsedPercent),
    ("reset-after-seconds", WindowField::ResetAfterSeconds),
    ("window-minutes", WindowField::WindowMinutes),
    ("disabled-reason", WindowField::DisabledReason),
    ("limit-reached", WindowField::LimitReached),
    ("utilization", WindowField::Utilization),
    ("limit-name", WindowField::LimitName),
    ("reset-at", WindowField::ResetAt),
    ("allowed", WindowField::Allowed),
    ("status", WindowField::Status),
    ("reset", WindowField::Reset),
];

/// Fields that mean "this scope cannot serve", independent of its utilization.
fn is_rejection_field(field: WindowField) -> bool {
    matches!(
        field,
        WindowField::Status
            | WindowField::LimitReached
            | WindowField::Allowed
            | WindowField::DisabledReason
    )
}

/// Provider namespaces. The bare root key names no window — it speaks for the whole credential — so
/// it strips to "".
const NAMESPACE_ROOTS: &[&str] = &["x-codex", "anthropic-ratelimit-unified"];

/// Anthropic reports windows by name instead of by length.
fn named_window(name: &str) -> Option<&'static str> {
    Some(match name {
        "" => "통합",
        "overage" => "overage",
        "primary" => "주",
        "secondary" => "보조",
        "3h" => "3시간",
        "5h" => "5시간",
        "7d" => "7일",
        "7d_oi" => "7일(추가)",
        "code-review" => "코드 리뷰",
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, Default)]
struct Group {
    remaining: Option<f64>,
    minutes: Option<f64>,
    absolute: Option<i64>,
    relative: Option<f64>,
}

/// Turn one header snapshot into displayable windows.
///
/// A window is rendered only when the snapshot carries a percentage for it — a bare reset watermark
/// with no utilization would otherwise read like a real allowance.
pub fn derive_windows(
    signals: &BTreeMap<String, String>,
    observed_at: Option<i64>,
) -> Vec<UsageWindow> {
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    // Flags are recorded per scope; the nearest scope wins, so a credential-wide "rejected" cannot
    // override a window that explicitly reports "allowed".
    let mut refused: Vec<String> = Vec::new();
    let mut allowed: Vec<String> = Vec::new();
    let mut limit_names: BTreeMap<String, String> = BTreeMap::new();

    for (name, value) in signals {
        let Some((key, field)) = split_signal(name) else {
            continue;
        };
        if field == WindowField::LimitName {
            limit_names.insert(key, value.clone());
            continue;
        }
        if is_rejection_field(field) {
            let target = if is_window_rejected(field, value) {
                &mut refused
            } else {
                &mut allowed
            };
            if !target.contains(&key) {
                target.push(key);
            }
            continue;
        }
        let group = groups.entry(key.clone()).or_default();
        match field {
            WindowField::UsedPercent => {
                if let Some(used) = num(value) {
                    group.remaining = Some(clamp(100.0 - used));
                }
            }
            WindowField::Utilization => {
                // Anthropic's utilization is a 0..1 fraction of the allowance.
                if let Some(fraction) = num(value).filter(|fraction| *fraction >= 0.0) {
                    group.remaining = Some(clamp((1.0 - fraction) * 100.0));
                }
            }
            WindowField::WindowMinutes => group.minutes = num(value),
            WindowField::ResetAfterSeconds => {
                if let Some(seconds) = num(value).filter(|seconds| *seconds > 0.0) {
                    group.relative = Some(seconds);
                }
            }
            _ => group.absolute = epoch_seconds_to_ms(value),
        }
    }

    let mut out = Vec::new();
    // A refusal can arrive with no utilization at all, so a flagged scope that owns its own windows
    // still needs a row: the refusal is the fact.
    let mut flagged: Vec<String> = refused.clone();
    for scope in &allowed {
        if !flagged.contains(scope) {
            flagged.push(scope.clone());
        }
    }
    for scope in flagged {
        if !groups.contains_key(&scope) && is_leaf_scope(&scope, &groups) {
            groups.insert(scope, Group::default());
        }
    }

    for (key, group) in &groups {
        let is_refused = resolve_refused(key, &refused, &allowed);
        // A zero-length window is an unused slot (Codex reports one when no secondary limit
        // applies), not an allowance or a spent limit.
        if group.minutes.is_some_and(|minutes| minutes <= 0.0) {
            continue;
        }
        if group.remaining.is_none() && !is_refused {
            continue;
        }
        // An absolute instant is authoritative; a relative one counts from the snapshot, not from
        // whenever this report happens to be rendered.
        let resets_at = group.absolute.or_else(|| {
            group
                .relative
                .map(|seconds| (observed_at.unwrap_or_else(now_ms)) + (seconds * 1000.0) as i64)
        });
        let limit = match_limit_name(key, &limit_names);
        let limit_matched = limit.is_some();
        out.push(UsageWindow {
            label: window_label(key, group.minutes, limit.as_ref()),
            // Never report an allowance the upstream has already refused.
            remaining_percent: Some(if is_refused {
                0.0
            } else {
                round2(group.remaining.unwrap_or(0.0))
            }),
            resets_at,
            note: Some(if is_refused {
                "한도 도달".to_owned()
            } else if limit_matched {
                String::new()
            } else {
                limit_names.get(key).cloned().unwrap_or_default()
            })
            .filter(|note| !note.is_empty()),
        });
    }
    out
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// A scope that should render as its own row: inside a provider namespace and governing no deeper
/// scope.
fn is_leaf_scope(scope: &str, groups: &BTreeMap<String, Group>) -> bool {
    !groups
        .keys()
        .any(|key| key.starts_with(&format!("{scope}-")))
}

/// The most specific flag naming this window decides it: a flag on the window itself, then a flag
/// on the exact scope that owns it.
fn resolve_refused(key: &str, refused: &[String], allowed: &[String]) -> bool {
    if refused.iter().any(|scope| scope == key) {
        return true;
    }
    if allowed.iter().any(|scope| scope == key) {
        return false;
    }
    let Some(parent) = parent_scope(key) else {
        return false;
    };
    refused.contains(&parent)
}

/// A flag covers exactly the windows whose own parent scope is that flag's scope. Prefix matching
/// is too coarse: Codex generates `X-Codex-Limit-Reached` from the base rate-limit object only.
fn parent_scope(key: &str) -> Option<String> {
    for slot in ["primary", "secondary"] {
        if let Some(stripped) = key.strip_suffix(&format!("-{slot}")) {
            return Some(stripped.to_owned());
        }
    }
    for root in NAMESPACE_ROOTS {
        if let Some(stripped) = key.strip_prefix(&format!("{root}-")) {
            let _ = stripped;
            return Some((*root).to_owned());
        }
    }
    None
}

/// Whether a rejection-shaped field actually says "refused".
fn is_window_rejected(field: WindowField, value: &str) -> bool {
    let text = value.trim().to_lowercase();
    if text.is_empty() {
        return false;
    }
    match field {
        WindowField::Status => !text.starts_with("allowed"),
        WindowField::Allowed => matches!(text.as_str(), "false" | "0" | "no"),
        WindowField::LimitReached => matches!(text.as_str(), "true" | "1" | "yes"),
        _ => true,
    }
}

fn strip_namespace(key: &str) -> String {
    for root in NAMESPACE_ROOTS {
        if key == *root {
            return String::new();
        }
        if let Some(stripped) = key.strip_prefix(&format!("{root}-")) {
            return stripped.to_owned();
        }
    }
    key.to_owned()
}

fn split_signal(name: &str) -> Option<(String, WindowField)> {
    let lower = name.to_lowercase();
    for (suffix, field) in WINDOW_FIELDS {
        if let Some(stripped) = lower.strip_suffix(&format!("-{suffix}")) {
            return Some((stripped.to_owned(), *field));
        }
    }
    None
}

/// Codex namespaces an extra limit by a short id and carries its human name in a sibling
/// `…-limit-name` signal.
fn match_limit_name(key: &str, limit_names: &BTreeMap<String, String>) -> Option<(String, String)> {
    let mut best: Option<(String, String)> = None;
    for (limit_key, name) in limit_names {
        let Some(rest) = key.strip_prefix(&format!("{limit_key}-")) else {
            continue;
        };
        let rest = rest.to_owned();
        if best
            .as_ref()
            .is_none_or(|(_, best_rest)| rest.len() < best_rest.len())
        {
            best = Some((name.clone(), rest));
        }
    }
    best
}

/// Build the display label for one window.
///
/// A window length alone is not an identity: the code-review limit and the base limit can both run
/// for 7 days, so the scope has to stay in the label or two distinct limits collapse into one row.
fn window_label(key: &str, minutes: Option<f64>, limit: Option<&(String, String)>) -> String {
    let scope = strip_namespace(match limit {
        Some((_, rest)) => rest,
        None => key,
    });
    let name = limit.map_or_else(String::new, |(name, _)| format!("{name} "));
    let Some(minutes) = minutes else {
        // A namespace root strips to "" and names the whole credential.
        return format!(
            "{name}{}",
            named_window(&scope).map_or_else(|| scope.replace('-', " "), str::to_owned)
        );
    };
    let group = scope_group(&scope);
    let group_label = if group.is_empty() {
        String::new()
    } else {
        format!(
            "{} ",
            named_window(&group).map_or_else(|| group.replace('-', " "), str::to_owned)
        )
    };
    format!("{name}{group_label}{}", minutes_label(minutes))
}

/// The namespaced group of a slot scope: `code-review-primary` -> `code-review`.
fn scope_group(scope: &str) -> String {
    for slot in ["primary", "secondary"] {
        if scope == slot {
            return String::new();
        }
        if let Some(stripped) = scope.strip_suffix(&format!("-{slot}")) {
            return stripped.to_owned();
        }
    }
    scope.to_owned()
}

fn minutes_label(minutes: f64) -> String {
    let minutes = minutes as i64;
    if minutes % 1440 == 0 {
        return format!("{}일", minutes / 1440);
    }
    if minutes % 60 == 0 {
        return format!("{}시간", minutes / 60);
    }
    format!("{minutes}분")
}

/// Compact scalar summary: plan, active limit, credits, unified rate-limit status.
fn account_meta(signals: &BTreeMap<String, String>) -> Option<String> {
    let read = |name: &str| -> Option<String> {
        signals
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    };
    let mut parts = Vec::new();
    if let Some(plan) = read("X-Codex-Plan-Type") {
        parts.push(format!("요금제 {plan}"));
    }
    if let Some(limit) = read("X-Codex-Active-Limit") {
        parts.push(format!("한도 {limit}"));
    }
    if read("X-Codex-Credits-Has-Credits").is_some_and(|value| value.to_lowercase() == "true") {
        parts.push(format!(
            "크레딧 {}",
            read("X-Codex-Credits-Balance").unwrap_or_else(|| "?".to_owned())
        ));
    }
    if let Some(unified) = read("Anthropic-Ratelimit-Unified-Status") {
        parts.push(format!("통합 {unified}"));
    }
    if let Some(overage) = read("Anthropic-Ratelimit-Unified-Overage-Status") {
        parts.push(format!("overage {overage}"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn epoch_seconds_to_ms(value: &str) -> Option<i64> {
    // Watermarks are epoch seconds; a non-positive value means "no window".
    num(value)
        .filter(|seconds| *seconds > 0.0)
        .map(|seconds| (seconds * 1000.0) as i64)
}

fn clamp(percent: f64) -> f64 {
    percent.clamp(0.0, 100.0)
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn str_at(record: &Map<String, Value>, key: &str) -> Option<String> {
    record
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn num(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn signals(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn codex_primary_renders_remaining_percent_and_window_length() {
        let windows = derive_windows(
            &signals(&[
                ("X-Codex-Primary-Used-Percent", "39"),
                ("X-Codex-Primary-Window-Minutes", "10080"),
                ("X-Codex-Primary-Reset-At", "1800000000"),
            ]),
            Some(0),
        );
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "7일");
        assert_eq!(windows[0].remaining_percent, Some(61.0));
        assert_eq!(windows[0].resets_at, Some(1_800_000_000_000));
    }

    #[test]
    fn anthropic_utilization_is_a_fraction_of_the_allowance() {
        let windows = derive_windows(
            &signals(&[
                ("Anthropic-Ratelimit-Unified-5h-Utilization", "0.43"),
                ("Anthropic-Ratelimit-Unified-5h-Reset-After-Seconds", "60"),
            ]),
            Some(1_000),
        );
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "5시간");
        assert_eq!(windows[0].remaining_percent, Some(57.0));
        // 0.43 * 100 = 42.999...; round2 keeps the rendered number honest.
        assert!(windows[0].remaining_percent.unwrap() > 56.9);
        assert_eq!(windows[0].resets_at, Some(61_000));
    }

    #[test]
    fn a_window_without_a_percentage_is_not_rendered() {
        let windows = derive_windows(
            &signals(&[("X-Codex-Primary-Reset-At", "1800000000")]),
            Some(0),
        );
        assert!(windows.is_empty());
    }

    #[test]
    fn a_zero_length_window_is_an_unused_slot() {
        let windows = derive_windows(
            &signals(&[
                ("X-Codex-Secondary-Used-Percent", "10"),
                ("X-Codex-Secondary-Window-Minutes", "0"),
            ]),
            Some(0),
        );
        assert!(windows.is_empty());
    }

    #[test]
    fn a_refusal_zeroes_an_allowance_and_notes_the_limit() {
        let windows = derive_windows(
            &signals(&[
                ("X-Codex-Used-Percent", "43"),
                ("X-Codex-Limit-Reached", "true"),
            ]),
            Some(0),
        );
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].remaining_percent, Some(0.0));
        assert_eq!(windows[0].note.as_deref(), Some("한도 도달"));
    }

    #[test]
    fn a_base_refusal_does_not_zero_an_independent_code_review_window() {
        let windows = derive_windows(
            &signals(&[
                ("X-Codex-Limit-Reached", "true"),
                ("X-Codex-Code-Review-Primary-Used-Percent", "4"),
                ("X-Codex-Code-Review-Primary-Window-Minutes", "10080"),
                ("X-Codex-Code-Review-Primary-Limit-Name", "Code Review"),
            ]),
            Some(0),
        );
        let review = windows
            .iter()
            .find(|window| window.label.contains("코드 리뷰"))
            .expect("code review window");
        assert_eq!(review.remaining_percent, Some(96.0));
        assert_eq!(review.label, "코드 리뷰 7일");
        assert_eq!(review.note.as_deref(), Some("Code Review"));
    }

    #[test]
    fn a_window_level_allowance_overrides_a_credential_wide_refusal() {
        let windows = derive_windows(
            &signals(&[
                ("Anthropic-Ratelimit-Unified-Status", "rejected"),
                ("Anthropic-Ratelimit-Unified-Overage-Allowed", "true"),
                ("Anthropic-Ratelimit-Unified-Overage-Utilization", "0.5"),
            ]),
            Some(0),
        );
        let overage = windows
            .iter()
            .find(|window| window.label == "overage")
            .expect("overage window");
        assert_eq!(overage.remaining_percent, Some(50.0));
    }

    #[test]
    fn the_account_parser_reads_the_documented_envelope() {
        let accounts = parse_auth_files(&json!({
            "files": [{
                "provider": "codex",
                "label": "primary",
                "status": "active",
                "quota": {
                    "observed_at": "2026-11-10T11:00:00Z",
                    "signals": {
                        "X-Codex-Plan-Type": "plus",
                        "X-Codex-Credits-Has-Credits": "true",
                        "X-Codex-Credits-Balance": "12",
                        "X-Codex-Primary-Used-Percent": "39",
                        "X-Codex-Primary-Window-Minutes": "10080"
                    }
                }
            }]
        }))
        .expect("accounts");
        assert_eq!(accounts.len(), 1);
        let account = &accounts[0];
        assert_eq!(account.provider, "codex");
        assert_eq!(account.label, "primary");
        assert_eq!(account.status, UsageAccountStatus::Ok);
        assert_eq!(account.meta.as_deref(), Some("요금제 plus · 크레딧 12"));
        assert_eq!(account.windows.len(), 1);
        assert_eq!(account.observed_at, Some(1_794_308_400_000));
    }

    #[test]
    fn a_bare_array_and_an_unknown_shape_both_behave() {
        assert!(parse_auth_files(&json!([{"provider": "p"}])).is_some());
        assert!(parse_auth_files(&json!({"nope": []})).is_none());
        assert!(parse_auth_files(&json!("text")).is_none());
    }

    #[test]
    fn disabled_and_error_credentials_are_labelled() {
        let accounts = parse_auth_files(&json!({"files": [
            {"provider": "a", "disabled": true, "quota": {}},
            {"provider": "b", "status": "error", "status_message": "revoked", "quota": {}},
            {"provider": "c", "status": "active", "status_message": "unexpected", "quota": {}}
        ]}))
        .expect("accounts");
        assert_eq!(accounts[0].status, UsageAccountStatus::Off);
        assert_eq!(accounts[0].detail, None);
        assert_eq!(accounts[1].status, UsageAccountStatus::Error);
        assert_eq!(accounts[1].detail.as_deref(), Some("revoked"));
        assert_eq!(accounts[2].status, UsageAccountStatus::Error);
    }

    #[test]
    fn model_quotas_keep_only_models_with_windows() {
        let accounts = parse_auth_files(&json!({"files": [{
            "provider": "p",
            "quota": {"signals": {"X-Codex-Used-Percent": "10"}},
            "model_quotas": {
                "claude-opus-5": {"observed_at": "2026-11-10T10:00:00Z", "signals": {"X-Codex-Used-Percent": "50"}},
                "empty": {"signals": {}}
            }
        }]}))
        .expect("accounts");
        assert_eq!(accounts[0].models.len(), 1);
        assert_eq!(accounts[0].models[0].id, "claude-opus-5");
        // The stalest watermark behind the rows decides the account freshness.
        assert_eq!(accounts[0].observed_at, Some(1_794_304_800_000));
    }

    #[test]
    fn label_fallbacks_follow_the_ts_order() {
        let accounts = parse_auth_files(&json!({"auth_files": [
            {"email": "e@x"}, {"account": "acct"}, {"name": "nm"}, {}
        ]}))
        .expect("accounts");
        assert_eq!(accounts[0].label, "e@x");
        assert_eq!(accounts[1].label, "acct");
        assert_eq!(accounts[2].label, "nm");
        assert_eq!(accounts[3].label, "(이름 없음)");
        assert_eq!(accounts[3].provider, "unknown");
    }
}
