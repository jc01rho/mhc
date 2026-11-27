//! Device and listing-row resolution for `mhc remote` via `maho-thread-address-core` (omo #14305, #14318, #14327, #14295).

use std::collections::{BTreeMap, BTreeSet};

use maho_thread_address_core::{
    AddressRefusal, DeviceListing, DeviceRef, DeviceResolution, DeviceRoster, DeviceRow, KeyedRow,
    ListingRound, NamedRow, RefusalCode, RefusalDetails, ResolveDeviceOptions, RowName,
    device_listing, no_device, resolve_device, row_key, row_name,
};
use serde_json::{Value, json};

use crate::exit::RemoteExit;

pub(crate) fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub(crate) fn non_empty<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    let value = text(row, key);
    (!value.trim().is_empty()).then_some(value)
}

pub fn roster(rows: &[Value]) -> DeviceRoster {
    let here = rows
        .iter()
        .find(|row| row.get("this_device").and_then(Value::as_bool) == Some(true))
        .map(|row| text(row, "device").to_owned())
        .filter(|id| !id.is_empty());
    DeviceRoster {
        here,
        devices: rows
            .iter()
            .map(|row| DeviceRow {
                id: text(row, "device").to_owned(),
                name: text(row, "label").to_owned(),
                online: row.get("online").and_then(Value::as_bool) == Some(true),
                last_seen_ms: row.get("last_seen_ms").and_then(Value::as_i64),
            })
            .collect(),
        listing_of: None,
    }
}

pub fn typed_device(rows: &[Value], typed: &str) -> Result<DeviceRef, Box<AddressRefusal>> {
    match resolve_device(
        &roster(rows),
        typed,
        ResolveDeviceOptions {
            online: true,
            exact_id: false,
        },
    ) {
        DeviceResolution::Ok(device) => Ok(device),
        DeviceResolution::Refusal(refusal) => Err(Box::new(refusal)),
    }
}

/// Resolves a `--device` filter via `resolve_device` (`online: false`); an unknown name/id passes through so the
/// listing itself decides whether a listing-named device exists (#14305 U6, #13402).
pub fn device_filter_target(rows: &[Value], typed: &str) -> Result<String, Box<AddressRefusal>> {
    match resolve_device(
        &roster(rows),
        typed,
        ResolveDeviceOptions {
            online: false,
            exact_id: false,
        },
    ) {
        DeviceResolution::Ok(device) => Ok(device.id),
        DeviceResolution::Refusal(refusal) if refusal.reason == "no_device" => Ok(typed.to_owned()),
        DeviceResolution::Refusal(refusal) => Err(Box::new(refusal)),
    }
}

pub fn round_names(device: &str, rows: &[Value], incomplete: &[Value], devices: &Value) -> bool {
    rows.iter().any(|row| text(row, "device") == device)
        || incomplete
            .iter()
            .any(|named| named.as_str() == Some(device))
        || devices
            .as_object()
            .is_some_and(|states| states.contains_key(device))
}

/// Refuses a `--device` filter only when neither the roster nor the listing round names that device (#13402).
pub fn unknown_device(
    device: &str,
    roster_rows: &[Value],
    listing_rows: &[Value],
    incomplete: &[Value],
    listing_devices: &Value,
) -> Option<AddressRefusal> {
    if roster_rows.iter().any(|row| text(row, "device") == device)
        || round_names(device, listing_rows, incomplete, listing_devices)
    {
        return None;
    }
    Some(no_device(device, &roster(roster_rows).devices))
}

/// Placeholder row for a device the listing names but the roster missed; surfaced offline to match `sessions` (#14327 L-b).
pub fn listing_named_device_row(id: &str) -> Value {
    json!({
        "device": id,
        "label": id,
        "kind": "unknown",
        "platform": "unknown",
        "online": false,
        "last_seen_ms": null,
    })
}

fn listing_round(incomplete: &[Value], devices: &Value) -> ListingRound {
    let incomplete_ids = incomplete
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let states = devices.as_object().map(|map| {
        map.iter()
            .filter_map(|(key, value)| value.as_str().map(|state| (key.clone(), state.to_owned())))
            .collect::<BTreeMap<String, String>>()
    });
    ListingRound {
        incomplete: incomplete_ids,
        states,
    }
}

