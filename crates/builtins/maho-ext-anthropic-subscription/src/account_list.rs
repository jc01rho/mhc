//! The `/claude-account` list: one readable block per account, pin and next turn as tags (senpi `account-list.ts`,
//! senpi#6288 and senpi#6289).
use crate::{
    accounts::AccountSlot, affinity::PinSource, auth_lane::PROVIDER_ID,
    model_blocks::prune_model_blocks,
};
use maho_core::{
    account_list_view::{
        AccountListEntry, ListZone, describe_block_status, format_when, render_account_list,
    },
    subscription_limits::{limit_store::AccountLimits, limit_summary::account_limits_for},
};

pub struct ClaudeAccountListInput<'a> {
    pub accounts: &'a [AccountSlot],
    pub pinned: Option<&'a str>,
    pub pin_source: PinSource,
    pub affinity_pick: Option<&'a str>,
    pub affinity_error: Option<&'a str>,
    pub now: f64,
}

const MANAGE_HINT: &str =
    "Manage: /claude-account add | pin <id> | unpin | unblock <id> | remove <id>";
const TITLE: &str = "Anthropic Subscription accounts";

fn pin_tag(source: PinSource) -> &'static str {
    match source {
        PinSource::Cli => "pinned for this session",
        PinSource::Settings => "pinned in settings",
        PinSource::Stored => "pinned",
    }
}

fn pin_source_name(source: PinSource) -> &'static str {
    match source {
        PinSource::Cli => "cli",
        PinSource::Settings => "settings",
        PinSource::Stored => "stored",
    }
}

fn slot_status(slot: &AccountSlot, now: f64, zone: &ListZone) -> Vec<String> {
    let (blocked, reason, until) = crate::account_management::timed_block(
        slot.blocked_until,
        slot.block_reason.as_deref(),
        now,
    );
    let mut states = Vec::new();
    if blocked {
        states.push(describe_block_status(
            blocked,
            reason,
            until.map(|until| until as i64),
            now as i64,
            zone,
        ));
    }
    for (model, block) in
        prune_model_blocks(slot.model_blocks.as_ref(), now, None).unwrap_or_default()
    {
        states.push(format!(
            "{model} blocked, retry {}",
            format_when(block.blocked_until as i64, now as i64, zone)
        ));
    }
    if states.is_empty() {
        states.push("available".to_owned());
    }
    states
}

/// `formatClaudeAccountList` over a given zone and limits lookup, so the list can be rendered without the process-wide
/// limit store and the machine's time zone.
pub fn format_claude_account_list_with(
    input: &ClaudeAccountListInput<'_>,
    zone: &ListZone,
    limits_for: &dyn Fn(&str) -> Option<AccountLimits>,
) -> String {
    let now = input.now;
    let entries: Vec<AccountListEntry> = input
        .accounts
        .iter()
        .map(|account| {
            let mut tags = Vec::new();
            if input.pinned == Some(account.name.as_str()) {
                tags.push(pin_tag(input.pin_source).to_owned());
            }
            if input.affinity_pick == Some(account.name.as_str()) {
                tags.push("next turn".to_owned());
            }
            if account.source == crate::accounts::AccountSource::Env {
                tags.push("from environment".to_owned());
            }
            AccountListEntry {
                label: maho_ai::auth::pool::slots::account_label(
                    &account.name,
                    account.display_name.as_deref(),
                ),
                tags,
                status: slot_status(account, now, zone),
                limits: limits_for(&account.name),
            }
        })
        .collect();
    let missing_pin = input
        .pinned
        .filter(|pinned| !input.accounts.iter().any(|account| account.name == *pinned))
        .map(|pinned| {
            format!(
                "Pinned account: {pinned} ({}) - not in this list",
                pin_source_name(input.pin_source)
            )
        });
    if entries.is_empty() {
        let mut footer: Vec<String> = missing_pin.into_iter().collect();
        footer.push("Add one with /claude-account add.".to_owned());
        return render_account_list(TITLE, &entries, &footer, now as i64, zone);
    }
    let mut footer: Vec<String> = missing_pin.into_iter().collect();
    if let Some(error) = input.affinity_error {
        footer.push(format!("Next turn: unavailable - {error}"));
    }
    footer.push(MANAGE_HINT.to_owned());
    render_account_list(TITLE, &entries, &footer, now as i64, zone)
}

