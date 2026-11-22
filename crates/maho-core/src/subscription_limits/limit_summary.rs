//! One account's limit windows in plain words, for account lists (senpi `limit-summary.ts`,
//! oh-my-openagent#9234 and senpi#6289).
use super::limit_store::{AccountLimits, subscription_limits};
use crate::credential_account_status::format_account_retry_time;

/// How old a limit reading is, for account lists.
pub fn reading_age(ms: i64) -> String {
    let minutes = ms.div_euclid(60_000);
    if minutes < 1 {
        return "just now".to_owned();
    }
    if minutes < 60 {
        return format!("{minutes} min ago");
    }
    let hours = minutes / 60;
    if hours < 48 {
        format!("{hours} h ago")
    } else {
        format!("{} d ago", hours / 24)
    }
}

fn instant_ms(iso: &str) -> Option<i64> {
    iso.parse::<jiff::Timestamp>()
        .ok()
        .map(|at| at.as_millisecond())
}

/// `describeAccountLimits`: the share used and when each window resets, plus how old the reading is. A window whose
/// reset has passed since the reading says so instead of repeating a stale share; an account nothing has reported for
/// says so instead of showing 0%.
pub fn describe_account_limits(limits: Option<&AccountLimits>, now: i64) -> String {
    let Some(limits) = limits else {
        return "limits: not reported yet".to_owned();
    };
    if limits.windows.is_empty() {
        return match &limits.error {
            None => "limits: not reported yet".to_owned(),
            Some(error) => format!("limits: last check failed: {error}"),
        };
    }
    let windows: Vec<String> = limits
        .windows
        .iter()
        .map(|window| {
            let resets_at = window.resets_at.as_deref().and_then(instant_ms);
            if resets_at.is_some_and(|at| at <= now) {
                return format!("{} reset since this reading", window.label);
            }
            let used = format!("{} {}% used", window.label, window.used_percent.round());
            match resets_at {
                Some(at) => format!("{used}, resets {}", format_account_retry_time(at)),
                None => used,
            }
        })
        .collect();
    let reported_at = instant_ms(&limits.reported_at).unwrap_or(now);
    let failed = limits
        .error
        .as_ref()
        .map_or_else(String::new, |error| format!("; last check failed: {error}"));
    format!(
        "limits: {} (updated {}{failed})",
        windows.join("; "),
        reading_age(now - reported_at)
    )
}

/// `accountLimitsFor`: the latest limits the process holds for one provider account.
pub fn account_limits_for(provider: &str, account: &str) -> Option<AccountLimits> {
    subscription_limits()
        .snapshot()
        .into_iter()
        .find(|limits| limits.provider == provider && limits.account == account)
}

#[cfg(test)]
mod tests {
    use super::super::limit_windows::LimitWindow;
    use super::*;

    const NOW: i64 = 1_795_359_840_000;
    const HOUR: i64 = 3_600_000;

    fn iso(ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ms)
            .expect("timestamp")
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn window(label: &str, used_percent: f64, resets_at: Option<i64>) -> LimitWindow {
        LimitWindow {
            id: format!("w:{label}"),
            label: label.to_owned(),
            window_minutes: None,
            used_percent,
            resets_at: resets_at.map(iso),
        }
    }

    fn limits(windows: Vec<LimitWindow>, reported_at: i64, error: Option<&str>) -> AccountLimits {
        AccountLimits {
            provider: "anthropic-subscription".to_owned(),
            account: "work".to_owned(),
            windows,
            reported_at: iso(reported_at),
            plan: None,
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn a_reading_ages_from_just_now_through_minutes_and_hours_to_days() {
        assert_eq!(reading_age(-5), "just now");
        assert_eq!(reading_age(59_999), "just now");
        assert_eq!(reading_age(60_000), "1 min ago");
        assert_eq!(reading_age(59 * 60_000), "59 min ago");
        assert_eq!(reading_age(60 * 60_000), "1 h ago");
        assert_eq!(reading_age(47 * HOUR), "47 h ago");
        assert_eq!(reading_age(48 * HOUR), "2 d ago");
        assert_eq!(reading_age(100 * HOUR), "4 d ago");
    }

    #[test]
    fn each_window_shows_its_share_and_reset_and_the_reading_its_age() {
        let five_hour = NOW + 2 * HOUR;
        let week = NOW + 72 * HOUR;
        let reported = limits(
            vec![
                window("5h", 72.4, Some(five_hour)),
                window("7d", 31.0, Some(week)),
            ],
            NOW,
            None,
        );
        assert_eq!(
            describe_account_limits(Some(&reported), NOW),
            format!(
                "limits: 5h 72% used, resets {}; 7d 31% used, resets {} (updated just now)",
                format_account_retry_time(five_hour),
                format_account_retry_time(week)
            )
        );
    }

    #[test]
    fn a_window_that_reset_since_the_reading_says_so_and_a_failed_check_is_named() {
        let stale = limits(
            vec![
                window("5h", 100.0, Some(NOW - HOUR)),
                window("7d", 10.0, None),
            ],
            NOW - 5 * 60_000,
            Some("rate limited"),
        );
        assert_eq!(
            describe_account_limits(Some(&stale), NOW),
            "limits: 5h reset since this reading; 7d 10% used (updated 5 min ago; last check failed: rate limited)"
        );
    }

    #[test]
    fn an_account_nothing_reported_for_says_why_instead_of_showing_zero() {
        assert_eq!(
            describe_account_limits(None, NOW),
            "limits: not reported yet"
        );
        assert_eq!(
            describe_account_limits(Some(&limits(Vec::new(), NOW, None)), NOW),
            "limits: not reported yet"
        );
        assert_eq!(
            describe_account_limits(Some(&limits(Vec::new(), NOW, Some("sign-in needed"))), NOW),
            "limits: last check failed: sign-in needed"
        );
    }
}
