use super::*;
use tempfile::tempdir;

#[cfg(unix)]
#[test]
fn read_only_files_inside_a_read_only_directory_are_removed() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempdir().unwrap();
    let tree = root.path().join("tree");
    std::fs::create_dir_all(tree.join("locked")).unwrap();
    for name in [
        "reflection-persona.md",
        "reflection-task.md",
        "transcript-payload.json",
    ] {
        std::fs::write(tree.join(name), "payload\n").unwrap();
        std::fs::set_permissions(tree.join(name), std::fs::Permissions::from_mode(0o400)).unwrap();
    }
    std::fs::write(tree.join("locked").join("inner.json"), "{}\n").unwrap();
    std::fs::set_permissions(
        tree.join("locked").join("inner.json"),
        std::fs::Permissions::from_mode(0o400),
    )
    .unwrap();
    std::fs::set_permissions(tree.join("locked"), std::fs::Permissions::from_mode(0o500)).unwrap();

    remove_tree(&tree).unwrap();

    assert!(!tree.exists());
}

#[test]
fn a_missing_path_is_success() {
    let root = tempdir().unwrap();
    let absent = root.path().join("absent");

    remove_tree(&absent).unwrap();

    assert!(!absent.exists());
}

#[test]
fn a_single_file_is_removed() {
    let root = tempdir().unwrap();
    let file = root.path().join("run.json");
    std::fs::write(&file, "{}").unwrap();

    remove_tree(&file).unwrap();

    assert!(!file.exists());
}
