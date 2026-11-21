//! Port of `guard-foreign-roots.ts` (omo #12476, #12301): whether a walk from above the memory area
//! could reach another identity. Each memory area is listed once. An identity root is a directory,
//! so a regular file (`.DS_Store`, `desktop.ini`) is skipped. Every other entry is own iff it is, or
//! contains, one of the caller's own roots; otherwise it is foreign whatever its type (directory,
//! symlink, junction, dangling link, FIFO). Entries are never resolved, so a link aliasing the own
//! root is still foreign. A missing area holds nothing; any other listing error fails closed.

use std::io;
use std::path::{Path, PathBuf};

use crate::policy_guard::within;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignRootScan {
    Clear,
    /// `area` holds an entry that is not the caller's own root. The entry itself is never reported.
    Foreign {
        area: PathBuf,
    },
    /// `area` could not be listed for a reason other than not existing, so it cannot be cleared.
    Unlisted {
        area: PathBuf,
    },
}

/// One directory entry as listed, typed by the listing itself (never resolved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaEntry {
    pub name: String,
    pub is_file: bool,
}

pub type ReadAreaEntries<'a> = &'a dyn Fn(&Path) -> io::Result<Vec<AreaEntry>>;

pub fn scan_foreign_roots(
    areas: &[PathBuf],
    own_roots: &[PathBuf],
    read_entries: ReadAreaEntries<'_>,
) -> ForeignRootScan {
    for area in areas {
        let entries = match read_entries(area) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return ForeignRootScan::Unlisted { area: area.clone() },
        };
        let foreign = entries.iter().any(|entry| {
            !entry.is_file
                && !own_roots
                    .iter()
                    .any(|own| within(&area.join(&entry.name), own))
        });
        if foreign {
            return ForeignRootScan::Foreign { area: area.clone() };
        }
    }
    ForeignRootScan::Clear
}

pub fn read_area_entries(dir: &Path) -> io::Result<Vec<AreaEntry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        entries.push(AreaEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_file: entry.file_type()?.is_file(),
        });
    }
    Ok(entries)
}

#[cfg(test)]
#[path = "guard_foreign_roots_tests.rs"]
mod tests;