/// Computes deduplicated incomplete device IDs for a listing round via `device_listing` (#14295, #14305 U6).
pub fn unanswered_devices(
    roster_rows: &[Value],
    incomplete: &[Value],
    devices: &Value,
) -> Vec<String> {
    let roster_devices = roster(roster_rows).devices;
    let round = listing_round(incomplete, devices);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut ordered: Vec<String> = Vec::new();
    for id in round
        .incomplete
        .iter()
        .chain(round.states.iter().flat_map(|map| map.keys()))
    {
        if seen.insert(id.clone())
            && device_listing(id, &round, &roster_devices) == DeviceListing::Incomplete
        {
            ordered.push(id.clone());
        }
    }
    ordered
}

pub fn row_identity_key(row: &Value) -> String {
    let keyed = KeyedRow {
        device: text(row, "device").to_owned(),
        durable_id: non_empty(row, "durable_id").map(str::to_owned),
        session_id: text(row, "session_id").to_owned(),
    };
    row_key(&keyed)
}

pub fn row_display_name(row: &Value) -> RowName {
    let folder = non_empty(row, "path")
        .filter(|path| *path != "unknown")
        .unwrap_or_else(|| text(row, "project"))
        .to_owned();
    row_name(&NamedRow {
        name: non_empty(row, "title").map(str::to_owned),
        first_message: None,
        folder,
    })
}

pub fn enrich_listing_row(row: &Value) -> Value {
    let mut updated = row.clone();
    if let Some(object) = updated.as_object_mut() {
        let display = row_display_name(row);
        object
            .entry("name".to_owned())
            .or_insert_with(|| Value::String(display.name));
        if display.unnamed {
            object.insert("unnamed".to_owned(), Value::Bool(true));
        }
    }
    updated
}

pub fn exit_code_of(code: RefusalCode) -> i32 {
    match code {
        RefusalCode::DeviceOffline | RefusalCode::HostUnavailable => RemoteExit::Unavailable.code(),
        _ => RemoteExit::Failed.code(),
    }
}

pub fn refusal_message(refusal: &AddressRefusal) -> String {
    let typed = refusal.typed.as_deref().unwrap_or_default();
    match refusal.code {
        RefusalCode::DeviceOffline => format!("device {typed} is offline"),
        RefusalCode::AmbiguousTarget => format!("{typed} names more than one device"),
        RefusalCode::NotFound => format!("no device {typed} on this account"),
        _ => format!("the device part does not name one device of this account ({typed})"),
    }
}

pub fn refusal_details_json(refusal: &AddressRefusal, typed_device_miss: Option<&str>) -> Value {
    let mut value = serde_json::to_value(&refusal.details).unwrap_or(Value::Null);
    if let (Some(typed), Some(map)) = (typed_device_miss, value.as_object_mut())
        && refusal.reason == "no_device"
    {
        map.insert("id".to_owned(), Value::String(typed.to_owned()));
    }
    value
}

