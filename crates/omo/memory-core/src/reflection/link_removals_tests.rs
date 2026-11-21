use super::*;

#[test]
fn the_archive_names_a_note_by_its_path_with_or_without_md() {
    assert!(recorded_in(
        "+- notes/old-plan.md: superseded",
        "notes/old-plan.md",
        false
    ));
    assert!(recorded_in(
        "+- notes/old-plan (superseded)",
        "notes/old-plan.md",
        false
    ));
    assert!(recorded_in("notes/old-plan.md", "notes/old-plan.md", false));
}

#[test]
fn a_longer_or_differently_typed_name_does_not_count() {
    assert!(!recorded_in(
        "+- notes/old-plan.mdx",
        "notes/old-plan.md",
        false
    ));
    assert!(!recorded_in(
        "+- notes/old-plan.txt",
        "notes/old-plan.md",
        false
    ));
    assert!(!recorded_in(
        "+- notes/old-plan-2.md",
        "notes/old-plan.md",
        false
    ));
    assert!(!recorded_in(
        "+- archive/notes/old-plan.md",
        "notes/old-plan.md",
        false
    ));
}

#[test]
fn a_bare_file_name_counts_only_when_no_other_note_shares_it() {
    assert!(recorded_in(
        "+- old-plan.md moved out",
        "notes/old-plan.md",
        true
    ));
    assert!(!recorded_in(
        "+- old-plan.md moved out",
        "notes/old-plan.md",
        false
    ));
    assert!(!recorded_in(
        "+- old-plan moved out",
        "notes/old-plan.md",
        true
    ));
}
