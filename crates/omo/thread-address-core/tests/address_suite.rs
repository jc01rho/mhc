//! Upstream counterpart: `parse.test.ts`, `row.test.ts`, `device.test.ts`.

use std::sync::Arc;

use maho_thread_address_core::{
    DeviceListing, DeviceResolution, DeviceRoster, DeviceRow, KeyedRow, MergeRow, NamedRow,
    ParsedAddress, RefusalCode, RefusalDetails, ResolveDeviceOptions, SessionRef, format_address,
    id_shaped, is_id_prefix, listing_key, merge_rows, normalize_name, parse_address,
    resolve_device, row_address, row_key, row_name, same_session,
};

fn bare(text: &str) -> ParsedAddress {
    ParsedAddress::Bare {
        text: text.to_owned(),
    }
}

#[test]
fn parse_address_names_self_a_bare_address_or_a_device_and_a_thread() {
    let device = |device: &str, thread: &str, text: &str| ParsedAddress::Device {
        device: device.to_owned(),
        thread: thread.to_owned(),
        text: text.to_owned(),
    };
    let cases = [
        ("self", ParsedAddress::SelfThread),
        (
            "0aa00001-0000-4000-8000-000000000001",
            bare("0aa00001-0000-4000-8000-000000000001"),
        ),
        ("  my thread  ", bare("my thread")),
        ("studio/beta", device("studio", "beta", "studio/beta")),
        ("here/rpc-3", device("here", "rpc-3", "here/rpc-3")),
        ("a/b/c", bare("a/b/c")),
        ("/b", bare("/b")),
        ("a/", bare("a/")),
        ("./x", bare("./x")),
        ("~/x", bare("~/x")),
        ("C:/x", bare("C:/x")),
        ("dir/x.jsonl", bare("dir/x.jsonl")),
    ];
    for (text, expected) in cases {
        assert_eq!(parse_address(text), expected, "{text}");
    }
}

#[test]
fn a_formatted_address_reads_back_as_its_two_parts() {
    let address = format_address("b7c41e09d2a64f1e", "0bb00001-0000-4000-8000-000000000001");
    assert_eq!(
        parse_address(&address),
        ParsedAddress::Device {
            device: "b7c41e09d2a64f1e".to_owned(),
            thread: "0bb00001-0000-4000-8000-000000000001".to_owned(),
            text: address.clone(),
        }
    );
}

#[test]
fn only_a_thread_id_or_a_session_id_is_id_shaped() {
    let cases = [
        ("0bb00001-0000-4000-8000-000000000001", true),
        ("AbCdEfGhIjKlMnOpQrStUv", true),
        ("rpc-7", true),
        ("beta old", false),
        ("release-planner-2", false),
        ("nothing-named-so", false),
        ("beta", false),
    ];
    for (text, expected) in cases {
        assert_eq!(id_shaped(text), expected, "{text}");
    }
}

#[test]
fn an_id_prefix_needs_eight_hex_characters_before_any_hyphen() {
    assert!(is_id_prefix("0bb00001"));
    assert!(is_id_prefix("0BB00001-00"));
    assert!(!is_id_prefix("0bb0000"));
    assert!(!is_id_prefix("0bb0-0001"));
    assert!(!is_id_prefix("release-planner"));
}

#[test]
fn names_compare_after_nfkc_case_and_whitespace_folding() {
    assert_eq!(normalize_name("  Studio\t  Laptop "), "studio laptop");
    assert_eq!(normalize_name("ｓｔｕｄｉｏ ｌａｐｔｏｐ"), "studio laptop");
    assert_eq!(
        normalize_name("caf\u{0065}\u{0301}"),
        normalize_name("caf\u{00e9}")
    );
}

fn keyed(device: &str, thread: Option<&str>, key: &str) -> KeyedRow {
    KeyedRow {
        device: device.to_owned(),
        durable_id: thread.map(str::to_owned),
        session_id: key.to_owned(),
    }
}

#[test]
fn a_row_with_a_thread_id_is_keyed_by_it_and_addressed_with_its_device() {
    let listed = keyed(
        "b7c41e09d2a64f1e",
        Some("0bb00001-0000-4000-8000-000000000001"),
        "rk-b",
    );
    assert_eq!(row_key(&listed), "0bb00001-0000-4000-8000-000000000001");
    assert_eq!(
        row_address(&listed),
        "b7c41e09d2a64f1e/0bb00001-0000-4000-8000-000000000001"
    );
    let unpublished = keyed("b7c41e09d2a64f1e", None, "rk-b");
    assert_eq!(row_key(&unpublished), "b7c41e09d2a64f1e/rk-b");
    assert_eq!(row_address(&unpublished), "b7c41e09d2a64f1e/rk-b");
}

