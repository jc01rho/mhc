//! The socket secret lifecycle of sisyphuslabs/omo-desktop-app#4199 (design 4.2-4.5), as senpi's
//! `socket-transport.ts` ships it: what `ensureSocketSecret` and `createSocketSecret` do to the file. Each case that
//! guards a security rule names the rule; reverting it makes that case fail.
use maho_socket_secret::{
    SOCKET_SECRET_BYTES, create_socket_secret, prepare_socket_secret_parent,
    provision_socket_secret_locked, read_valid_socket_secret, socket_secret_lock_path,
};
use std::path::{Path, PathBuf};

fn secret_path(dir: &Path) -> PathBuf {
    dir.join("rpc").join("rpc.sock.secret")
}

fn staging_leftovers(secret: &Path) -> Vec<String> {
    let parent = secret.parent().expect("parent");
    std::fs::read_dir(parent)
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".stage-"))
        .collect()
}

#[test]
fn a_created_secret_is_valid_and_is_what_the_file_holds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = secret_path(dir.path());

    let secret = create_socket_secret(&path).expect("create");

    assert_eq!(secret.len(), SOCKET_SECRET_BYTES);
    assert_eq!(std::fs::read(&path).expect("read"), secret.to_vec());
    assert_eq!(read_valid_socket_secret(&path).expect("read"), Some(secret));
}

#[test]
fn a_missing_secret_reads_as_none_and_a_wrong_length_one_is_not_valid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = secret_path(dir.path());
    assert_eq!(read_valid_socket_secret(&path).expect("missing"), None);

    prepare_socket_secret_parent(&path).expect("parent");
    for length in [
        0usize,
        8,
        SOCKET_SECRET_BYTES - 1,
        SOCKET_SECRET_BYTES + 1,
        4096,
    ] {
        std::fs::write(&path, vec![9u8; length]).expect("write");
        assert_eq!(
            read_valid_socket_secret(&path).expect("read"),
            None,
            "{length} bytes"
        );
    }
}

#[test]
fn provisioning_publishes_a_missing_secret_and_then_keeps_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = secret_path(dir.path());
    prepare_socket_secret_parent(&path).expect("parent");

    let first = provision_socket_secret_locked(&path).expect("provision");
    let second = provision_socket_secret_locked(&path).expect("provision again");

    assert_eq!(first, second);
    assert_eq!(std::fs::read(&path).expect("read"), first.to_vec());
}

#[test]
fn a_directory_at_the_secret_path_is_refused_and_kept() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = secret_path(dir.path());
    std::fs::create_dir_all(&path).expect("directory");

    assert!(create_socket_secret(&path).is_err());
    assert!(provision_socket_secret_locked(&path).is_err());
    assert!(read_valid_socket_secret(&path).is_err());
    assert!(path.is_dir());
}

