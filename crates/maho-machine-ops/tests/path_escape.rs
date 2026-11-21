//! The escape matrix. Every test is written so it fails when the confinement it names is removed: the
//! comments say which line of the crate each case guards (`gate::check_path` containment,
//! `filesystem::canonicalize_by_opening` symlink following, `local_files::split_glob_base` and the glob
//! branch of `LocalFileOps::glob`, `real_path::spawn_path_in`).
#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;

use common::{Fixture, SECRET_TEXT, code, text};
use maho_machine_ops::{
    EditRequest, GlobRequest, GrepRequest, ListRequest, OpErrorCode, ReadRequest, StatRequest,
    WriteRequest, spawn_path_in,
};

const DENIED: OpErrorCode = OpErrorCode::PolicyDenied;

fn glob_paths(result: maho_machine_ops::GlobResult) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<_> = result.entries.into_iter().map(|entry| entry.path).collect();
    paths.sort();
    paths
}

// Guards gate::check_path `contains`: without the containment check a `..` path reads outside the root.
#[tokio::test]
async fn dotdot_escape_is_denied_for_every_operation() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    assert_eq!(
        code(
            files
                .read(&ReadRequest::new("../outside/secret.txt"), &ctx)
                .await
        ),
        DENIED
    );
    assert_eq!(
        code(
            files
                .write(&WriteRequest::new("../outside/planted.txt", "x"), &ctx)
                .await
        ),
        DENIED
    );
    assert_eq!(
        code(
            files
                .edit(
                    &EditRequest::new("../outside/secret.txt", "secret", "x"),
                    &ctx
                )
                .await
        ),
        DENIED
    );
    assert_eq!(
        code(files.stat(&StatRequest::new("../outside"), &ctx).await),
        DENIED
    );
    assert_eq!(
        code(files.list(&ListRequest::new("../outside"), &ctx).await),
        DENIED
    );
    assert_eq!(
        code(
            files
                .grep(&GrepRequest::new("../outside", "secret"), &ctx)
                .await
        ),
        DENIED
    );
    assert!(!f.outside.join("planted.txt").exists());
    assert_eq!(text(&f.outside.join("secret.txt")), SECRET_TEXT);
}

// Guards gate::check_path: an absolute path is not exempt from containment.
#[tokio::test]
async fn an_absolute_path_outside_the_root_is_denied() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    let secret = f.outside.join("secret.txt");
    assert_eq!(
        code(files.read(&ReadRequest::new(&secret), &ctx).await),
        DENIED
    );
    assert_eq!(
        code(
            files
                .write(&WriteRequest::new(&secret, "overwritten"), &ctx)
                .await
        ),
        DENIED
    );
    assert_eq!(text(&secret), SECRET_TEXT);
}

// Guards `contains` being component-wise: `root-evil` starts with the string `root` but is not inside it.
#[tokio::test]
async fn a_sibling_sharing_the_roots_string_prefix_is_denied() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    assert_eq!(
        code(
            files
                .read(&ReadRequest::new("../root-evil/secret.txt"), &ctx)
                .await
        ),
        DENIED
    );
    assert_eq!(
        code(
            files
                .read(&ReadRequest::new(f.sibling.join("secret.txt")), &ctx)
                .await
        ),
        DENIED
    );
}

// Guards gate::check_path: the root itself and its children are inside.
#[tokio::test]
async fn the_root_itself_and_a_dotdot_that_stays_inside_are_allowed() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    assert!(files.list(&ListRequest::new("."), &ctx).await.is_ok());
    let read = files
        .read(&ReadRequest::new("dir/../inside.txt"), &ctx)
        .await
        .unwrap();
    assert_eq!(read.path, f.root.join("inside.txt"));
}

// Guards canonicalize_by_opening: the symlink is followed, so `escape/...` lands in `outside`.
#[cfg(unix)]
#[tokio::test]
async fn a_symlink_escaping_the_root_is_denied_for_every_operation() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    assert_eq!(
        code(
            files
                .read(&ReadRequest::new("escape/secret.txt"), &ctx)
                .await
        ),
        DENIED
    );
    assert_eq!(
        code(
            files
                .write(&WriteRequest::new("escape/planted.txt", "x"), &ctx)
                .await
        ),
        DENIED
    );
    assert_eq!(
        code(
            files
                .edit(&EditRequest::new("escape/secret.txt", "secret", "x"), &ctx)
                .await
        ),
        DENIED
    );
    assert_eq!(
        code(files.stat(&StatRequest::new("escape"), &ctx).await),
        DENIED
    );
    assert_eq!(
        code(files.list(&ListRequest::new("escape"), &ctx).await),
        DENIED
    );
    assert_eq!(
        code(
            files
                .grep(&GrepRequest::new("escape", "secret"), &ctx)
                .await
        ),
        DENIED
    );
    assert_eq!(
        code(
            files
                .glob(&GlobRequest::in_dir("*.txt", "escape"), &ctx)
                .await
        ),
        DENIED
    );
    assert!(!f.outside.join("planted.txt").exists());
    assert_eq!(text(&f.outside.join("secret.txt")), SECRET_TEXT);
}