#[test]
fn only_a_real_name_is_an_address() {
    let named = |name: Option<&str>, first_message: Option<&str>| NamedRow {
        name: name.map(str::to_owned),
        first_message: first_message.map(str::to_owned),
        folder: "/work/b".to_owned(),
    };
    let real = row_name(&named(Some("beta"), Some("hello")));
    assert_eq!((real.name.as_str(), real.unnamed), ("beta", false));
    let message = row_name(&named(Some(""), Some("fix the login page")));
    assert_eq!(
        (message.name.as_str(), message.unnamed),
        ("fix the login page", true)
    );
    let folder = row_name(&named(None, None));
    assert_eq!((folder.name.as_str(), folder.unnamed), ("b", true));
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct At {
    key: String,
    device: String,
}

impl MergeRow for At {
    fn key(&self) -> &str {
        &self.key
    }
    fn device(&self) -> &str {
        &self.device
    }
}

fn at(device: &str, key: &str) -> At {
    At {
        key: key.to_owned(),
        device: device.to_owned(),
    }
}

#[test]
fn merging_keeps_one_row_per_thread() {
    assert_eq!(
        merge_rows(
            &[at("here", "t1")],
            &[at("here", "t1"), at("b", "t2")],
            Some("here")
        ),
        vec![at("here", "t1"), at("b", "t2")]
    );
    assert_eq!(
        merge_rows(&[at("here", "t1")], &[at("b", "t1")], Some("here")),
        vec![at("here", "t1")]
    );
    assert_eq!(
        merge_rows(
            &[at("here", "t1"), at("here", "t2")],
            &[at("b", "t3"), at("c", "t4")],
            Some("here")
        ),
        vec![
            at("here", "t1"),
            at("here", "t2"),
            at("b", "t3"),
            at("c", "t4")
        ]
    );
}

fn session(device: &str, thread: Option<&str>, key: Option<&str>) -> SessionRef {
    SessionRef {
        device: device.to_owned(),
        thread: thread.map(str::to_owned),
        key: key.map(str::to_owned),
    }
}

#[test]
fn two_records_name_one_session_by_device_then_id_then_listing_key() {
    let cases = [
        (
            session("b", Some("t1"), Some("k1")),
            session("b", Some("t1"), Some("k2")),
            true,
        ),
        (
            session("b", Some("t1"), Some("k1")),
            session("b", Some("t2"), Some("k1")),
            false,
        ),
        (
            session("b", Some("t1"), Some("k1")),
            session("b", None, Some("k1")),
            true,
        ),
        (
            session("b", Some("t1"), Some("k1")),
            session("b", None, Some("k2")),
            false,
        ),
        (
            session("b", None, Some("k1")),
            session("b", None, Some("k1")),
            true,
        ),
        (session("b", None, None), session("b", None, None), false),
        (
            session("b", Some("t1"), None),
            session("c", Some("t1"), None),
            false,
        ),
    ];
    for (a, b, expected) in cases {
        assert_eq!(same_session(&a, &b), expected, "{a:?} {b:?}");
        assert_eq!(same_session(&b, &a), expected, "{b:?} {a:?}");
    }
    assert_ne!(listing_key("b", "rk-1"), listing_key("c", "rk-1"));
    assert_eq!(listing_key("b", "rk-1"), "b/rk-1");
}

fn device(id: &str, name: &str, online: bool, last_seen_ms: Option<i64>) -> DeviceRow {
    DeviceRow {
        id: id.to_owned(),
        name: name.to_owned(),
        online,
        last_seen_ms,
    }
}

fn account() -> DeviceRoster {
    DeviceRoster {
        here: Some("here".to_owned()),
        devices: vec![
            device("here", "this mac", true, None),
            device("b7c41e09d2a64f1e", "Studio Laptop", true, None),
            device("b7c4aaaa00000000", "studio", false, Some(5)),
            device("c0ffee5a1d2b3c4d", "attic", false, Some(7)),
            device("d00d000000000001", "twin", false, None),
            device("d00d000000000002", "Twin", true, None),
        ],
        listing_of: None,
    }
}

fn options(online: bool) -> ResolveDeviceOptions {
    ResolveDeviceOptions {
        online,
        exact_id: false,
    }
}

fn resolved_id(roster: &DeviceRoster, text: &str, online: bool) -> String {
    match resolve_device(roster, text, options(online)) {
        DeviceResolution::Ok(device) => device.id,
        DeviceResolution::Refusal(refusal) => refusal.code.as_str().to_owned(),
    }
}

#[test]
fn a_device_part_resolves_by_exact_id_then_here_then_name_then_unique_prefix() {
    let roster = account();
    let cases = [
        ("b7c41e09d2a64f1e", "b7c41e09d2a64f1e"),
        ("here", "here"),
        ("THIS  mac", "here"),
        ("ｓｔｕｄｉｏ ｌａｐｔｏｐ", "b7c41e09d2a64f1e"),
        ("twin", "d00d000000000002"),
        ("c0ffee", "c0ffee5a1d2b3c4d"),
        ("c0ffe", "not_found"),
        ("attic", "c0ffee5a1d2b3c4d"),
        ("nowhere", "not_found"),
    ];
    for (text, expected) in cases {
        assert_eq!(resolved_id(&roster, text, false), expected, "{text}");
    }
}

fn refusal_of(resolution: DeviceResolution) -> maho_thread_address_core::AddressRefusal {
    match resolution {
        DeviceResolution::Refusal(refusal) => refusal,
        DeviceResolution::Ok(device) => panic!("resolved {device:?}"),
    }
}

#[test]
fn two_devices_sharing_a_prefix_are_ambiguous() {
    let refusal = refusal_of(resolve_device(&account(), "d00d00", options(false)));
    assert_eq!(refusal.code, RefusalCode::AmbiguousTarget);
    assert!(matches!(
        refusal.details,
        RefusalDetails::Device { kind: "device", .. }
    ));
}

#[test]
fn an_offline_device_is_refused_only_when_the_caller_needs_it_online() {
    let roster = account();
    let refusal = refusal_of(resolve_device(&roster, "c0ffee5a1d2b3c4d", options(true)));
    assert_eq!(refusal.code, RefusalCode::DeviceOffline);
    assert_eq!(
        serde_json::to_value(&refusal.details).unwrap_or_default(),
        serde_json::json!({
            "device": "c0ffee5a1d2b3c4d",
            "device_name": "attic",
            "last_seen": 7,
            "candidates": [{ "device": "c0ffee5a1d2b3c4d", "name": "attic", "online": false }],
        })
    );
    for text in ["c0ffee5a1d2b3c4d", "ATTIC", "c0ffee5a"] {
        assert_eq!(resolved_id(&roster, text, false), "c0ffee5a1d2b3c4d");
        assert_eq!(resolved_id(&roster, text, true), "device_offline");
    }
}

#[test]
fn a_resolved_device_says_whether_it_is_here_and_how_this_round_listed_it() {
    let mut roster = account();
    roster.listing_of = Some(Arc::new(|_: &str| DeviceListing::Incomplete));
    match resolve_device(&roster, "studio laptop", options(false)) {
        DeviceResolution::Ok(device) => {
            assert_eq!(device.id, "b7c41e09d2a64f1e");
            assert_eq!(device.name, "Studio Laptop");
            assert!(device.online);
            assert!(!device.here);
            assert_eq!(device.listing, DeviceListing::Incomplete);
        }
        DeviceResolution::Refusal(refusal) => panic!("refused {refusal:?}"),
    }
}

#[test]
fn here_offline_in_the_roster_is_refused_only_when_needed_online() {
    let roster = DeviceRoster {
        here: Some("here".to_owned()),
        devices: vec![device("here", "this mac", false, None)],
        listing_of: None,
    };
    assert_eq!(resolved_id(&roster, "here", false), "here");
    assert_eq!(resolved_id(&roster, "here", true), "device_offline");
}

#[test]
fn two_offline_devices_sharing_a_name_are_ambiguous() {
    let roster = DeviceRoster {
        here: None,
        devices: vec![
            device("e0000000000001", "box", false, None),
            device("e0000000000002", "box", false, None),
        ],
        listing_of: None,
    };
    assert_eq!(resolved_id(&roster, "box", false), "ambiguous_target");
}

#[test]
fn here_without_a_device_id_for_this_device_is_not_found() {
    let mut roster = account();
    roster.here = None;
    roster.devices.retain(|row| row.id != "here");
    for online in [false, true] {
        let refusal = refusal_of(resolve_device(&roster, "here", options(online)));
        assert_eq!(refusal.code, RefusalCode::NotFound);
        assert_eq!(refusal.reason, "no_device");
    }
}

#[test]
fn an_exact_id_resolves_only_that_id() {
    let roster = account();
    let exact = ResolveDeviceOptions {
        online: true,
        exact_id: true,
    };
    for text in ["b7c41e", "Studio Laptop", "this mac"] {
        let refusal = refusal_of(resolve_device(&roster, text, exact));
        assert_eq!(refusal.code, RefusalCode::NotFound, "{text}");
        assert_eq!(refusal.reason, "no_device", "{text}");
    }
    assert!(matches!(
        resolve_device(&roster, "b7c41e09d2a64f1e", exact),
        DeviceResolution::Ok(_)
    ));
    assert_eq!(
        refusal_of(resolve_device(&roster, "c0ffee5a1d2b3c4d", exact)).code,
        RefusalCode::DeviceOffline
    );
}
