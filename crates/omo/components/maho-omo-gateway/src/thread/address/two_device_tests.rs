//! `resolve_address` over two devices of one account (omo `address/resolve.test.ts`, `resolve-refusals.test.ts`;
//! #14305, #14318, #14328, #14386): every input form, the refusal table, the probes and the reads one resolution makes.

use futures::executor::block_on;
use serde_json::{Value, json};

use super::refusals::ThreadRefusal;
use super::resolve::resolve_address;
use super::two_devices::{
    ALIAS_B, ALIAS_HERE, B_ENDED, B_ID, B_LIVE, B_NAME, B_PROBE, B_SHARED, B_TWIN_ID, FIX_ID,
    FIX_LOGIN, FIX_NAME, HERE_ENDED, HERE_ID, HERE_LIVE, HERE_NAME, HERE_SHARED, HERE_SLASH,
    KEY_B_LIVE, NOWHERE, OFF_ID, OFF_NAME, OFF_SEEN, QUIET_ID, ROUTE_HERE_LIVE, STALE, Setup,
    TwoDevices, WORKSPACE, device, row,
};
use super::types::{Pin, Resolution, ResolveContext, ResolveOptions, Resolved, ResolvedWhere};
use crate::thread::addressing::ThreadAddressEntry;
use crate::thread::contracts_results::ThreadStatus;
use crate::thread::remote::ports::ClientDeviceRow;
use crate::thread::store_extension_session_ops::UNKNOWN_CALLER;

const KEY22: &str = "AbCdEfGhIjKlMnOpQrStUv";
const KEYLESS: &str = "c2Vzc2lvbi1rZXktMDAwMQ";
const C_ID: &str = "c3d4e5f60718293a";

fn plain() -> ResolveOptions<'static> {
    ResolveOptions::default()
}

fn probing() -> ResolveOptions<'static> {
    ResolveOptions {
        probe: true,
        ..ResolveOptions::default()
    }
}

fn online() -> ResolveOptions<'static> {
    ResolveOptions {
        need_online: true,
        ..ResolveOptions::default()
    }
}

fn unique() -> ResolveOptions<'static> {
    ResolveOptions {
        unique_holder: true,
        id_prefix: false,
        ..ResolveOptions::default()
    }
}

fn with_probe() -> Setup {
    Setup {
        probe: true,
        ..Setup::default()
    }
}

fn hit(result: Result<Resolved, ThreadRefusal>, text: &str) -> Resolved {
    match result {
        Ok(resolved) => resolved,
        Err(refused) => panic!("{text}: {} {}", refused.code, refused.message),
    }
}

fn refused(result: Result<Resolved, ThreadRefusal>, text: &str) -> ThreadRefusal {
    match result {
        Ok(resolved) => panic!("{text}: expected a refusal, resolved {}", resolved.thread),
        Err(refusal) => refusal,
    }
}

fn detail<'a>(refusal: &'a ThreadRefusal, key: &str) -> Option<&'a Value> {
    refusal
        .details
        .as_ref()
        .and_then(|details| details.get(key))
}

fn text_detail<'a>(refusal: &'a ThreadRefusal, key: &str) -> Option<&'a str> {
    detail(refusal, key).and_then(Value::as_str)
}

fn code(refusal: &ThreadRefusal) -> &str {
    refusal.code.as_str()
}

fn where_device(resolved: &Resolved) -> Option<&str> {
    match &resolved.where_ {
        ResolvedWhere::Here { .. } => None,
        ResolvedWhere::Device { row } => Some(row.device.as_str()),
    }
}

