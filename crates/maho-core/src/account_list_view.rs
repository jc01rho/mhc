//! One account block of a TUI account list: a header line with the label and short tags, then aligned `label  value`
//! rows for status and each limit window (senpi `account-list-view.ts`, senpi#6288 and senpi#6289). Plain text,
//! because RPC clients receive the same notice.
use crate::subscription_limits::{limit_store::AccountLimits, limit_summary::reading_age};

const MINUTE: i64 = 60_000;
const DAY: i64 = 24 * 60 * MINUTE;

/// The time zone a list shows clock times in.
#[derive(Clone)]
pub struct ListZone(jiff::tz::TimeZone);

impl ListZone {
    pub fn system() -> Self {
        Self(jiff::tz::TimeZone::system())
    }

    pub fn utc() -> Self {
        Self(jiff::tz::TimeZone::UTC)
    }
}

/// `AccountListEntry`: one account of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountListEntry {
    pub label: String,
    pub tags: Vec<String>,
    /// One phrase per reason the account sits out, or `available`.
    pub status: Vec<String>,
    pub limits: Option<AccountLimits>,
}

fn relative(ms: i64) -> String {
    let minutes = (ms + MINUTE - 1).div_euclid(MINUTE);
    if minutes < 1 {
        return "now".to_owned();
    }
    if minutes < 60 {
        return format!("in {minutes} min");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return if minutes % 60 == 0 {
            format!("in {hours} h")
        } else {
            format!("in {hours} h {} min", minutes % 60)
        };
    }
    let days = hours / 24;
    if hours % 24 == 0 {
        format!("in {days} d")
    } else {
        format!("in {days} d {} h", hours % 24)
    }
}

/// Local clock time: the time alone on today's date, a weekday within six days, a month and day beyond.
fn clock(epoch_ms: i64, now: i64, zone: &ListZone) -> String {
    let (Ok(at), Ok(current)) = (
        jiff::Timestamp::from_millisecond(epoch_ms),
        jiff::Timestamp::from_millisecond(now),
    ) else {
        return epoch_ms.to_string();
    };
    let at = at.to_zoned(zone.0.clone());
    let current = current.to_zoned(zone.0.clone());
    let time = at.strftime("%-I:%M %p").to_string();
    if at.date() == current.date() {
        return time;
    }
    let day = if epoch_ms - now < 6 * DAY {
        at.strftime("%a").to_string()
    } else {
        format!("{},", at.strftime("%b %-d"))
    };
    format!("{day} {time}")
}

/// `formatWhen`: "in 3 h (10:44 PM)": how long first, then the local clock time without year or time zone.
pub fn format_when(epoch_ms: i64, now: i64, zone: &ListZone) -> String {
    format!(
        "{} ({})",
        relative(epoch_ms - now),
        clock(epoch_ms, now, zone)
    )
}

/// `describeBlockStatus`: an account-level block in list wording; the reasons match `describe_account_block`.
pub fn describe_block_status(
    blocked: bool,
    reason: Option<&str>,
    until: Option<i64>,
    now: i64,
    zone: &ListZone,
) -> String {
    if !blocked {
        return "available".to_owned();
    }
    let when = until.map(|until| format_when(until, now, zone));
    match (reason, when) {
        (Some("disabled"), _) => "account disabled; needs attention".to_owned(),
        (Some("auth_error"), None) => "sign-in rejected; sign in again".to_owned(),
        (Some("auth_error"), Some(when)) => format!("sign-in rejected, next check {when}"),
        (Some("rate_limit"), None) => "rate limited".to_owned(),
        (Some("rate_limit"), Some(when)) => format!("rate limited, retry {when}"),
        (_, None) => "blocked; reason and retry time unavailable".to_owned(),
        (_, Some(when)) => format!("blocked, retry {when}"),
    }
}

fn instant_ms(iso: &str) -> Option<i64> {
    iso.parse::<jiff::Timestamp>()
        .ok()
        .map(|at| at.as_millisecond())
}