pub fn format_claude_account_list(input: &ClaudeAccountListInput<'_>) -> String {
    format_claude_account_list_with(input, &ListZone::system(), &|name| {
        account_limits_for(PROVIDER_ID, name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_blocks::{ModelBlock, ModelBlocks};
    use maho_core::subscription_limits::limit_windows::LimitWindow;

    const NOW: f64 = 1_794_225_600_000.0;
    const HOUR: f64 = 3_600_000.0;

    fn iso(ms: f64) -> String {
        chrono::DateTime::from_timestamp_millis(ms as i64)
            .expect("timestamp")
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn limits_of(account: &str, windows: &[(&str, f64, f64)]) -> AccountLimits {
        AccountLimits {
            provider: PROVIDER_ID.to_owned(),
            account: account.to_owned(),
            windows: windows
                .iter()
                .map(|(label, used_percent, resets_at)| LimitWindow {
                    id: format!("w:{label}"),
                    label: (*label).to_owned(),
                    window_minutes: None,
                    used_percent: *used_percent,
                    resets_at: Some(iso(*resets_at)),
                })
                .collect(),
            reported_at: iso(NOW),
            plan: None,
            error: None,
        }
    }

    fn slot(name: &str) -> AccountSlot {
        AccountSlot {
            name: name.to_owned(),
            ..Default::default()
        }
    }

    fn list(input: &ClaudeAccountListInput<'_>) -> String {
        format_claude_account_list_with(input, &ListZone::utc(), &|name| match name {
            "alpha" => Some(limits_of(
                "alpha",
                &[
                    ("5h", 72.4, NOW + 2.0 * HOUR),
                    ("7d", 31.0, NOW + 72.0 * HOUR),
                ],
            )),
            "bravo" => Some(limits_of("bravo", &[("5h", 100.0, NOW + 3.0 * HOUR)])),
            _ => None,
        })
    }

    #[test]
    fn each_account_is_a_readable_block_with_status_limits_pin_and_next_turn_tags() {
        let alpha = AccountSlot {
            display_name: Some("Personal".to_owned()),
            ..slot("alpha")
        };
        let bravo = AccountSlot {
            blocked_until: Some(NOW + 3.0 * HOUR),
            block_reason: Some("rate_limit".to_owned()),
            ..slot("bravo")
        };
        let charlie_blocks: ModelBlocks = [(
            "fable".to_owned(),
            ModelBlock {
                blocked_until: NOW + 26.0 * HOUR,
            },
        )]
        .into();
        let charlie = AccountSlot {
            model_blocks: Some(charlie_blocks),
            ..slot("charlie")
        };
        let accounts = [alpha, bravo, charlie];

        let text = list(&ClaudeAccountListInput {
            accounts: &accounts,
            pinned: Some("alpha"),
            pin_source: PinSource::Settings,
            affinity_pick: Some("alpha"),
            affinity_error: None,
            now: NOW,
        });

        assert_eq!(
            text,
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
    fn a_pin_that_matches_no_account_and_an_unavailable_next_turn_are_named_below_the_list() {
        let accounts = [slot("alpha")];
        let text = list(&ClaudeAccountListInput {
            accounts: &accounts,
            pinned: Some("gone"),
            pin_source: PinSource::Settings,
            affinity_pick: None,
            affinity_error: Some("account fallback is off"),
            now: NOW,
        });
        assert!(
            text.contains("Pinned account: gone (settings) - not in this list"),
            "{text}"
        );
        assert!(
            text.contains("\nNext turn: unavailable - account fallback is off\n"),
            "{text}"
        );
        assert!(!text.contains("[pinned"), "{text}");
    }

    #[test]
    fn with_no_stored_account_the_configured_pin_is_still_named() {
        let text = list(&ClaudeAccountListInput {
            accounts: &[],
            pinned: Some("gone"),
            pin_source: PinSource::Settings,
            affinity_pick: None,
            affinity_error: None,
            now: NOW,
        });
        assert_eq!(
            text,
            [
                "Anthropic Subscription accounts: none",
                "Pinned account: gone (settings) - not in this list",
                "Add one with /claude-account add.",
            ]
            .join("\n")
        );
    }

    #[test]
    fn an_environment_account_is_tagged_and_a_disabled_one_needs_attention() {
        let env = AccountSlot {
            source: crate::accounts::AccountSource::Env,
            ..slot("env")
        };
        let disabled = AccountSlot {
            block_reason: Some("account_disabled".to_owned()),
            ..slot("off")
        };
        let accounts = [env, disabled];
        let text = list(&ClaudeAccountListInput {
            accounts: &accounts,
            pinned: None,
            pin_source: PinSource::Stored,
            affinity_pick: None,
            affinity_error: None,
            now: NOW,
        });
        assert!(
            text.contains("env  [from environment]\n  status  available"),
            "{text}"
        );
        assert!(
            text.contains("off\n  status  account disabled; needs attention"),
            "{text}"
        );
    }
}