#[test]
fn the_lock_path_is_one_regular_file_whatever_the_spelling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = secret_path(dir.path());
    prepare_socket_secret_parent(&path).expect("parent");

    let lock = socket_secret_lock_path(&path).expect("lock path");
    let alias = dir
        .path()
        .join("rpc")
        .join("..")
        .join("rpc")
        .join("rpc.sock.secret");
    let aliased = socket_secret_lock_path(&alias).expect("aliased lock path");

    assert_eq!(
        lock.file_name().and_then(|name| name.to_str()),
        Some("rpc.sock.secret.publish.lock")
    );
    assert_eq!(lock, aliased);
    assert!(lock.is_file());
    assert!(!path.exists(), "taking the lock path creates no secret");
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::{Arc, Barrier};

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    // Guards `.mode(0o600)` and the descriptor chmod of the staging file, and the 0o700 directory it makes.
    #[test]
    fn the_secret_and_the_directory_made_for_it_are_owner_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = secret_path(dir.path());

        create_socket_secret(&path).expect("create");

        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().expect("parent")), 0o700);
    }

    // Guards the lock file's creation mode: the lock database must never be readable by another user.
    #[test]
    fn the_lock_file_is_owner_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = secret_path(dir.path());
        prepare_socket_secret_parent(&path).expect("parent");

        let lock = socket_secret_lock_path(&path).expect("lock path");

        assert_eq!(mode(&lock), 0o600);
    }

    // Guards the no-replace publication (`link` instead of a write to the final path): a second create returns the
    // secret the first one published and the file keeps it.
    #[test]
    fn a_second_create_returns_the_published_secret_and_never_replaces_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = secret_path(dir.path());

        let first = create_socket_secret(&path).expect("create");
        let second = create_socket_secret(&path).expect("create again");

        assert_eq!(first, second);
        assert_eq!(std::fs::read(&path).expect("read"), first.to_vec());
    }

    // Guards the same rule under contention: concurrent creators all return the one secret that was published. A
    // truncate-and-write creator hands every thread its own secret and leaves the file with the last one.
    #[test]
    fn concurrent_creators_agree_on_the_one_published_secret() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = Arc::new(secret_path(dir.path()));
        let creators = 8;
        let start = Arc::new(Barrier::new(creators));
        let handles: Vec<_> = (0..creators)
            .map(|_| {
                let path = Arc::clone(&path);
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    create_socket_secret(&path).expect("create")
                })
            })
            .collect();
        let secrets: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("join"))
            .collect();

        let published = std::fs::read(&*path).expect("read");
        for secret in &secrets {
            assert_eq!(secret.to_vec(), published);
        }
        assert!(staging_leftovers(&path).is_empty());
    }

    // Guards the `O_NOFOLLOW` read and the regular-file check: a symlink at the secret path is refused on every
    // entry, and what it points at is neither read nor written.
    #[test]
    fn a_symlink_at_the_secret_path_is_refused_and_its_target_is_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = secret_path(dir.path());
        prepare_socket_secret_parent(&path).expect("parent");
        let outside = dir.path().join("outside");
        std::fs::write(&outside, [7u8; SOCKET_SECRET_BYTES]).expect("outside");
        symlink(&outside, &path).expect("symlink");

        assert!(create_socket_secret(&path).is_err());
        assert!(provision_socket_secret_locked(&path).is_err());
        assert!(read_valid_socket_secret(&path).is_err());

        assert_eq!(
            std::fs::read(&outside).expect("read"),
            vec![7u8; SOCKET_SECRET_BYTES]
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .expect("metadata")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn a_dangling_symlink_is_refused_and_creates_nothing_behind_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = secret_path(dir.path());
        prepare_socket_secret_parent(&path).expect("parent");
        let missing = dir.path().join("missing");
        symlink(&missing, &path).expect("symlink");

        assert!(create_socket_secret(&path).is_err());
        assert!(provision_socket_secret_locked(&path).is_err());

        assert!(!missing.exists());
    }

    // Guards `O_NOFOLLOW` on the lock file: a planted symlink must not redirect the lock database.
    #[test]
    fn a_symlinked_lock_file_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = secret_path(dir.path());
        prepare_socket_secret_parent(&path).expect("parent");
        let lock = path
            .parent()
            .expect("parent")
            .join("rpc.sock.secret.publish.lock");
        let elsewhere = dir.path().join("elsewhere");
        std::fs::write(&elsewhere, b"not a lock").expect("elsewhere");
        symlink(&elsewhere, &lock).expect("symlink");

        assert!(socket_secret_lock_path(&path).is_err());
        assert_eq!(std::fs::read(&elsewhere).expect("read"), b"not a lock");
    }

    // Create never replaces: an invalid entry is refused as it stands. Provisioning under the lock repairs it with one
    // rename, to a fresh 32-byte secret, and leaves no staging file.
    #[test]
    fn an_invalid_secret_is_refused_by_create_and_repaired_by_provisioning() {
        for invalid in [vec![1u8; 8], vec![1u8; SOCKET_SECRET_BYTES + 1], Vec::new()] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = secret_path(dir.path());
            prepare_socket_secret_parent(&path).expect("parent");
            std::fs::write(&path, &invalid).expect("invalid secret");

            assert!(create_socket_secret(&path).is_err());
            assert_eq!(std::fs::read(&path).expect("read"), invalid);

            let repaired = provision_socket_secret_locked(&path).expect("repair");
            assert_eq!(std::fs::read(&path).expect("read"), repaired.to_vec());
            assert_eq!(mode(&path), 0o600);
            assert!(staging_leftovers(&path).is_empty());
        }
    }

    #[test]
    fn publishing_and_repairing_leave_no_staging_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = secret_path(dir.path());
        create_socket_secret(&path).expect("create");
        assert!(staging_leftovers(&path).is_empty());

        std::fs::write(&path, [0u8; 3]).expect("invalid");
        provision_socket_secret_locked(&path).expect("repair");
        assert!(staging_leftovers(&path).is_empty());
    }
}
