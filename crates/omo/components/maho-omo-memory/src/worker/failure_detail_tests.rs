use super::*;

const BUN_CRASH_STDERR: &str = concat!(
    "340 |         const darkPath = path.join(themesDir, \"dark.json\");\n",
    "345 |             dark: JSON.parse(fs.readFileSync(darkPath, \"utf-8\")),\n",
    "                                      ^\n",
    "ENOENT: no such file or directory, open '/opt/omo-runtime/dist/modes/interactive/theme/dark.json'\n",
    "    path: \"/opt/omo-runtime/dist/modes/interactive/theme/dark.json\",\n",
    "  syscall: \"open\",\n",
    "    errno: -2,\n",
    "     code: \"ENOENT\"\n",
    "\n",
    "      at getBuiltinThemes (/global/senpi/dist/modes/interactive/theme/theme.js:345:33)\n",
    "      at loadTheme (/global/senpi/dist/modes/interactive/theme/theme.js:513:23)\n",
    "\n",
    "Bun v1.4.2 (macOS arm64)",
);

#[test]
fn a_bun_crash_tail_keeps_only_the_message_line() {
    assert_eq!(
        child_failure_cause(Some(BUN_CRASH_STDERR)).as_deref(),
        Some(
            "ENOENT: no such file or directory, open '/opt/omo-runtime/dist/modes/interactive/theme/dark.json'"
        )
    );
}

#[test]
fn a_one_sentence_detail_survives_unchanged() {
    assert_eq!(
        child_failure_cause(Some("memory run supervisor exited with 143")).as_deref(),
        Some("memory run supervisor exited with 143")
    );
}

#[test]
fn the_truncation_marker_is_dropped() {
    assert_eq!(
        child_failure_cause(Some(
            "[truncated to last 65536 bytes]\nTypeError: undefined is not a function"
        ))
        .as_deref(),
        Some("TypeError: undefined is not a function")
    );
}

#[test]
fn a_named_error_wins_over_line_order() {
    assert_eq!(
        child_failure_cause(Some(
            "Resolving the model catalog\nError: Model \"fake/one\" not found."
        ))
        .as_deref(),
        Some("Error: Model \"fake/one\" not found.")
    );
}

#[test]
fn only_machinery_leaves_no_cause() {
    assert_eq!(
        child_failure_cause(Some(
            "345 |   dark: JSON.parse(x),\n      ^\n  at loadTheme (theme.js:513:23)\nBun v1.4.2 (macOS arm64)"
        )),
        None
    );
}

#[test]
fn an_empty_or_absent_detail_invents_nothing() {
    assert_eq!(child_failure_cause(None), None);
    assert_eq!(child_failure_cause(Some("")), None);
    assert_eq!(child_failure_cause(Some("   \n\t\n")), None);
}

#[test]
fn a_pathological_line_is_bounded() {
    let detail = format!("Error: {}", "x".repeat(FAILURE_CAUSE_MAX_CHARS * 2));

    let cause = child_failure_cause(Some(&detail)).unwrap();

    assert_eq!(cause.chars().count(), FAILURE_CAUSE_MAX_CHARS);
    assert!(cause.ends_with('…'));
}