// Guards canonicalize_by_opening's missing-tail handling: the parent `escape` exists and is a symlink, the
// target does not exist yet, and the canonical path must still land outside the root.
#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_parent_of_a_missing_write_target_is_denied() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    assert_eq!(
        code(
            files
                .write(&WriteRequest::new("escape/new.txt", "x"), &ctx)
                .await
        ),
        DENIED
    );
    let mut nested = WriteRequest::new("escape/made/deeper.txt", "x");
    nested.create_parent_dirs = true;
    assert_eq!(code(files.write(&nested, &ctx).await), DENIED);
    assert!(!f.outside.join("new.txt").exists());
    assert!(!f.outside.join("made").exists());
}

// Guards the symlink branch of canonicalize_by_opening: a dangling link inside the root that points outside
// resolves to where a write would land, not to the link's own (inside) name.
#[cfg(unix)]
#[tokio::test]
async fn a_dangling_symlink_pointing_outside_is_denied() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    std::os::unix::fs::symlink(f.outside.join("planted.txt"), f.root.join("dangling")).unwrap();
    assert_eq!(
        code(files.write(&WriteRequest::new("dangling", "x"), &ctx).await),
        DENIED
    );
    assert!(!f.outside.join("planted.txt").exists());
}

// Guards canonicalization of the root itself: a root reached through a symlink must still contain its
// children, and still refuse `..` out of it.
#[cfg(unix)]
#[tokio::test]
async fn a_root_reached_through_a_symlink_still_confines() {
    let f = Fixture::create();
    let files = f.ops.files;
    let linked_root = f.base.path().join("root-link");
    std::os::unix::fs::symlink(&f.root, &linked_root).unwrap();
    let ctx = f.ctx_rooted(&linked_root);
    assert!(
        files
            .read(&ReadRequest::new("inside.txt"), &ctx)
            .await
            .is_ok()
    );
    assert_eq!(
        code(
            files
                .read(&ReadRequest::new("../outside/secret.txt"), &ctx)
                .await
        ),
        DENIED
    );
}

// Guards the glob branch of LocalFileOps::glob: an absolute pattern inside the root runs...
#[tokio::test]
async fn a_rooted_glob_pattern_inside_the_root_runs() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    let expected = vec![f.root.join("dir").join("a.ts")];
    let absolute = f
        .root
        .join("dir")
        .join("*.ts")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        glob_paths(files.glob(&GlobRequest::new(absolute), &ctx).await.unwrap()),
        expected
    );
    let dotdot = GlobRequest::in_dir("../dir/*.ts", f.root.join("dir"));
    assert_eq!(
        glob_paths(files.glob(&dotdot, &ctx).await.unwrap()),
        expected
    );
}

// ...and the one outside is refused (without the glob split the pattern would be matched unchecked).
#[tokio::test]
async fn a_rooted_glob_pattern_outside_the_root_is_denied() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    let outside = f.outside.join("*").to_string_lossy().into_owned();
    assert_eq!(
        code(files.glob(&GlobRequest::new(outside), &ctx).await),
        DENIED
    );
    assert_eq!(
        code(files.glob(&GlobRequest::new("../*"), &ctx).await),
        DENIED
    );
    let evil = f.sibling.join("*").to_string_lossy().into_owned();
    assert_eq!(
        code(files.glob(&GlobRequest::new(evil), &ctx).await),
        DENIED
    );
    let search_outside = GlobRequest::in_dir("*.txt", &f.outside);
    assert_eq!(code(files.glob(&search_outside, &ctx).await), DENIED);
}

// Guards the `rest.contains("..")` refusal in split_glob_base: where `..` after a wildcard lands is only
// known after matching, so it is refused up front.
#[tokio::test]
async fn dotdot_after_a_wildcard_is_denied() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    assert_eq!(
        code(files.glob(&GlobRequest::new("dir/*/../a.ts"), &ctx).await),
        DENIED
    );
    assert_eq!(
        code(files.glob(&GlobRequest::new("**/../*"), &ctx).await),
        DENIED
    );
}

// Guards check_path on the glob base: an absolute pattern through the escaping symlink is refused.
#[cfg(unix)]
#[tokio::test]
async fn a_rooted_glob_pattern_through_a_symlink_escaping_the_root_is_denied() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    let through = f
        .root
        .join("escape")
        .join("*")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        code(files.glob(&GlobRequest::new(through), &ctx).await),
        DENIED
    );
    let relative = files
        .glob(&GlobRequest::new("escape/*"), &ctx)
        .await
        .unwrap();
    assert!(relative.entries.is_empty());
}

