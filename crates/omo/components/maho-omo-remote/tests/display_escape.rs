use maho_omo_remote::device::candidate_lines;
use maho_omo_remote::render::{
    device_lines, escape_for_display, escape_for_display_keep_lf, person_next_action,
    refusal_lines, session_ended_line, session_lines, status_lines, undelivered_prompt_lines,
    unsafe_for_display,
};
use maho_thread_address_core::{DeviceCandidate, RefusalCode, RefusalDetails};
use serde_json::json;

fn display_unsafe_fixtures() -> Vec<String> {
    let mut codes: Vec<u32> = Vec::new();
    codes.extend(0x00..=0x1f);
    codes.push(0x7f);
    codes.extend(0x80..=0x9f);
    codes.extend([0x061c, 0x200e, 0x200f]);
    codes.extend(0x202a..=0x202e);
    codes.extend(0x2066..=0x2069);
    codes.extend(0x200b..=0x200d);
    codes.extend(0x2060..=0x2064);
    codes.extend([0xfeff, 0x2028, 0x2029]);
    codes.extend(0xfff9..=0xfffb);
    codes.push(0xe0041);

    let mut fixtures: Vec<String> = codes
        .into_iter()
        .filter_map(char::from_u32)
        .map(|ch| format!("a{ch}b"))
        .collect();
    for seq in [
        "\u{001b}]52;c;aGk=\u{0007}",
        "\u{001b}[2J\u{001b}[H",
        "\u{001b}[31mred\u{001b}[0m",
        "\u{009b}2J",
        "\u{009d}52;c;aGk=\u{0007}",
        "\u{001b}]0;x\u{0007}visible\u{009b}31m",
    ] {
        fixtures.push(format!("a{seq}b"));
    }
    fixtures
}

const ORDINARY: [&str; 4] = ["세션 이름", "e\u{0301}", "\u{1F600}", "naïve café"];

fn has_unsafe_char(text: &str, keep_line_feeds: bool) -> bool {
    text.chars()
        .any(|ch| unsafe_for_display(ch, keep_line_feeds))
}

fn every_remote_line(value: &str) -> Vec<String> {
    let status = json!({
        "state": "connected",
        "enabled": true,
        "device": { "name": value },
        "detail": { "action": value },
    });
    let devices = vec![json!({
        "device": value,
        "label": value,
        "kind": value,
        "platform": value,
        "online": true,
        "last_seen_ms": null,
    })];
    let rows = vec![json!({
        "device": value,
        "session_id": value,
        "title": value,
        "source": "cache",
        "state": value,
        "project": value,
        "updated_ms": 0,
    })];
    let unanswered = vec![value.to_owned()];
    let details = RefusalDetails::Device {
        kind: "device",
        device: value.to_owned(),
        candidates: vec![DeviceCandidate {
            device: value.to_owned(),
            name: value.to_owned(),
            online: true,
        }],
    };
    let candidates = candidate_lines(RefusalCode::AmbiguousTarget, &details);
    let mut lines = Vec::new();
    lines.extend(status_lines(
        &status,
        value,
        &[(value.to_owned(), value.to_owned())],
    ));
    lines.extend(device_lines(&devices, 0));
    lines.extend(session_lines(&rows, &devices, &unanswered, 0));
    lines.extend(candidates.clone());
    lines.extend(refusal_lines(value, value, &candidates));
    lines.extend(undelivered_prompt_lines(value, value, value));
    lines.push(session_ended_line(value, value));
    lines
}

#[test]
fn escape_for_display_escapes_all_control_format_and_separator_characters() {
    assert_eq!(
        escape_for_display("evil\u{202e}name\nnext line\u{001b}[31m"),
        r"evil\u202ename\u000anext line\u001b[31m"
    );
    assert_eq!(escape_for_display("a\u{2028}b"), r"a\u2028b");
    assert_eq!(escape_for_display("a\u{2029}b"), r"a\u2029b");
    assert_eq!(escape_for_display("a\u{200b}b"), r"a\u200bb");
    assert_eq!(escape_for_display("a\u{200d}b"), r"a\u200db");
    assert_eq!(escape_for_display("a\u{e0041}b"), r"a\u{e0041}b");
    assert_eq!(
        escape_for_display_keep_lf("one\ntwo\r\nthree"),
        "one\ntwo\\u000d\nthree"
    );

    for fixture in display_unsafe_fixtures() {
        let shown = escape_for_display(&fixture);
        assert!(
            !has_unsafe_char(&shown, false),
            "unsafe char left in {shown:?} for {fixture:?}"
        );
        assert!(shown.starts_with('a') && shown.ends_with('b'));

        let kept_lf = escape_for_display_keep_lf(&fixture);
        assert!(
            !has_unsafe_char(&kept_lf, true),
            "unsafe char left in {kept_lf:?} for {fixture:?}"
        );
    }

    for text in ORDINARY {
        assert_eq!(escape_for_display(text), text);
        assert_eq!(escape_for_display_keep_lf(text), text);
    }
}

#[test]
fn remote_human_line_builders_leave_no_unsafe_code_units_and_keep_ordinary_text() {
    for fixture in display_unsafe_fixtures() {
        for line in every_remote_line(&fixture) {
            assert!(
                !has_unsafe_char(&line, false),
                "unsafe unit in {line:?} for {fixture:?}"
            );
        }
        assert_eq!(person_next_action(&fixture), fixture);
    }

    for text in ORDINARY {
        let joined = every_remote_line(text).join("\n");
        assert!(
            joined.matches(text).count() >= 10,
            "ordinary text {text} missing in:\n{joined}"
        );
    }
}
