//! Port of omo-cpa `src/render.ts`. Status line and the full `/cpa` report.
//!
//! Korean copy is pinned literally: the owner-facing text is the contract.

use crate::types::{CpaConfig, HealthSnapshot, HealthState, UsageAccount, UsageAccountStatus, UsageResult, UsageWindow};

/// East-Asian-aware display width, so Korean text never breaks column alignment.
pub fn display_width(text: &str) -> usize {
    text.chars()
        .map(|character| {
            let code = character as u32;
            let wide = (0x1100..=0x115f).contains(&code)
                || (0x2e80..=0xa4cf).contains(&code)
                || (0xac00..=0xd7a3).contains(&code)
                || (0xf900..=0xfaff).contains(&code)
                || (0xfe30..=0xfe6f).contains(&code)
                || (0xff00..=0xff60).contains(&code)
                || (0xffe0..=0xffe6).contains(&code)
                || (0x20000..=0x3fffd).contains(&code);
            if wide { 2 } else { 1 }
        })
        .sum()
}

pub fn pad(text: &str, width: usize) -> String {
    let visible = display_width(text);
    if visible < width {
        format!("{text}{}", " ".repeat(width - visible))
    } else {
        text.to_owned()
    }
}

pub fn state_label(state: HealthState) -> &'static str {
    match state {
        HealthState::Ok => "정상",
        HealthState::Degraded => "불안정",
        HealthState::Down => "다운",
        HealthState::RateLimited => "요청 제한",
        HealthState::Unknown => "미확인",
    }
}

pub fn account_state_label(status: UsageAccountStatus) -> &'static str {
    match status {
        UsageAccountStatus::Ok => "정상",
        UsageAccountStatus::Error => "오류",
        UsageAccountStatus::Off => "비활성",
    }
}

/// Keeps a lone short window from hugging the percentage.
pub const MIN_WINDOW_WIDTH: usize = 12;

/// Live tier sizes, derived from the catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TierCounts {
    pub primary: usize,
    pub last: usize,
    pub chat_unfit: usize,
}

/// One-line summary suitable for the TUI footer.
pub fn render_status_line(tiers: Option<TierCounts>, health: &HealthSnapshot) -> String {
    let mut parts = vec![format!("CPA {}", state_label(health.state))];
    if let Some(tiers) = tiers {
        parts.push(format!("주력 {}종 · 최후 {}종", tiers.primary, tiers.last));
    }
    parts.join(" · ")
}

pub struct ReportInput<'a> {
    pub config: &'a CpaConfig,
    pub tiers: Option<TierCounts>,
    pub tier_reason: Option<&'a str>,
    pub health: &'a HealthSnapshot,
    pub usage: &'a UsageResult,
}

/// Full multi-line report for the `/cpa` command.
pub fn render_report(input: &ReportInput<'_>) -> String {
    render_report_at(input, now_ms())
}

/// Same report against an explicit instant, so the age lines stay testable.
pub fn render_report_at(input: &ReportInput<'_>, now: i64) -> String {
    let mut lines: Vec<String> = Vec::new();
    let rule = "─".repeat(72);

    lines.push("CPA 상태".to_owned());
    lines.push(rule.clone());
    lines.push(format!("서버        {}", input.config.root));
    lines.push(format!(
        "상태        {} · {}",
        state_label(input.health.state),
        input.health.detail
    ));
    lines.push(String::new());

    lines.push("모델 티어".to_owned());
    lines.push(rule.clone());
    match input.tiers {
        None => lines.push(format!(
            "  확인 불가 · {}",
            input.tier_reason.unwrap_or("이유 불명")
        )),
        Some(tiers) => {
            lines.push(format!(
                "  주력 {}종 · 최후 {}종 · 대화 불가 {}종",
                tiers.primary, tiers.last, tiers.chat_unfit
            ));
            lines.push(
                "  티어는 실시간 카탈로그에서 산출합니다. omo 설정 파일은 읽지도 쓰지도 않습니다."
                    .to_owned(),
            );
        }
    }
    lines.push(String::new());

    lines.push("계정 사용량".to_owned());
    lines.push(rule);
    match input.usage {
        UsageResult::Unsupported { reason } => lines.push(format!("  n/a · {reason}")),
        UsageResult::Supported { accounts } if accounts.is_empty() => {
            lines.push("  계정 없음".to_owned());
        }
        UsageResult::Supported { accounts } => {
            lines.push(format!("  {}", usage_summary(accounts, now)));
            for account in order_accounts(accounts) {
                let head = format!(
                    "  {} {} {}",
                    pad(&account.label, 30),
                    pad(&account.provider, 15),
                    account_state_label(account.status)
                );
                let tail: Vec<String> = [
                    account
                        .detail
                        .clone()
                        .or_else(|| (account.status == UsageAccountStatus::Off).then(|| "서버에서 사용 중지".to_owned())),
                    account.meta.clone(),
                ]
                .into_iter()
                .flatten()
                .collect();
                lines.push(if tail.is_empty() {
                    head
                } else {
                    format!("{head} · {}", tail.join(" · "))
                });

                let mut windows = account.windows.clone();
                windows.extend(leftover_model_windows(account));
                let width = windows
                    .iter()
                    .map(|window| display_width(&window.label))
                    .max()
                    .unwrap_or(MIN_WINDOW_WIDTH)
                    .max(MIN_WINDOW_WIDTH);
                for window in &windows {
                    lines.push(format!(
                        "      {} {}",
                        pad(&window.label, width),
                        render_window(window, now)
                    ));
                }
            }
        }
    }

    lines.join("\n")
}

