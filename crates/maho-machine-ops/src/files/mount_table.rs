use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::timeout;

use crate::files::automount_trigger::{depth_within, resolved_segments_of};
use crate::node_path::{normalize, resolve};

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

pub(crate) async fn read_mount_view() -> MountView {
    let mounts = read_mount_table().await;
    let roots: Vec<String> = mounts
        .iter()
        .filter(|entry| entry.fs_type == "autofs")
        .map(|entry| entry.mount_point.clone())
        .collect();
    let case_insensitive = cfg!(target_os = "macos");
    let mut aliases = BTreeMap::new();
    if !roots.is_empty()
        && let Ok(mut entries) = tokio::fs::read_dir("/").await
    {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if !entry.file_type().await.is_ok_and(|ft| ft.is_symlink()) {
                continue;
            }
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            let alias = format!("/{name_str}");
            let Ok(link) = tokio::fs::read_link(&alias).await else {
                continue;
            };
            let target_path = if link.is_absolute() {
                normalize(&link)
            } else {
                resolve(Path::new("/"), &link)
            };
            let target = target_path.to_string_lossy().into_owned();
            let target_segments = resolved_segments_of(Path::new(&target));
            if roots
                .iter()
                .any(|root| depth_within(root, &target_segments, case_insensitive).is_some())
            {
                aliases.insert(alias, target);
            }
        }
    }
    MountView {
        mounts,
        aliases,
        case_insensitive,
    }
}
