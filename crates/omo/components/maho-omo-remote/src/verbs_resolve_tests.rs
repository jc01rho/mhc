//! Ported cases of upstream `remote/resolve.test.ts` and the CLI-applicable rows of
//! `thread/address/surfaces.table.test.ts` (#14305 U8) for the shared thread-part ladder.

use serde_json::{Value, json};

use super::match_session_rows;

fn row(device: &str, key: &str, durable: &str, title: &str, project: &str) -> Value {
    json!({
        "device": device,
        "session_id": key,
        "durable_id": durable,
        "title": title,
        "project": project,
        "name": title,
    })
}

fn ids(rows: &[Value], query: &str) -> Vec<String> {
    match_session_rows(rows, query)
        .iter()
        .map(|row| row["durable_id"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn durable_id_name_and_device_qualified_id_each_answer_their_row() {
    // resolve.test.ts: "resolved by durable id, by name and by <device>/<durable id>".
    let rows = vec![row("dev-b", "rk-1", "dur-1", "Build Bot", "/work/omo")];
    for query in ["dur-1", "build  bot", "ＢＵＩＬＤ ＢＯＴ"] {
        assert_eq!(ids(&rows, query), vec!["dur-1"], "{query}");
    }
}

#[test]
fn one_thread_id_listed_by_two_devices_is_ambiguous_and_the_device_handle_picks_one() {
    // resolve.test.ts: "two devices each with a session of one name" and
    // "one thread id listed by two devices with no name"; surfaces.table.test.ts: "ambiguous: a device name two
    // online devices share".
    let rows = vec![
        row("dev-b", "rk-1", "dur-twice", "", "/work/anon-folder"),
        row("dev-c", "rk-2", "dur-twice", "", "/work/anon-folder"),
    ];
    assert_eq!(ids(&rows, "dur-twice"), vec!["dur-twice", "dur-twice"]);
}

#[test]
fn exact_id_beats_another_rows_listing_key() {
    // resolve.test.ts: "exact thread id wins, device-qualified and bare alike (exact id > listing key)".
    let rows = vec![
        row("dev-b", "rk-1", "dur-1", "Bot", "/work/omo"),
        row("dev-b", "dur-1", "dur-9", "Other", "/work/omo"),
    ];
    assert_eq!(ids(&rows, "dur-1"), vec!["dur-1"]);
    assert_eq!(ids(&rows, "rk-1"), vec!["dur-1"]);
}

#[test]
fn unique_hex_id_prefix_counts_each_durable_id_once() {
    // resolve.test.ts: "unique 8+ hex id prefix"; surfaces.table.test.ts: "8+ hex thread id prefix".
    let rows = vec![
        row("dev-b", "rk-1", "01234567-aaaa-bbbb", "Bot", "/work/omo"),
        row("dev-c", "rk-2", "01234567-cccc-dddd", "Twin", "/work/omo"),
    ];
    assert_eq!(
        ids(&rows, "01234567"),
        vec!["01234567-aaaa-bbbb", "01234567-cccc-dddd"]
    );
    assert_eq!(ids(&rows, "01234567-a"), vec!["01234567-aaaa-bbbb"]);
    assert!(
        ids(&rows, "0123456").is_empty(),
        "a prefix shorter than 8 hex digits never matches"
    );
}

#[test]
fn an_unnamed_rows_folder_fallback_is_not_an_address() {
    // resolve.test.ts: "each ambiguous_target candidate says its name is a fallback (unnamed)".
    let rows = vec![json!({
        "device": "dev-b",
        "session_id": "rk-1",
        "durable_id": "dur-1",
        "title": "",
        "name": "folder",
        "unnamed": true,
        "project": "folder",
    })];
    assert!(
        ids(&rows, "folder").is_empty(),
        "a fallback name is never an address"
    );
    assert_eq!(ids(&rows, "rk-1"), vec!["dur-1"]);
}

#[test]
fn a_row_without_a_durable_id_is_named_by_its_listing_key() {
    // surfaces.table.test.ts: "<device name>/<listing key>" for a row whose durable id is absent.
    let rows = vec![json!({
        "device": "dev-b",
        "session_id": "rk-9",
        "title": "Keyed",
        "name": "Keyed",
        "project": "/work/omo",
    })];
    assert_eq!(ids(&rows, "rk-9"), vec![""]);
    assert_eq!(ids(&rows, "Keyed"), vec![""]);
}

#[test]
fn a_name_no_row_holds_and_an_unlisted_session_are_misses() {
    // surfaces.table.test.ts: "nowhere: a name no device holds" and "nowhere: a thread id no device holds".
    let rows = vec![row("dev-b", "rk-1", "dur-1", "Bot", "/work/omo")];
    assert!(ids(&rows, "no-such-thread").is_empty());
    assert!(ids(&rows, "dur-gone").is_empty());
}