fn usage_summary(accounts: &[UsageAccount], now: i64) -> String {
    let active = accounts
        .iter()
        .filter(|account| account.status != UsageAccountStatus::Off)
        .count();
    let with_usage: Vec<&UsageAccount> = accounts
        .iter()
        .filter(|account| !account.windows.is_empty() || !account.models.is_empty())
        .collect();
    let mut parts = vec![
        format!("활성 {active}"),
        format!("비활성 {}", accounts.len() - active),
        format!("사용량 관측 {}", with_usage.len()),
    ];
    // Every watermark behind the rows still shown, so the age cannot look fresher than the oldest
    // number beneath it.
    let oldest = with_usage
        .iter()
        .filter_map(|account| account.observed_at)
        .min();
    if let Some(oldest) = oldest {
        parts.push(format!("스냅샷 {}", format_ago(oldest, now, "전")));
    }
    parts.join(" · ")
}

/// Accounts carrying live watermarks first, then healthy, then disabled.
fn order_accounts(accounts: &[UsageAccount]) -> Vec<&UsageAccount> {
    let rank = |account: &&UsageAccount| -> u8 {
        if !account.windows.is_empty() || !account.models.is_empty() {
            0
        } else if account.status == UsageAccountStatus::Off {
            2
        } else {
            1
        }
    };
    let mut ordered: Vec<&UsageAccount> = accounts.iter().collect();
    ordered.sort_by(|left, right| {
        rank(left)
            .cmp(&rank(right))
            .then_with(|| left.label.cmp(&right.label))
    });
    ordered
}

/// Model-scoped rows, minus anything the account row already shows.
fn leftover_model_windows(account: &UsageAccount) -> Vec<UsageWindow> {
    let shown: Vec<String> = account.windows.iter().map(window_key).collect();
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for model in &account.models {
        for window in &model.windows {
            let key = window_key(window);
            if shown.contains(&key) || seen.contains(&key) {
                continue;
            }
            seen.push(key);
            out.push(UsageWindow {
                label: format!("{} {}", model.id, window.label),
                ..window.clone()
            });
        }
    }
    out
}

/// Identity of a rendered window: everything the row prints after its label.
fn window_key(window: &UsageWindow) -> String {
    format!(
        "{}|{}|{}|{}",
        window.label,
        window
            .remaining_percent
            .map_or_else(|| "null".to_owned(), |value| value.to_string()),
        window
            .resets_at
            .map_or_else(|| "null".to_owned(), |value| value.to_string()),
        window.note.as_deref().unwrap_or("null")
    )
}

fn render_window(window: &UsageWindow, now: i64) -> String {
    let percent = window.remaining_percent.map_or_else(
        || "n/a".to_owned(),
        |value| format!("{}%", trim_number(value)),
    );
    let reset = window
        .resets_at
        .map(|at| format!("리셋 {}", format_reset(at, now)));
    let mut parts = vec![percent];
    if let Some(note) = &window.note {
        parts.push(note.clone());
    }
    parts.extend(reset);
    parts.join(" · ")
}