fn limit_rows(limits: Option<&AccountLimits>, now: i64, zone: &ListZone) -> Vec<(String, String)> {
    let row = |label: &str, value: String| (label.to_owned(), value);
    let Some(limits) = limits else {
        return vec![row("limits", "not reported yet".to_owned())];
    };
    if limits.windows.is_empty() {
        let value = match &limits.error {
            None => "not reported yet".to_owned(),
            Some(error) => format!("last check failed: {error}"),
        };
        return vec![row("limits", value)];
    }
    let mut rows: Vec<(String, String)> = limits
        .windows
        .iter()
        .map(|window| {
            let resets_at = window.resets_at.as_deref().and_then(instant_ms);
            if resets_at.is_some_and(|at| at <= now) {
                return row(&window.label, "reset since this reading".to_owned());
            }
            let used = format!("{:>4} used", format!("{}%", window.used_percent.round()));
            let value = match resets_at {
                Some(at) => format!("{used}, resets {}", format_when(at, now, zone)),
                None => used,
            };
            row(&window.label, value)
        })
        .collect();
    let failed = limits
        .error
        .as_ref()
        .map_or_else(String::new, |error| format!("; last check failed: {error}"));
    let reported_at = instant_ms(&limits.reported_at).unwrap_or(now);
    rows.push(row(
        "updated",
        format!("{}{failed}", reading_age(now - reported_at)),
    ));
    rows
}