fn candidates(refusal: &ThreadRefusal) -> Vec<Value> {
    detail(refusal, "candidates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn field(candidate: &Value, key: &str) -> String {
    candidate[key].as_str().unwrap_or_default().to_owned()
}

struct Form {
    text: String,
    thread: &'static str,
    on: Option<&'static str>,
    resolution: Resolution,
    device: Option<&'static str>,
}

fn form(
    text: impl Into<String>,
    thread: &'static str,
    on: Option<&'static str>,
    resolution: Resolution,
    device: Option<&'static str>,
) -> Form {
    Form {
        text: text.into(),
        thread,
        on,
        resolution,
        device,
    }
}

#[test]
fn every_input_form_names_the_same_thread_on_this_device_or_on_b() {
    let forms = vec![
        form(HERE_LIVE, HERE_LIVE, None, Resolution::Id, None),
        form(B_LIVE, B_LIVE, Some(B_ID), Resolution::Id, None),
        form(
            ROUTE_HERE_LIVE,
            HERE_LIVE,
            None,
            Resolution::SessionId,
            None,
        ),
        form(KEY_B_LIVE, B_LIVE, Some(B_ID), Resolution::SessionId, None),
        form(ALIAS_HERE, HERE_LIVE, None, Resolution::Alias, None),
        form(ALIAS_B, B_LIVE, Some(B_ID), Resolution::Alias, None),
        form("alpha", HERE_LIVE, None, Resolution::Name, None),
        form("beta", B_LIVE, Some(B_ID), Resolution::Name, None),
        form(&HERE_LIVE[..8], HERE_LIVE, None, Resolution::IdPrefix, None),
        form(&B_LIVE[..8], B_LIVE, Some(B_ID), Resolution::IdPrefix, None),
        form("self", HERE_LIVE, None, Resolution::SelfThread, None),
        form(
            format!("{B_ID}/{B_LIVE}"),
            B_LIVE,
            Some(B_ID),
            Resolution::Id,
            Some(B_ID),
        ),
        form(
            "studio   LAPTOP/beta",
            B_LIVE,
            Some(B_ID),
            Resolution::Name,
            Some(B_ID),
        ),
        form(
            format!("{}/{}", &B_ID[..6], &B_LIVE[..8]),
            B_LIVE,
            Some(B_ID),
            Resolution::IdPrefix,
            Some(B_ID),
        ),
        form(
            format!("{B_ID}/{KEY_B_LIVE}"),
            B_LIVE,
            Some(B_ID),
            Resolution::SessionId,
            Some(B_ID),
        ),
        form(
            format!("{B_NAME}/{ALIAS_B}"),
            B_LIVE,
            Some(B_ID),
            Resolution::Alias,
            Some(B_ID),
        ),
        form(
            format!("here/{HERE_LIVE}"),
            HERE_LIVE,
            None,
            Resolution::Id,
            Some(HERE_ID),
        ),
        form(
            format!("{HERE_NAME}/alpha"),
            HERE_LIVE,
            None,
            Resolution::Name,
            Some(HERE_ID),
        ),
        form(
            format!("here/{ROUTE_HERE_LIVE}"),
            HERE_LIVE,
            None,
            Resolution::SessionId,
            Some(HERE_ID),
        ),
    ];
    for item in forms {
        let two = TwoDevices::new(Setup::default());
        let resolved = hit(two.resolve(&item.text, &plain()), &item.text);
        assert_eq!(resolved.thread, item.thread, "{}", item.text);
        assert_eq!(where_device(&resolved), item.on, "{}", item.text);
        assert_eq!(resolved.resolution, item.resolution, "{}", item.text);
        if let Some(expected) = item.device {
            let device = resolved.device.as_ref().map(|device| device.id.as_str());
            assert_eq!(device, Some(expected), "{}", item.text);
        }
        if let Some(device) = &resolved.device {
            assert_eq!(
                resolved.address,
                format!("{}/{}", device.id, resolved.thread),
                "{}",
                item.text
            );
        }
    }
}

#[test]
fn an_id_b_holds_but_does_not_list_is_asked_of_b_itself() {
    let two = TwoDevices::new(with_probe());
    let text = format!("{B_ID}/{B_PROBE}");
    let resolved = hit(two.resolve(&text, &plain()), &text);
    assert_eq!(resolved.thread, B_PROBE);
    assert!(matches!(
        &resolved.where_,
        ResolvedWhere::Device { row } if row.device == B_ID && row.durable_id.as_deref() == Some(B_PROBE)
    ));
}

#[test]
fn an_ended_thread_on_b_resolves_to_its_ended_row() {
    let two = TwoDevices::new(Setup::default());
    let resolved = hit(two.resolve(B_ENDED, &plain()), B_ENDED);
    assert!(matches!(
        &resolved.where_,
        ResolvedWhere::Device { row } if row.state == "ended"
    ));
}

#[test]
fn an_address_a_resolution_returns_resolves_as_written_to_the_same_thread() {
    let two = TwoDevices::new(Setup::default());
    let first = hit(two.resolve("beta", &plain()), "beta");
    let again = hit(two.resolve(&first.address, &plain()), &first.address);
    assert_eq!(again.thread, B_LIVE);
    let device = again.device.expect("a device");
    assert_eq!((device.id.as_str(), device.name.as_str()), (B_ID, B_NAME));
}

#[test]
fn a_thread_name_with_a_slash_takes_the_device_reading_only_when_its_first_part_is_a_device() {
    let two = TwoDevices::new(Setup::default());
    let resolved = hit(two.resolve("fix/login", &plain()), "fix/login");
    assert_eq!(resolved.thread, HERE_SLASH);
    assert_eq!(resolved.resolution, Resolution::Name);
    assert_eq!(where_device(&resolved), None);

    let with_fix = TwoDevices::new(Setup {
        fix: true,
        ..Setup::default()
    });
    let ambiguous = refused(with_fix.resolve("fix/login", &plain()), "fix/login");
    assert_eq!(code(&ambiguous), "ambiguous_target");
    let mut threads: Vec<String> = candidates(&ambiguous)
        .iter()
        .map(|candidate| field(candidate, "thread"))
        .collect();
    threads.sort();
    let mut expected = vec![FIX_LOGIN.to_owned(), HERE_SLASH.to_owned()];
    expected.sort();
    assert_eq!(threads, expected);

    let missed = refused(
        with_fix.resolve(&format!("{FIX_NAME}/nothing-named-so"), &plain()),
        "fix/nothing",
    );
    assert_eq!(code(&missed), "not_found");
    assert_eq!(text_detail(&missed, "kind"), Some("thread"));
    assert_eq!(text_detail(&missed, "device"), Some(FIX_ID));
    assert_eq!(text_detail(&missed, "device_name"), Some(FIX_NAME));
}

#[test]
fn a_name_two_devices_share_is_ambiguous_with_one_pasteable_candidate_per_device() {
    let two = TwoDevices::new(Setup::default());
    let ambiguous = refused(two.resolve("shared", &plain()), "shared");
    assert_eq!(code(&ambiguous), "ambiguous_target");
    assert_eq!(text_detail(&ambiguous, "kind"), Some("thread"));
    let list = candidates(&ambiguous);
    let pairs: Vec<(String, String)> = list
        .iter()
        .map(|candidate| (field(candidate, "thread"), field(candidate, "device_name")))
        .collect();
    assert!(pairs.contains(&(HERE_SHARED.to_owned(), HERE_NAME.to_owned())));
    assert!(pairs.contains(&(B_SHARED.to_owned(), B_NAME.to_owned())));
    for candidate in &list {
        let address = field(candidate, "address");
        assert_eq!(
            address,
            format!(
                "{}/{}",
                field(candidate, "device"),
                field(candidate, "thread")
            )
        );
        let again = hit(two.resolve(&address, &plain()), &address);
        assert_eq!(again.thread, field(candidate, "thread"));
    }
    let on_b = format!("{B_NAME}/shared");
    let qualified = hit(two.resolve(&on_b, &plain()), &on_b);
    assert_eq!(qualified.thread, B_SHARED);
    assert_eq!(where_device(&qualified), Some(B_ID));
}

#[test]
fn a_closed_copy_here_answers_an_id_b_lists_live() {
    let two = TwoDevices::new(Setup::default());
    let stale = hit(two.resolve(STALE, &plain()), STALE);
    assert_eq!((stale.thread.as_str(), where_device(&stale)), (STALE, None));
    assert_eq!(stale.resolution, Resolution::Id);
    let ended = hit(two.resolve(HERE_ENDED, &plain()), HERE_ENDED);
    assert_eq!(ended.thread, HERE_ENDED);
}

#[test]
fn unique_holder_resolves_one_holder_and_refuses_several() {
    let two = TwoDevices::new(Setup::default());
    let one = hit(two.resolve(B_LIVE, &unique()), B_LIVE);
    assert_eq!(where_device(&one), Some(B_ID));

    let copy = refused(two.resolve(STALE, &unique()), STALE);
    assert_eq!(code(&copy), "ambiguous_target");
    let mut devices: Vec<String> = candidates(&copy)
        .iter()
        .map(|candidate| field(candidate, "device"))
        .collect();
    devices.sort();
    let mut expected = vec![B_ID.to_owned(), HERE_ID.to_owned()];
    expected.sort();
    assert_eq!(devices, expected);

    let named = format!("{B_ID}/{STALE}");
    let qualified = hit(two.resolve(&named, &unique()), &named);
    assert_eq!(where_device(&qualified), Some(B_ID));
}

#[test]
fn unique_holder_names_both_devices_that_list_an_id_whatever_the_listing_order() {
    let on = |device: &str, key: &str| {
        let mut listed = row(NOWHERE, key, "copy", "idle", device);
        listed.updated_ms = 1;
        listed
    };
    let orders = [
        vec![on(B_ID, "rk-b"), on(C_ID, "rk-c")],
        vec![on(C_ID, "rk-c"), on(B_ID, "rk-b")],
    ];
    for rows in orders {
        let two = TwoDevices::from_parts(
            Setup::default(),
            rows,
            vec![
                ClientDeviceRow {
                    is_self: true,
                    ..device(HERE_ID, HERE_NAME, true)
                },
                device(B_ID, B_NAME, true),
                device(C_ID, "garage", true),
            ],
            Vec::new(),
        );
        let ambiguous = refused(two.resolve(NOWHERE, &unique()), NOWHERE);
        assert_eq!(code(&ambiguous), "ambiguous_target");
        let mut addresses: Vec<String> = candidates(&ambiguous)
            .iter()
            .map(|candidate| field(candidate, "address"))
            .collect();
        addresses.sort();
        let mut expected = vec![format!("{B_ID}/{NOWHERE}"), format!("{C_ID}/{NOWHERE}")];
        expected.sort();
        assert_eq!(addresses, expected);
    }
}

#[test]
fn a_routing_id_or_listing_key_b_has_not_listed_passes_through_to_b_and_a_name_is_not_found() {
    for part in [KEY22, "rpc-7"] {
        let two = TwoDevices::new(Setup::default());
        let text = format!("{B_ID}/{part}");
        let resolved = hit(two.resolve(&text, &plain()), &text);
        assert_eq!(resolved.address, text);
        assert!(matches!(
            &resolved.where_,
            ResolvedWhere::Device { row } if row.device == B_ID && row.session_id == part
        ));
    }
    for part in ["no such thread", "release-planner-2"] {
        let two = TwoDevices::new(Setup::default());
        let text = format!("{B_NAME}/{part}");
        let missed = refused(two.resolve(&text, &plain()), &text);
        assert_eq!(code(&missed), "not_found");
        assert_eq!(text_detail(&missed, "kind"), Some("thread"));
        assert_eq!(text_detail(&missed, "device"), Some(B_ID));
    }
}

#[test]
fn this_devices_book_is_asked_for_the_thread_part_once_the_device_part_names_this_device() {
    let two = TwoDevices::new(Setup::default());
    let visible: Vec<ThreadAddressEntry> = two
        .entries
        .iter()
        .filter(|entry| entry.thread_id != HERE_ENDED)
        .cloned()
        .collect();
    let everything = two.entries.clone();
    let local_for = |part: &str| -> Vec<ThreadAddressEntry> {
        if part == HERE_ENDED {
            everything.clone()
        } else {
            visible.clone()
        }
    };
    let ctx = ResolveContext {
        remote: Some(&two.surface),
        caller_workspace_root: WORKSPACE,
        local: &visible,
        local_for: Some(&local_for),
        caller_id: HERE_LIVE,
    };
    for device in ["here", HERE_ID, HERE_NAME] {
        let text = format!("{device}/{HERE_ENDED}");
        let resolved = hit(block_on(resolve_address(&ctx, &text, &plain())), &text);
        assert_eq!(resolved.thread, HERE_ENDED, "{text}");
        assert_eq!(where_device(&resolved), None, "{text}");
    }
}

#[test]
fn a_device_qualified_id_reads_only_that_device_and_one_status_round() {
    for text in [
        format!("{B_ID}/{B_LIVE}"),
        format!("{B_NAME}/{B_LIVE}"),
        format!("{B_ID}/{KEY22}"),
        format!("{B_NAME}/beta"),
    ] {
        let two = TwoDevices::new(Setup::default());
        let resolved = hit(two.resolve(&text, &probing()), &text);
        assert_eq!(where_device(&resolved), Some(B_ID), "{text}");
        let counters = two.counters();
        assert_eq!((counters.unfiltered, counters.statuses), (0, 1), "{text}");
    }
}

#[test]
fn a_name_the_device_does_not_hold_is_read_account_wide_once_before_not_found() {
    let two = TwoDevices::new(Setup::default());
    let text = format!("{B_NAME}/no such thread");
    let missed = refused(two.resolve(&text, &probing()), &text);
    assert_eq!(code(&missed), "not_found");
    assert_eq!(two.counters().unfiltered, 1);
}

#[test]
fn a_here_qualified_name_reads_no_account_wide_listing() {
    for text in ["here/alpha".to_owned(), format!("{HERE_ID}/alpha")] {
        let two = TwoDevices::new(Setup::default());
        let resolved = hit(two.resolve(&text, &probing()), &text);
        assert_eq!(resolved.thread, HERE_LIVE);
        assert_eq!(where_device(&resolved), None);
        assert_eq!(two.counters().unfiltered, 0, "{text}");
    }
}

#[test]
fn an_id_b_does_not_hold_is_b_s_answer_with_no_whole_text_reading() {
    let two = TwoDevices::new(Setup::default());
    let text = format!("{B_ID}/{NOWHERE}");
    let missed = refused(two.resolve(&text, &probing()), &text);
    assert_eq!(text_detail(&missed, "device"), Some(B_ID));
    assert_eq!(two.counters().unfiltered, 0);
}

#[test]
fn a_thread_here_named_like_a_device_qualified_address_resolves_after_one_account_wide_read() {
    let mut two = TwoDevices::new(Setup::default());
    let named_id = "0aa00009-0000-4000-8000-000000000009";
    two.entries.push(ThreadAddressEntry {
        thread_id: named_id.to_owned(),
        name: format!("{B_NAME}/notes"),
        cwd: WORKSPACE.to_owned(),
        status: ThreadStatus::Live,
        updated_at: None,
        session_ids: Vec::new(),
        device: None,
        aliases: Vec::new(),
        row: None,
    });
    let text = format!("{B_NAME}/notes");
    let resolved = hit(two.resolve(&text, &probing()), &text);
    assert_eq!(resolved.thread, named_id);
    assert_eq!(where_device(&resolved), None);
    assert_eq!(two.counters().unfiltered, 1);
}

#[test]
fn a_device_named_fix_and_a_thread_here_named_fix_slash_login_are_ambiguous_without_an_account_wide_read()
 {
    let two = TwoDevices::new(Setup {
        fix: true,
        ..Setup::default()
    });
    let ambiguous = refused(two.resolve("fix/login", &probing()), "fix/login");
    assert_eq!(code(&ambiguous), "ambiguous_target");
    assert_eq!(two.counters().unfiltered, 0);
}

#[test]
fn an_id_b_holds_only_on_disk_reads_b_s_listing_and_the_roster_once() {
    let two = TwoDevices::new(with_probe());
    let text = format!("{B_ID}/{B_PROBE}");
    let resolved = hit(two.resolve(&text, &probing()), &text);
    assert_eq!(resolved.thread, B_PROBE);
    let counters = two.counters();
    assert_eq!((counters.filtered, counters.rosters), (1, 1));
}

#[test]
fn an_id_nobody_holds_reads_b_s_listing_once_and_again_once_and_the_roster_once() {
    let two = TwoDevices::new(with_probe());
    let text = format!("{B_ID}/{NOWHERE}");
    let missed = refused(two.resolve(&text, &probing()), &text);
    assert_eq!(code(&missed), "not_found");
    let counters = two.counters();
    assert_eq!((counters.filtered, counters.rosters), (2, 1));
}

#[test]
fn a_bare_id_on_b_reads_the_account_listing_at_most_once_and_the_status_once() {
    let two = TwoDevices::new(Setup::default());
    let resolved = hit(two.resolve(B_LIVE, &probing()), B_LIVE);
    assert_eq!(resolved.thread, B_LIVE);
    let counters = two.counters();
    assert!(counters.unfiltered <= 1);
    assert_eq!(counters.statuses, 1);
}

#[test]
fn a_listing_row_that_publishes_no_thread_id_resolves_by_its_listing_key() {
    let keyless = row("", KEYLESS, "keyless", "idle", B_ID);
    let keyless = {
        let mut listed = keyless;
        listed.durable_id = None;
        listed.updated_ms = 1;
        listed
    };
    for text in [
        format!("{B_ID}/{KEYLESS}"),
        format!("{B_NAME}/{KEYLESS}"),
        KEYLESS.to_owned(),
    ] {
        let two = TwoDevices::from_parts(
            Setup::default(),
            vec![keyless.clone()],
            vec![
                ClientDeviceRow {
                    is_self: true,
                    ..device(HERE_ID, HERE_NAME, true)
                },
                device(B_ID, B_NAME, true),
            ],
            Vec::new(),
        );
        let resolved = hit(two.resolve(&text, &plain()), &text);
        assert_eq!(resolved.address, format!("{B_ID}/{KEYLESS}"), "{text}");
        assert!(
            matches!(
                &resolved.where_,
                ResolvedWhere::Device { row } if row.device == B_ID && row.title == "keyless"
            ),
            "{text}"
        );
    }
}

#[test]
fn the_refusal_table_answers_each_condition_with_its_code_and_details() {
    let cases = vec![
        (
            "empty text",
            String::new(),
            plain(),
            Setup::default(),
            "invalid_arguments",
            None,
        ),
        (
            "a control character",
            "a\u{1}b".to_owned(),
            plain(),
            Setup::default(),
            "invalid_arguments",
            None,
        ),
        (
            "a device part that matches no device",
            "nowhere-device/beta".to_owned(),
            plain(),
            Setup::default(),
            "not_found",
            Some(("kind", json!("device"))),
        ),
        (
            "a device name two online devices carry",
            format!("{B_NAME}/beta"),
            plain(),
            Setup {
                twin: true,
                ..Setup::default()
            },
            "ambiguous_target",
            Some(("kind", json!("device"))),
        ),
        (
            "an offline device when the caller needs it online",
            format!("{OFF_NAME}/{NOWHERE}"),
            online(),
            Setup::default(),
            "device_offline",
            Some(("last_seen", json!(OFF_SEEN))),
        ),
        (
            "a thread on B while B is offline and the caller needs it online",
            B_LIVE.to_owned(),
            online(),
            Setup {
                b_offline: true,
                ..Setup::default()
            },
            "device_offline",
            Some(("device_name", json!(B_NAME))),
        ),
        (
            "a relay configured but not connected",
            "no thread is named so".to_owned(),
            plain(),
            Setup {
                plan: Some("offline".to_owned()),
                ..Setup::default()
            },
            "host_unavailable",
            Some(("state", json!("offline"))),
        ),
        (
            "a name on B while B misses the listing round",
            "beta".to_owned(),
            plain(),
            Setup {
                incomplete: true,
                ..Setup::default()
            },
            "host_unavailable",
            Some(("devices", json!([B_ID]))),
        ),
        (
            "a device-qualified name on B while B misses the listing round",
            format!("{B_ID}/beta"),
            plain(),
            Setup {
                incomplete: true,
                ..Setup::default()
            },
            "host_unavailable",
            Some(("reason", json!("listing_incomplete"))),
        ),
        (
            "a name B does not hold, B answered",
            format!("{B_NAME}/no such thread"),
            plain(),
            Setup::default(),
            "not_found",
            Some(("device_name", json!(B_NAME))),
        ),
        (
            "nothing matches and every device answered",
            "no thread is named so".to_owned(),
            plain(),
            Setup::default(),
            "not_found",
            Some(("kind", json!("thread"))),
        ),
        (
            "a name two threads carry",
            "shared".to_owned(),
            plain(),
            Setup::default(),
            "ambiguous_target",
            Some(("kind", json!("thread"))),
        ),
        (
            "no relay set up",
            "no thread is named so".to_owned(),
            plain(),
            Setup {
                plan: Some("signed_out".to_owned()),
                ..Setup::default()
            },
            "not_found",
            Some(("kind", json!("thread"))),
        ),
    ];
    for (label, text, opts, setup, expected_code, expected_detail) in cases {
        let two = TwoDevices::new(setup);
        let refusal = refused(two.resolve(&text, &opts), label);
        assert_eq!(code(&refusal), expected_code, "{label}");
        if let Some((key, value)) = expected_detail {
            assert_eq!(detail(&refusal, key), Some(&value), "{label}");
        }
    }
}

#[test]
fn caller_and_workspace_refusals_are_not_swallowed_by_the_account_read() {
    let two = TwoDevices::new(Setup::default());
    let missing = refused(
        two.resolve_as("self", &plain(), UNKNOWN_CALLER, WORKSPACE),
        "self",
    );
    assert_eq!(code(&missing), "caller_context_missing");
    let denied = refused(
        two.resolve_as(
            "alpha",
            &plain(),
            HERE_LIVE,
            "/nonexistent-workspace-for-scope",
        ),
        "alpha",
    );
    assert_eq!(code(&denied), "scope_denied");
}

#[test]
fn a_miss_while_b_missed_the_round_names_the_devices_that_did_not_answer() {
    let two = TwoDevices::new(Setup {
        incomplete: true,
        ..Setup::default()
    });
    let missed = refused(two.resolve("no thread is named so", &plain()), "miss");
    assert_eq!(code(&missed), "host_unavailable");
    assert_eq!(detail(&missed, "devices"), Some(&json!([B_ID])));
    assert!(missed.message.contains(B_ID));
    assert!(missed.next_action.contains("thread_list"));
}

#[test]
fn a_device_that_answered_one_round_and_missed_the_next_is_remembered_for_its_threads() {
    let two = TwoDevices::new(Setup {
        drops_after_first_round: true,
        ..Setup::default()
    });
    let first = hit(two.resolve(B_LIVE, &plain()), B_LIVE);
    assert_eq!(first.thread, B_LIVE);
    let second = refused(two.resolve(B_LIVE, &plain()), B_LIVE);
    assert_eq!(code(&second), "host_unavailable");
    assert_eq!(text_detail(&second, "reason"), Some("listing_incomplete"));
    assert_eq!(detail(&second, "devices"), Some(&json!([B_ID])));
}

#[test]
fn an_id_never_listed_names_every_device_the_round_missed() {
    let two = TwoDevices::new(Setup {
        drops_after_first_round: true,
        quiet_third: true,
        ..Setup::default()
    });
    assert_eq!(hit(two.resolve(B_LIVE, &plain()), B_LIVE).thread, B_LIVE);
    let remembered = refused(two.resolve(B_LIVE, &plain()), B_LIVE);
    assert_eq!(detail(&remembered, "devices"), Some(&json!([B_ID])));
    let never = refused(two.resolve(NOWHERE, &plain()), NOWHERE);
    assert_eq!(text_detail(&never, "reason"), Some("listing_incomplete"));
    let mut devices: Vec<String> = detail(&never, "devices")
        .and_then(Value::as_array)
        .map(|listed| {
            listed
                .iter()
                .map(|device| device.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default();
    devices.sort();
    let mut expected = vec![B_ID.to_owned(), QUIET_ID.to_owned()];
    expected.sort();
    assert_eq!(devices, expected);
}

#[test]
fn two_online_devices_named_alike_are_both_candidates_with_their_ids() {
    let two = TwoDevices::new(Setup {
        twin: true,
        ..Setup::default()
    });
    let text = format!("{B_NAME}/beta");
    let ambiguous = refused(two.resolve(&text, &plain()), &text);
    let mut devices: Vec<String> = candidates(&ambiguous)
        .iter()
        .map(|candidate| field(candidate, "device"))
        .collect();
    devices.sort();
    let mut expected = vec![B_ID.to_owned(), B_TWIN_ID.to_owned()];
    expected.sort();
    assert_eq!(devices, expected);
}

#[test]
fn the_per_device_probe_decides_a_thread_id_nothing_lists() {
    let two = TwoDevices::new(with_probe());
    let held = hit(two.resolve(B_PROBE, &probing()), B_PROBE);
    assert_eq!(held.resolution, Resolution::Probe);
    assert!(matches!(
        &held.where_,
        ResolvedWhere::Device { row } if row.device == B_ID && row.state == "ended"
    ));

    let silent = TwoDevices::new(Setup {
        silent: vec![B_ID.to_owned()],
        ..with_probe()
    });
    let unanswered = refused(silent.resolve(NOWHERE, &probing()), NOWHERE);
    assert_eq!(code(&unanswered), "host_unavailable");
    assert_eq!(
        text_detail(&unanswered, "reason"),
        Some("device_unanswered")
    );
    assert_eq!(detail(&unanswered, "devices"), Some(&json!([B_ID])));

    let missed_round = TwoDevices::new(Setup {
        incomplete: true,
        ..with_probe()
    });
    let none = refused(missed_round.resolve(NOWHERE, &probing()), NOWHERE);
    assert_eq!(code(&none), "not_found");
}

#[test]
fn an_approved_reply_pinned_to_its_senders_device_asks_only_that_device() {
    let found = TwoDevices::new(Setup::default());
    let pin = Pin {
        device: B_ID,
        thread: B_LIVE,
    };
    let listed = ResolveOptions {
        pin: Some(pin),
        ..plain()
    };
    assert_eq!(hit(found.resolve(B_LIVE, &listed), "listed").thread, B_LIVE);

    let on_disk = TwoDevices::new(with_probe());
    let probed = ResolveOptions {
        pin: Some(Pin {
            device: B_ID,
            thread: B_PROBE,
        }),
        ..plain()
    };
    assert_eq!(
        hit(on_disk.resolve(B_PROBE, &probed), "probed").thread,
        B_PROBE
    );

    let nowhere = ResolveOptions {
        pin: Some(Pin {
            device: B_ID,
            thread: NOWHERE,
        }),
        ..plain()
    };
    let silent = TwoDevices::new(Setup {
        silent: vec![B_ID.to_owned()],
        ..with_probe()
    });
    let unanswered = refused(silent.resolve(NOWHERE, &nowhere), "silent");
    assert_eq!(code(&unanswered), "host_unavailable");
    assert_eq!(
        text_detail(&unanswered, "reason"),
        Some("host_not_answering")
    );
    assert_eq!(detail(&unanswered, "devices"), Some(&json!([B_ID])));

    let answered = TwoDevices::new(with_probe());
    let none = refused(answered.resolve(NOWHERE, &nowhere), "none");
    assert_eq!(code(&none), "not_found");
    assert_eq!(text_detail(&none, "device_name"), Some(B_NAME));

    let no_wake = TwoDevices::new(Setup::default());
    let cannot = refused(no_wake.resolve(NOWHERE, &nowhere), "no wake");
    assert_eq!(code(&cannot), "host_unavailable");
    assert_eq!(text_detail(&cannot, "reason"), Some("wake_unavailable"));
    assert_eq!(text_detail(&cannot, "thread"), Some(NOWHERE));
}

#[test]
fn a_device_qualified_id_b_does_not_list_is_asked_of_b_as_a_pinned_reply_is() {
    let cases: Vec<(&str, Setup, &str, &str)> = vec![
        (
            "B answered and holds no such thread",
            with_probe(),
            "not_found",
            "no_thread_on_device",
        ),
        (
            "B did not answer the probe",
            Setup {
                silent: vec![B_ID.to_owned()],
                ..with_probe()
            },
            "host_unavailable",
            "host_not_answering",
        ),
        (
            "this device's service carries no wake",
            Setup::default(),
            "host_unavailable",
            "wake_unavailable",
        ),
        (
            "B missed this listing round",
            Setup {
                incomplete: true,
                ..with_probe()
            },
            "host_unavailable",
            "listing_incomplete",
        ),
    ];
    for (label, setup, expected_code, expected_reason) in cases {
        let two = TwoDevices::new(setup);
        let text = format!("{B_ID}/{NOWHERE}");
        let refusal = refused(two.resolve(&text, &plain()), label);
        assert_eq!(code(&refusal), expected_code, "{label}");
        assert_eq!(text_detail(&refusal, "device"), Some(B_ID), "{label}");
        if expected_code == "host_unavailable" {
            assert_eq!(
                text_detail(&refusal, "reason"),
                Some(expected_reason),
                "{label}"
            );
        } else {
            assert_eq!(text_detail(&refusal, "kind"), Some("thread"), "{label}");
            assert_eq!(
                text_detail(&refusal, "device_name"),
                Some(B_NAME),
                "{label}"
            );
        }
    }
}

#[test]
fn an_offline_device_resolves_rows_in_hand_and_answers_device_offline_for_the_rest_with_nothing_sent()
 {
    let offline_b = Setup {
        b_offline: true,
        ..with_probe()
    };
    let two = TwoDevices::new(offline_b.clone());
    let text = format!("{B_ID}/{B_LIVE}");
    let listed = hit(two.resolve(&text, &probing()), &text);
    assert_eq!(where_device(&listed), Some(B_ID));

    let cases = [
        (format!("{B_ID}/{NOWHERE}"), offline_b.clone(), B_ID),
        (format!("{B_NAME}/no such thread"), offline_b, B_ID),
        (format!("{OFF_ID}/{NOWHERE}"), with_probe(), OFF_ID),
    ];
    for (text, setup, device) in cases {
        let two = TwoDevices::new(setup);
        let refusal = refused(two.resolve(&text, &probing()), &text);
        assert_eq!(code(&refusal), "device_offline", "{text}");
        assert_eq!(text_detail(&refusal, "device"), Some(device), "{text}");
        assert_eq!(two.counters().lookups, 0, "{text}");
    }
}