// Guards the empty-rest branch of split_glob_base: a pattern ending in a separator lists that directory
// like `*` (the walker used to match nothing for an empty pattern).
#[tokio::test]
async fn a_pattern_ending_in_a_separator_lists_that_directory_like_star() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    std::fs::write(f.root.join("dir").join("b.ts"), "b").unwrap();
    let dir = f.root.join("dir");
    let star = glob_paths(
        files
            .glob(&GlobRequest::in_dir("*", &dir), &ctx)
            .await
            .unwrap(),
    );
    assert_eq!(star, vec![dir.join("a.ts"), dir.join("b.ts")]);
    let absolute = format!("{}/", dir.to_string_lossy());
    assert_eq!(
        glob_paths(files.glob(&GlobRequest::new(absolute), &ctx).await.unwrap()),
        star
    );
    let dotdot = GlobRequest::in_dir("../dir/", &dir);
    assert_eq!(glob_paths(files.glob(&dotdot, &ctx).await.unwrap()), star);
}

// Guards split_glob_base's "no glob syntax splits before the last segment": the base is the file's
// directory, so a literal path outside the root is still refused and one inside is found.
#[tokio::test]
async fn a_pattern_without_glob_syntax_is_split_before_its_last_segment() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    let inside = f
        .root
        .join("dir")
        .join("a.ts")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        glob_paths(files.glob(&GlobRequest::new(inside), &ctx).await.unwrap()),
        vec![f.root.join("dir").join("a.ts")]
    );
    let outside = f.outside.join("secret.txt").to_string_lossy().into_owned();
    assert_eq!(
        code(files.glob(&GlobRequest::new(outside), &ctx).await),
        DENIED
    );
    assert_eq!(
        code(
            files
                .glob(&GlobRequest::new("../outside/secret.txt"), &ctx)
                .await
        ),
        DENIED
    );
}

// Guards the open-free fallback of canonicalize_filesystem_path_with: when the real canonicalize misses its
// deadline the walker still resolves the symlink, so a containment decision is not made on a guess.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_resolution_past_its_deadline_falls_back_to_the_open_free_walker() {
    use maho_machine_ops::{RESOLUTION_DEADLINE, canonicalize_filesystem_path_with};
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir(base.join("real")).unwrap();
    std::os::unix::fs::symlink("real", base.join("link")).unwrap();
    let never =
        |_path: std::path::PathBuf| std::future::pending::<std::io::Result<std::path::PathBuf>>();
    let canonical = canonicalize_filesystem_path_with(
        &base.join("link").join("new.txt"),
        RESOLUTION_DEADLINE,
        never,
    )
    .await
    .unwrap();
    assert_eq!(canonical, base.join("real").join("new.txt"));
}

// Guards the strictness of the fallback: past the deadline a symlink loop is an error, never a guess that
// a containment decision could accept.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn the_fallback_fails_closed_on_a_non_missing_error() {
    use maho_machine_ops::{RESOLUTION_DEADLINE, canonicalize_filesystem_path_with};
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    std::os::unix::fs::symlink("b", base.join("a")).unwrap();
    std::os::unix::fs::symlink("a", base.join("b")).unwrap();
    let never =
        |_path: std::path::PathBuf| std::future::pending::<std::io::Result<std::path::PathBuf>>();
    let error = canonicalize_filesystem_path_with(&base.join("a"), RESOLUTION_DEADLINE, never)
        .await
        .unwrap_err();
    assert_eq!(error.code, OpErrorCode::IoError);
}

// Guards spawn_path_in: a confined start directory must resolve inside the root.
#[tokio::test]
async fn a_confined_spawn_path_must_resolve_inside_the_root() {
    let f = Fixture::create();
    let real = spawn_path_in(&f.root, Path::new("dir/./a-sub/../a.ts"), true)
        .await
        .unwrap();
    assert!(real.ends_with(Path::new("dir").join("a.ts")), "{real:?}");
    let escaped = spawn_path_in(&f.root, Path::new("../outside"), true)
        .await
        .unwrap_err();
    assert_eq!(escaped.code, DENIED);
    let above = spawn_path_in(&f.root, &f.outside, true).await.unwrap_err();
    assert_eq!(above.code, DENIED);
    let sibling = spawn_path_in(&f.root, &f.sibling, true).await.unwrap_err();
    assert_eq!(sibling.code, DENIED);
}

// Guards the symlink walk of real_path_under: a link inside the root that leads outside is not inside.
#[cfg(unix)]
#[tokio::test]
async fn a_confined_spawn_path_through_an_escaping_symlink_is_denied() {
    let f = Fixture::create();
    let error = spawn_path_in(&f.root, Path::new("escape"), true)
        .await
        .unwrap_err();
    assert_eq!(error.code, DENIED);
    let deeper = spawn_path_in(&f.root, Path::new("escape/missing/child"), true)
        .await
        .unwrap_err();
    assert_eq!(deeper.code, DENIED);
}

// An unconfined root only anchors a relative path; the OS decides the rest.
#[tokio::test]
async fn an_unconfined_spawn_path_only_anchors_a_relative_path() {
    let f = Fixture::create();
    let anchored = spawn_path_in(&f.root, Path::new("dir"), false)
        .await
        .unwrap();
    assert!(anchored.ends_with("dir"));
    let outside = spawn_path_in(&f.root, &f.outside, false).await.unwrap();
    assert!(outside.ends_with("outside"));
}