/// Relative inside two days, absolute beyond it, so "3일 후" never hides a date.
fn format_reset(at: i64, now: i64) -> String {
    let remaining = at - now;
    if remaining <= 0 {
        return "도래".to_owned();
    }
    if remaining < 48 * 60 * 60 * 1000 {
        return format_ago(at, now, "후");
    }
    chrono::DateTime::from_timestamp_millis(at)
        .map(|instant| {
            instant
                .with_timezone(&chrono::Local)
                .format("%-m/%-d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "날짜 불명".to_owned())
}

fn format_ago(at: i64, now: i64, suffix: &str) -> String {
    let total = (((at - now).abs() as f64) / 1000.0).round() as i64;
    let days = total / 86_400;
    let hours = (total % 86_400) / 3600;
    let minutes = (total % 3600) / 60;
    if days > 0 {
        return format!("{days}일 {hours}시간 {suffix}");
    }
    if hours > 0 {
        return format!("{hours}시간 {minutes}분 {suffix}");
    }
    format!("{minutes}분 {suffix}")
}

/// Renders a percentage the way JS stringifies a number: no trailing `.0`.
fn trim_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        let text = format!("{value}");
        text
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{UsageModelQuota, UsageWindow};

    fn snapshot(state: HealthState) -> HealthSnapshot {
        HealthSnapshot {
            state,
            detail: "정상 (HTTP 200)".into(),
            consecutive_failures: 0,
            last_status: Some(200),
            retry_at: None,
            updated_at: 0,
        }
    }

    fn config() -> CpaConfig {
        CpaConfig {
            root: "http://host:8317".into(),
            providers: Vec::new(),
            source: "env".into(),
            has_api_key: true,
            has_management_key: false,
        }
    }

    #[test]
    fn korean_text_counts_as_two_columns() {
        assert_eq!(display_width("CPA 정상"), 8);
        assert_eq!(display_width("abc"), 3);
        assert_eq!(pad("주력", 6), "주력  ");
        assert_eq!(pad("too-long", 3), "too-long");
    }

    #[test]
    fn the_status_line_omits_counts_until_the_catalog_resolves() {
        let unknown = render_status_line(None, &snapshot(HealthState::Unknown));
        assert_eq!(unknown, "CPA 미확인");
        let known = render_status_line(
            Some(TierCounts {
                primary: 12,
                last: 30,
                chat_unfit: 3,
            }),
            &snapshot(HealthState::Down),
        );
        assert_eq!(known, "CPA 다운 · 주력 12종 · 최후 30종");
    }

    #[test]
    fn the_report_states_every_section() {
        let health = snapshot(HealthState::Ok);
        let report = render_report_at(
            &ReportInput {
                config: &config(),
                tiers: Some(TierCounts {
                    primary: 2,
                    last: 3,
                    chat_unfit: 1,
                }),
                tier_reason: None,
                health: &health,
                usage: &UsageResult::Unsupported {
                    reason: "Management 키 없음".into(),
                },
            },
            0,
        );
        assert!(report.starts_with("CPA 상태\n"));
        assert!(report.contains("서버        http://host:8317"));
        assert!(report.contains("상태        정상 · 정상 (HTTP 200)"));
        assert!(report.contains("  주력 2종 · 최후 3종 · 대화 불가 1종"));
        assert!(report.contains("omo 설정 파일은 읽지도 쓰지도 않습니다"));
        assert!(report.contains("  n/a · Management 키 없음"));
    }

    #[test]
    fn an_unresolved_catalog_reports_the_reason() {
        let health = snapshot(HealthState::Unknown);
        let report = render_report_at(
            &ReportInput {
                config: &config(),
                tiers: None,
                tier_reason: Some("점검 실패: boom"),
                health: &health,
                usage: &UsageResult::Supported { accounts: Vec::new() },
            },
            0,
        );
        assert!(report.contains("  확인 불가 · 점검 실패: boom"));
        assert!(report.contains("  계정 없음"));
    }

    fn window(label: &str, remaining: Option<f64>, resets_at: Option<i64>, note: Option<&str>) -> UsageWindow {
        UsageWindow {
            label: label.into(),
            remaining_percent: remaining,
            resets_at,
            note: note.map(str::to_owned),
        }
    }

    fn account(label: &str, status: UsageAccountStatus, windows: Vec<UsageWindow>, models: Vec<UsageModelQuota>) -> UsageAccount {
        UsageAccount {
            provider: "codex".into(),
            label: label.into(),
            status,
            detail: None,
            meta: None,
            windows,
            models,
            observed_at: Some(0),
        }
    }

    #[test]
    fn accounts_order_by_watermarks_then_name_and_render_aligned_windows() {
        let accounts = vec![
            account("b-disabled", UsageAccountStatus::Off, Vec::new(), Vec::new()),
            account(
                "a-live",
                UsageAccountStatus::Ok,
                vec![window("주", Some(61.0), None, None)],
                Vec::new(),
            ),
            account("c-healthy", UsageAccountStatus::Ok, Vec::new(), Vec::new()),
        ];
        let report = render_report_at(
            &ReportInput {
                config: &config(),
                tiers: None,
                tier_reason: None,
                health: &snapshot(HealthState::Ok),
                usage: &UsageResult::Supported { accounts },
            },
            0,
        );
        let live = report.find("a-live").expect("live account");
        let healthy = report.find("c-healthy").expect("healthy account");
        let disabled = report.find("b-disabled").expect("disabled account");
        assert!(live < healthy && healthy < disabled);
        assert!(report.contains("  a-live                         codex           정상"));
        assert!(report.contains("      주           61%"));
        assert!(report.contains("활성 2 · 비활성 1 · 사용량 관측 1"));
        assert!(report.contains("스냅샷 0분 전"));
    }

    #[test]
    fn a_disabled_account_without_a_detail_says_so() {
        let report = render_report_at(
            &ReportInput {
                config: &config(),
                tiers: None,
                tier_reason: None,
                health: &snapshot(HealthState::Ok),
                usage: &UsageResult::Supported {
                    accounts: vec![account(
                        "gone",
                        UsageAccountStatus::Off,
                        Vec::new(),
                        Vec::new(),
                    )],
                },
            },
            0,
        );
        assert!(report.contains("비활성 · 서버에서 사용 중지"));
    }

    #[test]
    fn model_scoped_windows_render_only_when_they_add_a_row() {
        let mirrored = UsageModelQuota {
            id: "claude-opus-5".into(),
            windows: vec![window("주", Some(61.0), None, None)],
            observed_at: Some(0),
        };
        let distinct = UsageModelQuota {
            id: "claude-sonnet-5".into(),
            windows: vec![window("overage", Some(10.0), None, None)],
            observed_at: Some(0),
        };
        let report = render_report_at(
            &ReportInput {
                config: &config(),
                tiers: None,
                tier_reason: None,
                health: &snapshot(HealthState::Ok),
                usage: &UsageResult::Supported {
                    accounts: vec![account(
                        "codex",
                        UsageAccountStatus::Ok,
                        vec![window("주", Some(61.0), None, None)],
                        vec![mirrored, distinct],
                    )],
                },
            },
            0,
        );
        assert!(!report.contains("claude-opus-5 주"));
        assert!(report.contains("claude-sonnet-5 overage"));
        assert!(report.contains("10%"));
    }

    #[test]
    fn reset_instants_switch_from_relative_to_absolute() {
        let now = 1_000_000_000_000;
        assert_eq!(format_reset(now - 1, now), "도래");
        assert_eq!(format_reset(now + 5 * 60 * 1000, now), "5분 후");
        let far = now + 30 * 24 * 60 * 60 * 1000;
        let rendered = format_reset(far, now);
        assert!(rendered.contains(':'));
        assert!(!rendered.contains('일'));
    }

    #[test]
    fn ages_render_in_the_largest_meaningful_unit() {
        let now = 0;
        assert_eq!(format_ago(30 * 60 * 1000, now, "전"), "30분 전");
        assert_eq!(format_ago(63 * 60 * 1000, now, "전"), "1시간 3분 전");
        assert_eq!(
            format_ago(26 * 60 * 60 * 1000, now, "전"),
            "1일 2시간 전"
        );
        // Future instants use their magnitude and the caller's suffix.
        assert_eq!(format_ago(5 * 60 * 1000, now, "후"), "5분 후");
    }

    #[test]
    fn percentages_drop_the_trailing_zero_but_keep_fractions() {
        assert_eq!(trim_number(61.0), "61");
        assert_eq!(trim_number(42.5), "42.5");
        let with_fraction = render_window(&window("w", Some(42.5), None, None), 0);
        assert_eq!(with_fraction, "42.5%");
        let bare = render_window(&window("w", None, None, Some("한도 도달")), 0);
        assert_eq!(bare, "n/a · 한도 도달");
    }
}
