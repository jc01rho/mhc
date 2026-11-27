//! The store connection's `-wal` and `-shm` files, watched for a foreign close.
//!
//! Upstream counterpart: `thread/gateway/connection-lost.ts` `watchWalFiles` (omo 7f068c4d32). While any connection is
//! open SQLite never removes the two files, so a missing or replaced one means another process took itself for the last
//! connection and deleted them on close: writes made after that land in an unlinked WAL, where nobody else reads them.
//! The facade opens a fresh connection before its next operation. A platform that gives files no inode identity (and
//! cannot unlink an open file) watches nothing. Upstream also maps an `SQLITE_IOERR` answer to the same recovery and runs
//! the call once more; a call here runs once and returns that error.

use std::path::{Path, PathBuf};

const SUFFIXES: [&str; 2] = ["-wal", "-shm"];

type FileIdentity = (u64, u64);

#[cfg(unix)]
fn file_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_path: &Path) -> Option<FileIdentity> {
    None
}

/// Remembers each file by device and inode the first time it is seen.
pub struct WalWatch {
    database: PathBuf,
    seen: [Option<FileIdentity>; 2],
}

impl WalWatch {
    pub fn new(database: PathBuf) -> Self {
        Self {
            database,
            seen: [None; 2],
        }
    }

    /// Names a file of the open connection that was removed or replaced since it was first seen; `None` while both
    /// are as they were. A file seen for the first time is remembered.
    pub fn lost(&mut self) -> Option<String> {
        for (index, suffix) in SUFFIXES.iter().enumerate() {
            let mut name = self.database.as_os_str().to_owned();
            name.push(suffix);
            let now = file_identity(Path::new(&name));
            match (self.seen[index], now) {
                (None, Some(identity)) => self.seen[index] = Some(identity),
                (Some(_), None) => {
                    return Some(format!(
                        "{suffix} was removed while the connection was open"
                    ));
                }
                (Some(known), Some(identity)) if known != identity => {
                    return Some(format!(
                        "{suffix} was replaced while the connection was open"
                    ));
                }
                _ => {}
            }
        }
        None
    }

    /// Forgets what was seen, for the files a fresh connection opens.
    pub fn reset(&mut self) {
        self.seen = [None; 2];
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn watch(dir: &tempfile::TempDir) -> (WalWatch, PathBuf, PathBuf) {
        let database = dir.path().join("gateway.sqlite");
        let wal = dir.path().join("gateway.sqlite-wal");
        let shm = dir.path().join("gateway.sqlite-shm");
        (WalWatch::new(database), wal, shm)
    }

    #[test]
    fn files_that_stay_put_are_never_lost_and_absent_files_are_not_watched() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (mut watch, wal, shm) = watch(&dir);
        assert_eq!(watch.lost(), None, "neither file exists yet");
        std::fs::write(&wal, "wal").expect("wal");
        std::fs::write(&shm, "shm").expect("shm");
        assert_eq!(watch.lost(), None, "first sight remembers");
        std::fs::write(&wal, "more wal").expect("the same file grows");
        assert_eq!(watch.lost(), None);
    }

    #[test]
    fn a_removed_or_replaced_file_is_named_until_the_watch_is_reset() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (mut watch, wal, shm) = watch(&dir);
        std::fs::write(&wal, "wal").expect("wal");
        std::fs::write(&shm, "shm").expect("shm");
        assert_eq!(watch.lost(), None);

        std::fs::remove_file(&shm).expect("remove the shm");
        assert_eq!(
            watch.lost().as_deref(),
            Some("-shm was removed while the connection was open")
        );
        watch.reset();
        assert_eq!(watch.lost(), None, "the wal is seen again, the shm is gone");

        // The replacement exists beside the original before it takes its name, so the two never share an inode.
        let other = dir.path().join("other");
        std::fs::write(&other, "wal").expect("another file");
        std::fs::rename(&other, &wal).expect("replace the wal");
        assert_eq!(
            watch.lost().as_deref(),
            Some("-wal was replaced while the connection was open")
        );
    }
}