/// `renderAccountList`: a counted title, one block per account, then footer lines after a blank line.
pub fn render_account_list(
    title: &str,
    entries: &[AccountListEntry],
    footer: &[String],
    now: i64,
    zone: &ListZone,
) -> String {
    if entries.is_empty() {
        let mut lines = vec![format!("{title}: none")];
        lines.extend(footer.iter().cloned());
        return lines.join("\n");
    }
    let blocks: Vec<(&AccountListEntry, Vec<(String, String)>)> = entries
        .iter()
        .map(|entry| {
            let mut rows: Vec<(String, String)> = entry
                .status
                .iter()
                .enumerate()
                .map(|(index, status)| {
                    let label = if index == 0 { "status" } else { "" };
                    (label.to_owned(), status.clone())
                })
                .collect();
            rows.extend(limit_rows(entry.limits.as_ref(), now, zone));
            (entry, rows)
        })
        .collect();
    let width = blocks
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|(label, _)| label.chars().count()))
        .max()
        .unwrap_or(0);
    let mut lines = vec![format!("{title} ({})", entries.len())];
    for (entry, rows) in &blocks {
        lines.push(String::new());
        lines.push(if entry.tags.is_empty() {
            entry.label.clone()
        } else {
            format!("{}  [{}]", entry.label, entry.tags.join(", "))
        });
        for (label, value) in rows {
            lines.push(format!("  {label:<width$}  {value}"));
        }
    }
    if !footer.is_empty() {
        lines.push(String::new());
        lines.extend(footer.iter().cloned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription_limits::limit_windows::LimitWindow;

    const NOW: i64 = 1_794_225_600_000;
    const HOUR: i64 = 3_600_000;

    fn iso(ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ms)
            .expect("timestamp")
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn window(label: &str, used_percent: f64, resets_at: i64) -> LimitWindow {
        LimitWindow {
            id: format!("w:{label}"),
            label: label.to_owned(),
            window_minutes: None,
            used_percent,
            resets_at: Some(iso(resets_at)),
        }
    }

    fn limits(windows: Vec<LimitWindow>) -> AccountLimits {
        AccountLimits {
            provider: "anthropic-subscription".to_owned(),
            account: "work".to_owned(),
            windows,
            reported_at: iso(NOW),
            plan: None,
            error: None,
        }
    }

    fn entry(
        label: &str,
        tags: &[&str],
        status: &[&str],
        limits: Option<AccountLimits>,
    ) -> AccountListEntry {
        AccountListEntry {
            label: label.to_owned(),
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            status: status.iter().map(|state| (*state).to_owned()).collect(),
            limits,
        }
    }

    #[test]
    fn times_read_as_how_long_first_then_a_short_local_clock() {
        let utc = ListZone::utc();
        assert_eq!(format_when(NOW + 2 * HOUR, NOW, &utc), "in 2 h (2:00 PM)");
        assert_eq!(
            format_when(NOW + 90 * 60_000, NOW, &utc),
            "in 1 h 30 min (1:30 PM)"
        );
        assert_eq!(
            format_when(NOW + 45 * 60_000, NOW, &utc),
            "in 45 min (12:45 PM)"
        );
        assert_eq!(
            format_when(NOW + 72 * HOUR, NOW, &utc),
            "in 3 d (Thu 12:00 PM)"
        );
        assert_eq!(
            format_when(NOW + 26 * HOUR, NOW, &utc),
            "in 1 d 2 h (Tue 2:00 PM)"
        );
        assert_eq!(
            format_when(NOW + 10 * 24 * HOUR, NOW, &utc),
            "in 10 d (Nov 19, 12:00 PM)"
        );
        assert_eq!(format_when(NOW - HOUR, NOW, &utc), "now (11:00 AM)");
    }

    #[test]
    fn a_block_says_why_the_account_sits_out_and_when_it_is_due_back() {
        let utc = ListZone::utc();
        let until = Some(NOW + 3 * HOUR);
        assert_eq!(
            describe_block_status(false, None, None, NOW, &utc),
            "available"
        );
        assert_eq!(
            describe_block_status(true, Some("rate_limit"), until, NOW, &utc),
            "rate limited, retry in 3 h (3:00 PM)"
        );
        assert_eq!(
            describe_block_status(true, Some("auth_error"), until, NOW, &utc),
            "sign-in rejected, next check in 3 h (3:00 PM)"
        );
        assert_eq!(
            describe_block_status(true, Some("auth_error"), None, NOW, &utc),
            "sign-in rejected; sign in again"
        );
        assert_eq!(
            describe_block_status(true, Some("disabled"), None, NOW, &utc),
            "account disabled; needs attention"
        );
        assert_eq!(
            describe_block_status(true, None, until, NOW, &utc),
            "blocked, retry in 3 h (3:00 PM)"
        );
        assert_eq!(
            describe_block_status(true, None, None, NOW, &utc),
            "blocked; reason and retry time unavailable"
        );
    }

    #[test]
    fn a_list_is_one_aligned_block_per_account_with_limit_rows_and_a_footer() {
        let entries = [
            entry(
                "Personal (alpha)",
                &["pinned in settings", "next turn"],
                &["available"],
                Some(limits(vec![
                    window("5h", 72.4, NOW + 2 * HOUR),
                    window("7d", 31.0, NOW + 72 * HOUR),
                ])),
            ),
            entry(
                "bravo",
                &[],
                &["rate limited, retry in 3 h (3:00 PM)"],
                Some(limits(vec![window("5h", 100.0, NOW + 3 * HOUR)])),
            ),
            entry(
                "charlie",
                &[],
                &["fable blocked, retry in 1 d 2 h (Tue 2:00 PM)"],
                None,
            ),
        ];
        let footer = [
            "Manage: /claude-account add | pin <id> | unpin | unblock <id> | remove <id>"
                .to_owned(),
        ];

        assert_eq!(
            render_account_list(
                "Anthropic Subscription accounts",
                &entries,
                &footer,
                NOW,
                &ListZone::utc()
            ),
            [
                "Anthropic Subscription accounts (3)",
                "",
                "Personal (alpha)  [pinned in settings, next turn]",
                "  status   available",
                "  5h        72% used, resets in 2 h (2:00 PM)",
                "  7d        31% used, resets in 3 d (Thu 12:00 PM)",
                "  updated  just now",
                "",
                "bravo",
                "  status   rate limited, retry in 3 h (3:00 PM)",
                "  5h       100% used, resets in 3 h (3:00 PM)",
                "  updated  just now",
                "",
                "charlie",
                "  status   fable blocked, retry in 1 d 2 h (Tue 2:00 PM)",
                "  limits   not reported yet",
                "",
                "Manage: /claude-account add | pin <id> | unpin | unblock <id> | remove <id>",
            ]
            .join("\n")
        );
    }

    #[test]
    fn an_empty_list_is_one_line_then_the_footer() {
        let footer = [
            "Pinned account: gone (settings) - not in this list".to_owned(),
            "Add one with /claude-account add.".to_owned(),
        ];
        assert_eq!(
            render_account_list(
                "Anthropic Subscription accounts",
                &[],
                &footer,
                NOW,
                &ListZone::utc()
            ),
            [
                "Anthropic Subscription accounts: none",
                "Pinned account: gone (settings) - not in this list",
                "Add one with /claude-account add.",
            ]
            .join("\n")
        );
    }

    #[test]
    fn a_window_that_reset_since_the_reading_and_a_failed_check_read_plainly() {
        let mut stale = limits(vec![window("5h", 100.0, NOW - HOUR)]);
        stale.reported_at = iso(NOW - 10 * 60_000);
        stale.error = Some("sign-in needed".to_owned());
        let rows = limit_rows(Some(&stale), NOW, &ListZone::utc());
        assert_eq!(
            rows,
            [
                ("5h".to_owned(), "reset since this reading".to_owned()),
                (
                    "updated".to_owned(),
                    "10 min ago; last check failed: sign-in needed".to_owned()
                ),
            ]
        );
        let failed_only = AccountLimits {
            windows: Vec::new(),
            ..stale
        };
        assert_eq!(
            limit_rows(Some(&failed_only), NOW, &ListZone::utc()),
            [(
                "limits".to_owned(),
                "last check failed: sign-in needed".to_owned()
            )]
        );
    }
}