/// Candidate lines for an ambiguous refusal; empty for `device_offline` since an offline device is not a choice.
pub fn candidate_lines(code: RefusalCode, details: &RefusalDetails) -> Vec<String> {
    if code == RefusalCode::DeviceOffline {
        return Vec::new();
    }
    let RefusalDetails::Device { candidates, .. } = details else {
        return Vec::new();
    };
    candidates
        .iter()
        .map(|candidate| {
            let line = if candidate.name.is_empty() || candidate.name == candidate.device {
                format!("  {}", candidate.device)
            } else {
                format!("  {}  {}", candidate.device, candidate.name)
            };
            crate::render::escape_for_display(&line)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(device: &str, label: &str, online: bool, this_device: bool) -> Value {
        json!({ "device": device, "label": label, "online": online, "this_device": this_device })
    }

    fn account() -> Vec<Value> {
        vec![
            row("a11ce0de5e1f0123", "this mac", true, true),
            row("d15c0de012345678", "Studio Mac", true, false),
            row("b7c41e09d2a64f1e", "Studio Laptop", true, false),
            row("b7c4ffff00000000", "Studio Laptop", true, false),
            row("c0ffee5a1d2b3c4d", "attic", false, false),
        ]
    }

    #[test]
    fn every_typed_form_resolves_the_device_and_here_names_this_one() {
        let rows = account();
        for typed in ["d15c0de012345678", "Ｓｔｕｄｉｏ  ｍａｃ", "d15c0d"] {
            let device = typed_device(&rows, typed).expect(typed);
            assert_eq!(device.id, "d15c0de012345678", "{typed}");
            assert!(!device.here, "{typed}");
        }
        assert_eq!(
            typed_device(&rows, "b7c41e").expect("unique 6+ prefix").id,
            "b7c41e09d2a64f1e"
        );
        let here = typed_device(&rows, "here").expect("here");
        assert!(here.here);
        assert_eq!(here.id, "a11ce0de5e1f0123");
    }

    #[test]
    fn an_offline_device_is_refused_by_every_form_with_no_candidate_lines() {
        let rows = account();
        for typed in ["attic", "c0ffee5a1d2b3c4d"] {
            let refusal = typed_device(&rows, typed).expect_err(typed);
            assert_eq!(refusal.code, RefusalCode::DeviceOffline, "{typed}");
            assert_eq!(exit_code_of(refusal.code), 3);
            assert!(candidate_lines(refusal.code, &refusal.details).is_empty());
        }
    }

    #[test]
    fn two_devices_sharing_a_name_are_ambiguous_with_their_ids_as_candidates() {
        let rows = account();
        let refusal = typed_device(&rows, "Studio Laptop").expect_err("shared name");
        assert_eq!(refusal.code, RefusalCode::AmbiguousTarget);
        let lines = candidate_lines(refusal.code, &refusal.details);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.iter().any(|line| line.contains("b7c41e09d2a64f1e")));
    }

    #[test]
    fn device_filter_resolves_here_and_refuses_ambiguous_names_while_passing_unknown_ids() {
        let rows = account();
        assert_eq!(
            device_filter_target(&rows, "here").expect("here"),
            "a11ce0de5e1f0123"
        );
        assert_eq!(
            device_filter_target(&rows, "attic").expect("offline allowed in filter"),
            "c0ffee5a1d2b3c4d"
        );
        assert_eq!(
            device_filter_target(&rows, "Studio Laptop")
                .expect_err("ambiguous")
                .code,
            RefusalCode::AmbiguousTarget
        );
        assert_eq!(
            device_filter_target(&rows, "unlisted-id").expect("unknown passes through"),
            "unlisted-id"
        );
    }

    #[test]
    fn unknown_device_checks_roster_and_listing_round() {
        let rows: Vec<Value> = Vec::new();
        let listed = vec![json!({ "device": "b7c41e09d2a64f1e", "session_id": "s-1" })];
        assert!(unknown_device("b7c41e09d2a64f1e", &rows, &listed, &[], &Value::Null).is_none());
        assert!(
            unknown_device(
                "b7c41e09d2a64f1e",
                &rows,
                &[],
                &[],
                &json!({ "b7c41e09d2a64f1e": "undelivered" })
            )
            .is_none()
        );
        assert!(
            unknown_device(
                "b7c41e09d2a64f1e",
                &rows,
                &[],
                &[json!("b7c41e09d2a64f1e")],
                &Value::Null
            )
            .is_none()
        );
        let miss = unknown_device("here", &rows, &[], &[], &Value::Null).expect("here not listed");
        assert_eq!(miss.code, RefusalCode::NotFound);
    }

    #[test]
    fn unanswered_devices_dedups_includes_non_answered_states_and_skips_offline() {
        let rows = account();
        let unanswered = unanswered_devices(
            &rows,
            &[json!("b7c41e09d2a64f1e"), json!("c0ffee5a1d2b3c4d")],
            &json!({
                "b7c41e09d2a64f1e": "undelivered",
                "b7c4ffff00000000": "timed_out",
                "a11ce0de5e1f0123": "answered",
                "c0ffee5a1d2b3c4d": "offline",
            }),
        );
        assert_eq!(
            unanswered,
            vec!["b7c41e09d2a64f1e".to_owned(), "b7c4ffff00000000".to_owned()]
        );
    }

    #[test]
    fn unnamed_row_uses_folder_fallback_and_row_identity_uses_key_when_no_durable_id() {
        let anon = json!({
            "device": "b7c41e09d2a64f1e",
            "session_id": "rk-anon",
            "title": "",
            "project": "fallback",
            "path": "/home/user/proj",
        });
        assert_eq!(row_identity_key(&anon), "b7c41e09d2a64f1e/rk-anon");
        let enriched = enrich_listing_row(&anon);
        assert_eq!(enriched["name"], "proj");
        assert_eq!(enriched["unnamed"], true);

        let named = json!({
            "device": "b7c41e09d2a64f1e",
            "session_id": "rk-1",
            "durable_id": "dur-1",
            "title": "Fix bug",
            "project": "proj",
        });
        assert_eq!(row_identity_key(&named), "dur-1");
        let enriched_named = enrich_listing_row(&named);
        assert_eq!(enriched_named["name"], "Fix bug");
        assert!(enriched_named.get("unnamed").is_none());
    }
}
