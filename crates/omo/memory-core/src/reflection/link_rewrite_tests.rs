use super::*;

fn targets(live: &[&str], removed: &[(&str, Successor)]) -> LinkTargets {
    LinkTargets {
        live: live.iter().map(|path| (*path).to_owned()).collect(),
        removed: removed
            .iter()
            .map(|(path, successor)| ((*path).to_owned(), successor.clone()))
            .collect(),
    }
}

fn moved(path: &str) -> Successor {
    Successor::Moved {
        path: path.to_owned(),
    }
}

#[test]
fn wiki_links_follow_their_successor() {
    let targets = targets(
        &["notes/index.md", "people/ada.md", "ARCHIVE.md"],
        &[
            ("people/ada-old.md", moved("people/ada.md")),
            ("notes/old.md", Successor::Archived),
            ("notes/gone.md", Successor::Removed),
        ],
    );
    let content = "---\ndescription: index\n---\n\nSee [[people/ada-old]] and [[people/ada-old.md#bio|Ada]].\nOld: [[notes/old|the old plan]]; gone: [[notes/gone]] and [[notes/gone|that one]].\n";

    let next = repoint_links("notes/index.md", content, &targets, None);

    assert_eq!(
        next,
        "---\ndescription: index\n---\n\nSee [[people/ada]] and [[people/ada.md#bio|Ada]].\nOld: [[ARCHIVE.md|the old plan (archived)]]; gone: notes/gone and that one.\n"
    );
}

#[test]
fn markdown_links_are_rewritten_relative_to_the_note() {
    let targets = targets(
        &["notes/index.md", "people/ada.md", "ARCHIVE.md"],
        &[
            ("notes/ada.md", moved("people/ada.md")),
            ("notes/old.md", Successor::Archived),
            ("notes/gone.md", Successor::Removed),
        ],
    );
    let content = "Read [Ada](ada.md#bio), [old](old.md#x) and [gone](gone.md).\n";

    let next = repoint_links("notes/index.md", content, &targets, None);

    assert_eq!(
        next,
        "Read [Ada](../people/ada.md#bio), [old](../ARCHIVE.md) and gone.\n"
    );
}

#[test]
fn code_and_images_stay_as_written() {
    let targets = targets(&["a.md"], &[("gone.md", Successor::Removed)]);
    let content = "```\n[[gone]]\n```\n`[[gone]]` and ![img](gone.md) but [[gone]]\n";

    let next = repoint_links("a.md", content, &targets, None);

    assert_eq!(
        next,
        "```\n[[gone]]\n```\n`[[gone]]` and ![img](gone.md) but gone\n"
    );
}

#[test]
fn a_moved_note_keeps_its_own_relative_links_pointing_at_their_targets() {
    let targets = targets(
        &["archive/2026/plan.md", "notes/ref.md"],
        &[("notes/plan.md", moved("archive/2026/plan.md"))],
    );

    let next = repoint_links(
        "archive/2026/plan.md",
        "See [ref](ref.md).\n",
        &targets,
        Some("notes/plan.md"),
    );

    assert_eq!(next, "See [ref](../../notes/ref.md).\n");
}

#[test]
fn live_and_external_links_are_untouched() {
    let targets = targets(&["a.md", "b.md"], &[("gone.md", Successor::Removed)]);
    let content = "[[b]] [b](b.md) [web](https://example.com/gone.md) [[../gone]]\n";

    assert_eq!(repoint_links("a.md", content, &targets, None), content);
}

#[test]
fn inline_code_spans_match_the_backreference_pattern() {
    assert_eq!(inline_code_spans("a `x` b"), vec![(2, 5)]);
    assert_eq!(inline_code_spans("``x``"), vec![(0, 5)]);
    assert_eq!(inline_code_spans("``a`"), vec![(0, 2)]);
    assert_eq!(inline_code_spans("`lone"), Vec::<(usize, usize)>::new());
}

#[test]
fn posix_helpers_match_node() {
    assert_eq!(posix_normalize("a/./b/../c.md"), "a/c.md");
    assert_eq!(posix_normalize("../x"), "../x");
    assert_eq!(posix_normalize(""), ".");
    assert_eq!(posix_normalize("a/.."), ".");
    assert_eq!(posix_join(".", "x.md"), "x.md");
    assert_eq!(posix_dirname("note.md"), ".");
    assert_eq!(posix_dirname("a/b/c.md"), "a/b");
    assert_eq!(posix_basename("a/b/c.md"), "c.md");
    assert_eq!(posix_relative("notes", "people/ada.md"), "../people/ada.md");
    assert_eq!(posix_relative(".", "ARCHIVE.md"), "ARCHIVE.md");
}
