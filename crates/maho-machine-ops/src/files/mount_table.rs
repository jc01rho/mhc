use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::timeout;
use unicode_normalization::UnicodeNormalization;

const MOUNT_TABLE_TIMEOUT_MS: u64 = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    pub mount_point: String,
    pub fs_type: String,
}

impl MountEntry {
    pub fn new(mount_point: impl Into<String>, fs_type: impl Into<String>) -> Self {
        Self {
            mount_point: mount_point.into(),
            fs_type: fs_type.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MountView {
    pub mounts: Vec<MountEntry>,
    pub aliases: BTreeMap<String, String>,
    pub case_insensitive: bool,
}

impl MountView {
    pub fn new(mounts: Vec<MountEntry>, case_insensitive: bool) -> Self {
        Self {
            mounts,
            aliases: BTreeMap::new(),
            case_insensitive,
        }
    }
}

fn unescape_octal(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 4 <= bytes.len()
            && bytes[index + 1..index + 4]
                .iter()
                .all(|byte| matches!(byte, b'0'..=b'7'))
        {
            let value = u32::from(bytes[index + 1] - b'0') * 64
                + u32::from(bytes[index + 2] - b'0') * 8
                + u32::from(bytes[index + 3] - b'0');
            if let Some(ch) = char::from_u32(value) {
                out.push(ch);
                index += 4;
                continue;
            }
        }
        if let Some(ch) = text[index..].chars().next() {
            out.push(ch);
            index += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

/// `/proc/self/mountinfo`: field 5 is the mount point (octal-escaped), the first field after ` - ` is the type.
pub fn parse_linux_mount_info(text: &str) -> Vec<MountEntry> {
    let mut entries = Vec::new();
    for line in text.split('\n') {
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        let Some(mount_point) = before.split(' ').nth(4) else {
            continue;
        };
        let Some(fs_type) = after.split(' ').next() else {
            continue;
        };
        if mount_point.is_empty() || fs_type.is_empty() {
            continue;
        }
        entries.push(MountEntry::new(unescape_octal(mount_point), fs_type));
    }
    entries
}

/// macOS `mount` output: `<source> on <mount point> (<type>, <options>)`.
pub fn parse_darwin_mount_table(text: &str) -> Vec<MountEntry> {
    let mut entries = Vec::new();
    for line in text.split('\n') {
        let Some(on) = line.find(" on /") else {
            continue;
        };
        let Some(options) = line.rfind(" (") else {
            continue;
        };
        if options <= on + 4 {
            continue;
        }
        let fs_type = line[options + 2..]
            .split([',', ')'])
            .next()
            .map(str::trim)
            .unwrap_or("");
        if fs_type.is_empty() {
            continue;
        }
        entries.push(MountEntry::new(&line[on + 4..options], fs_type));
    }
    entries
}

fn spawn_darwin_mount_query(tx: oneshot::Sender<Vec<MountEntry>>) {
    std::thread::spawn(move || {
        let entries = Command::new("/sbin/mount")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| parse_darwin_mount_table(&String::from_utf8_lossy(&output.stdout)))
            .unwrap_or_default();
        if tx.send(entries).is_err() {}
    });
}

pub async fn read_mount_table() -> Vec<MountEntry> {
    if cfg!(target_os = "linux") {
        return tokio::fs::read_to_string("/proc/self/mountinfo")
            .await
            .map(|text| parse_linux_mount_info(&text))
            .unwrap_or_default();
    }
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let (tx, rx) = oneshot::channel();
    spawn_darwin_mount_query(tx);
    timeout(Duration::from_millis(MOUNT_TABLE_TIMEOUT_MS), rx)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
}

fn raw_segments_of(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).filter(|s| !s.is_empty()).collect()
}

pub(crate) fn resolved_segments_of(path: &Path) -> Vec<String> {
    let text = path.to_string_lossy();
    let full = if text.starts_with('/') || path.is_absolute() {
        text.into_owned()
    } else if let Ok(cwd) = std::env::current_dir() {
        format!("{}/{text}", cwd.to_string_lossy())
    } else {
        text.into_owned()
    };
    let mut out = Vec::new();
    for segment in raw_segments_of(&full) {
        match segment {
            "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other.to_owned()),
        }
    }
    out
}

fn fold_segment(segment: &str, case_insensitive: bool) -> Cow<'_, str> {
    if case_insensitive {
        Cow::Owned(segment.nfc().collect::<String>().to_lowercase())
    } else {
        Cow::Borrowed(segment)
    }
}

pub(crate) fn depth_within(root: &str, path: &[String], case_insensitive: bool) -> Option<usize> {
    let root_segments = raw_segments_of(root);
    if root_segments.len() > path.len() {
        return None;
    }
    let matches_prefix = root_segments.iter().enumerate().all(|(index, segment)| {
        fold_segment(segment, case_insensitive) == fold_segment(&path[index], case_insensitive)
    });
    matches_prefix.then_some(root_segments.len())
}

fn through_alias(path: Vec<String>, view: &MountView) -> Vec<String> {
    for (alias, target) in &view.aliases {
        if let Some(depth) = depth_within(alias, &path, view.case_insensitive) {
            let mut mapped: Vec<String> = raw_segments_of(target)
                .into_iter()
                .map(str::to_owned)
                .collect();
            mapped.extend_from_slice(&path[depth..]);
            return mapped;
        }
    }
    path
}

pub fn unmounted_automount_path(path: impl AsRef<Path>, view: &MountView) -> Option<String> {
    let mapped = through_alias(resolved_segments_of(path.as_ref()), view);
    let mut owners: Vec<&MountEntry> = Vec::new();
    let mut owner_depth: Option<usize> = None;
    for entry in &view.mounts {
        let Some(depth) = depth_within(&entry.mount_point, &mapped, view.case_insensitive) else {
            continue;
        };
        if owner_depth.is_some_and(|current| depth < current) {
            continue;
        }
        if owner_depth != Some(depth) {
            owners.clear();
        }
        owner_depth = Some(depth);
        owners.push(entry);
    }
    let owner = *owners.first()?;
    if owners.iter().any(|entry| entry.fs_type != "autofs") {
        return None;
    }
    let depth = owner_depth?;
    if depth >= mapped.len() {
        return Some(owner.mount_point.clone());
    }
    let prefix = owner.mount_point.trim_end_matches('/');
    Some(format!("{prefix}/{}", mapped[depth..].join("/")))
}
