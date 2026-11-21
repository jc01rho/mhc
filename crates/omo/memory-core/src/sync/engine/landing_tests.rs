use std::path::Path;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn ready(landing: &Landing) -> Option<(&str, Option<&str>)> {
    match landing {
        Landing::Ready { path, working } => Some((path.as_str(), working.as_deref())),
        Landing::Refused(_) => None,
    }
}

fn refused(landing: &Landing) -> Option<&str> {
    match landing {
        Landing::Refused(rule) => Some(rule.as_str()),
        Landing::Ready { .. } => None,
    }
}

#[test]
fn a_new_key_lands_and_its_parent_directory_is_created() -> TestResult {
    let root = tempfile::tempdir()?;
    let landing = prepare_landing(root.path(), Some("notes/a/b.md"))?;
    assert_eq!(ready(&landing), Some(("notes/a/b.md", None)));
    assert!(root.path().join("notes/a").is_dir());
    Ok(())
}

#[test]
fn an_existing_regular_file_lands_with_its_working_text() -> TestResult {
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join("a.md"), "hello")?;
    let landing = prepare_landing(root.path(), Some("a.md"))?;
    assert_eq!(ready(&landing), Some(("a.md", Some("hello"))));
    Ok(())
}

#[test]
fn an_ambiguous_key_is_refused() -> TestResult {
    let root = tempfile::tempdir()?;
    let landing = prepare_landing(root.path(), None)?;
    assert_eq!(refused(&landing), Some("ambiguous-normalization"));
    Ok(())
}

#[test]
fn a_key_naming_a_directory_is_a_non_regular_leaf() -> TestResult {
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join("dir"))?;
    let landing = prepare_landing(root.path(), Some("dir"))?;
    assert_eq!(refused(&landing), Some("non-regular-file"));
    Ok(())
}

#[test]
fn a_key_inside_git_is_outside_the_root() -> TestResult {
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join(".git"))?;
    let landing = prepare_landing(root.path(), Some(".git/config"))?;
    assert_eq!(refused(&landing), Some("outside-root"));
    Ok(())
}

#[test]
fn windows_refuses_a_path_git_cannot_stage() {
    let root = Path::new("C:\\memory");
    let long = "a".repeat(260);
    assert_eq!(
        commit_path_problem(root, &long, true),
        Some("git-path-too-long")
    );
    assert_eq!(commit_path_problem(root, "short.md", true), None);
    assert_eq!(commit_path_problem(root, &long, false), None);
}

#[cfg(unix)]
#[test]
fn a_symlinked_leaf_or_directory_is_refused_and_never_followed() -> TestResult {
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("secret.md"), "outside")?;
    let root = tempfile::tempdir()?;
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape"))?;
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        root.path().join("leaf.md"),
    )?;
    std::os::unix::fs::symlink(root.path().join("missing"), root.path().join("dangling.md"))?;
    for key in [
        "escape/secret.md",
        "escape/new.md",
        "leaf.md",
        "dangling.md",
    ] {
        let landing = prepare_landing(root.path(), Some(key))?;
        assert_eq!(refused(&landing), Some("symlink"), "{key}");
    }
    assert!(!outside.path().join("new.md").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_fifo_leaf_is_refused_before_it_is_read() -> TestResult {
    let root = tempfile::tempdir()?;
    let fifo = root.path().join("pipe.md");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status()?;
    assert!(made.success());
    let root_path = root.path().to_path_buf();
    let (sender, receiver) = std::sync::mpsc::channel();
    // Reading a FIFO blocks until a writer opens it, so the probe runs off the test thread and the
    // test fails (instead of hanging) when the leaf is read.
    std::thread::spawn(move || {
        let outcome = prepare_landing(&root_path, Some("pipe.md")).map_err(|e| e.to_string());
        let _ = sender.send(outcome);
    });
    let landing = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| "prepare_landing blocked on a FIFO leaf")??;
    assert_eq!(refused(&landing), Some("non-regular-file"));
    Ok(())
}
