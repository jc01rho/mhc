//! One account's health in plain words: why it sits out and until when, the cases the desktop account row shows
//! (senpi `credential-account-status.ts`, senpi#4092 and senpi#5006), so no account list says a bare "blocked".
use maho_ext_api::CredentialAccountSummary;

/// The short zone name `toLocaleString("en-US", { timeZoneName: "short" })` prints: UTC and the US zones by name,
/// every other zone as `GMT+h[:mm]`.
fn zone_label(abbreviation: &str, offset_seconds: i32) -> String {
    const US_ZONES: [&str; 8] = ["EST", "EDT", "CST", "CDT", "MST", "MDT", "PST", "PDT"];
    if abbreviation == "UTC" || (offset_seconds < 0 && US_ZONES.contains(&abbreviation)) {
        return abbreviation.to_owned();
    }
    if offset_seconds == 0 {
        return "GMT".to_owned();
    }
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let hours = offset_seconds.abs() / 3600;
    let minutes = offset_seconds.abs() % 3600 / 60;
    if minutes == 0 {
        format!("GMT{sign}{hours}")
    } else {
        format!("GMT{sign}{hours}:{minutes:02}")
    }
}

fn retry_time_in(epoch_ms: i64, zone: jiff::tz::TimeZone) -> String {
    let Ok(timestamp) = jiff::Timestamp::from_millisecond(epoch_ms) else {
        return epoch_ms.to_string();
    };
    let zoned = timestamp.to_zoned(zone);
    let abbreviation = zoned.strftime("%Z").to_string();
    format!(
        "{} {}",
        zoned.strftime("%b %-d, %Y, %-I:%M %p"),
        zone_label(&abbreviation, zoned.offset().seconds())
    )
}

/// `formatAccountRetryTime`: the local wall-clock time an account block lifts, in the reader's time zone.
pub fn format_account_retry_time(epoch_ms: i64) -> String {
    retry_time_in(epoch_ms, jiff::tz::TimeZone::system())
}

/// `describeAccountBlock`: one account's health in plain words. `reason` is `disabled`, `auth_error` or `rate_limit`
/// (anything else, or none, reads as an unexplained block); `until` is the epoch ms the block lifts.
pub fn describe_account_block(blocked: bool, reason: Option<&str>, until: Option<i64>) -> String {
    if !blocked {
        return "available".to_owned();
    }
    let until = until.map(format_account_retry_time);
    match (reason, until) {
        (Some("disabled"), _) => "account disabled; needs attention".to_owned(),
        (Some("auth_error"), None) => "sign-in rejected; sign in again".to_owned(),
        (Some("auth_error"), Some(until)) => format!("sign-in rejected; next check after {until}"),
        (Some("rate_limit"), None) => "rate limited".to_owned(),
        (Some("rate_limit"), Some(until)) => format!("rate limited; retry after {until}"),
        (_, None) => "blocked; reason and retry time unavailable".to_owned(),
        (_, Some(until)) => format!("blocked; retry after {until}"),
    }
}

/// `describeCredentialAccount`: one `/account`-style list row: label, source, health, per-model limits and pin.
/// Never key material.
pub fn describe_credential_account(account: &CredentialAccountSummary) -> String {
    let availability = if account.unused {
        "unused (other login kind; switch with /login)".to_owned()
    } else {
        describe_account_block(
            account.blocked,
            account.blocked_reason,
            account
                .blocked_until
                .and_then(|until| i64::try_from(until).ok()),
        )
    };
    let mut states = vec![
        maho_ai::auth::pool::slots::account_label(&account.name, account.display_name.as_deref()),
        account.source.as_str().to_owned(),
        availability,
    ];
    for block in &account.blocked_models {
        states.push(format!(
            "blocked for {} until {}",
            block.model,
            format_account_retry_time(block.until)
        ));
    }
    if account.pinned {
        states.push("pinned".to_owned());
    }
    states.join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::{CredentialAccountBlockedModel, CredentialAccountSource};

    const UNTIL: i64 = 1_795_359_840_000;

    fn summary(
        blocked: bool,
        reason: Option<&'static str>,
        until: Option<i64>,
    ) -> CredentialAccountSummary {
        CredentialAccountSummary {
            name: "limited".into(),
            display_name: None,
            source: CredentialAccountSource::Login,
            blocked,
            blocked_reason: reason,
            blocked_until: until.and_then(|until| u64::try_from(until).ok()),
            pinned: false,
            blocked_models: Vec::new(),
            unused: false,
        }
    }

    #[test]
    fn the_retry_time_is_a_wall_clock_time_with_a_short_zone_name() {
        assert_eq!(
            retry_time_in(UNTIL, jiff::tz::TimeZone::UTC),
            "Nov 22, 2026, 3:04 PM UTC"
        );
        assert_eq!(
            retry_time_in(UNTIL + 9 * 3_600_000, jiff::tz::TimeZone::UTC),
            "Nov 23, 2026, 12:04 AM UTC"
        );
    }

    #[test]
    fn zone_names_follow_the_en_us_short_form() {
        assert_eq!(zone_label("UTC", 0), "UTC");
        assert_eq!(zone_label("PST", -28_800), "PST");
        assert_eq!(zone_label("EDT", -14_400), "EDT");
        assert_eq!(zone_label("GMT", 0), "GMT");
        assert_eq!(zone_label("KST", 32_400), "GMT+9");
        assert_eq!(zone_label("IST", 19_800), "GMT+5:30");
        assert_eq!(zone_label("CET", 3_600), "GMT+1");
        assert_eq!(zone_label("-03", -10_800), "GMT-3");
    }

    #[test]
    fn a_blocked_account_says_why_it_sits_out_and_until_when() {
        let retry = format_account_retry_time(UNTIL);
        assert_eq!(describe_account_block(false, None, None), "available");
        assert_eq!(
            describe_account_block(true, Some("disabled"), None),
            "account disabled; needs attention"
        );
        assert_eq!(
            describe_account_block(true, Some("auth_error"), None),
            "sign-in rejected; sign in again"
        );
        assert_eq!(
            describe_account_block(true, Some("auth_error"), Some(UNTIL)),
            format!("sign-in rejected; next check after {retry}")
        );
        assert_eq!(
            describe_account_block(true, Some("rate_limit"), None),
            "rate limited"
        );
        assert_eq!(
            describe_account_block(true, Some("rate_limit"), Some(UNTIL)),
            format!("rate limited; retry after {retry}")
        );
        assert_eq!(
            describe_account_block(true, None, None),
            "blocked; reason and retry time unavailable"
        );
        assert_eq!(
            describe_account_block(true, None, Some(UNTIL)),
            format!("blocked; retry after {retry}")
        );
    }

    #[test]
    fn an_account_row_carries_label_source_health_model_blocks_and_pin() {
        let retry = format_account_retry_time(UNTIL);
        let mut account = summary(true, Some("rate_limit"), Some(UNTIL));
        account.display_name = Some("Work".into());
        account.pinned = true;
        account.blocked_models = vec![CredentialAccountBlockedModel {
            model: "fable".into(),
            until: UNTIL,
        }];
        assert_eq!(
            describe_credential_account(&account),
            format!(
                "Work (limited) | login | rate limited; retry after {retry} | blocked for fable until {retry} | pinned"
            )
        );
        account.unused = true;
        assert!(
            describe_credential_account(&account)
                .contains("| unused (other login kind; switch with /login) |")
        );
        assert_eq!(
            describe_credential_account(&summary(false, None, None)),
            "limited | login | available"
        );
    }
}
