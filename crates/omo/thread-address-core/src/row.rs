//! Row identity and naming.
//!
//! Upstream counterpart: `row.ts`.

use std::collections::BTreeSet;

use crate::parse::format_address;

fn folder_name(folder: &str) -> &str {
    folder
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(folder)
}

/// What row identity reads of a listing row: its device, its thread id when it publishes one, its listing key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyedRow {
    pub device: String,
    pub durable_id: Option<String>,
    pub session_id: String,
}

fn published(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// One thread, one row: the thread id, or `<device id>/<listing key>` for a session that publishes none.
pub fn row_key(row: &KeyedRow) -> String {
    match published(row.durable_id.as_deref()) {
        Some(durable_id) => durable_id.to_owned(),
        None => format_address(&row.device, &row.session_id),
    }
}

/// The address a row is passed back as: `<device id>/<thread id>`, or its key when it has no thread id.
pub fn row_address(row: &KeyedRow) -> String {
    match published(row.durable_id.as_deref()) {
        Some(durable_id) => format_address(&row.device, durable_id),
        None => row_key(row),
    }
}

/// What naming reads of a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedRow {
    pub name: Option<String>,
    pub first_message: Option<String>,
    pub folder: String,
}

/// A row's name and whether it is only a fallback (`unnamed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowName {
    pub name: String,
    pub unnamed: bool,
}

/// The only naming function (design section 6): a session's own name, else a fallback that is the same on both legs
/// (the first user message when known, else the folder name) marked `unnamed`, because only a real name is an address.
pub fn row_name(row: &NamedRow) -> RowName {
    let name = row.name.as_deref().map(str::trim).unwrap_or_default();
    if !name.is_empty() {
        return RowName {
            name: name.to_owned(),
            unnamed: false,
        };
    }
    let message = row
        .first_message
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    RowName {
        name: if message.is_empty() {
            folder_name(&row.folder).to_owned()
        } else {
            message.to_owned()
        },
        unnamed: true,
    }
}

/// What merging reads of a row: its key and its device.
pub trait MergeRow {
    fn key(&self) -> &str;
    fn device(&self) -> &str;
}

/// This device's rows from its own endpoints, other devices' rows from the account listing (design section 6): the
/// listing's copy of this device's sessions is dropped, and so is a remote row whose key this device already lists.
pub fn merge_rows<R: MergeRow + Clone>(local: &[R], remote: &[R], here: Option<&str>) -> Vec<R> {
    let listed: BTreeSet<&str> = local.iter().map(MergeRow::key).collect();
    let mut merged = local.to_vec();
    merged.extend(
        remote
            .iter()
            .filter(|row| Some(row.device()) != here && !listed.contains(row.key()))
            .cloned(),
    );
    merged
}

/// The key unique for every listed session, whether or not it publishes a thread id: `<device id>/<listing key>`.
pub fn listing_key(device_id: &str, key: &str) -> String {
    format_address(device_id, key)
}

/// A session as two records name it: its device, its thread id when published, its listing key when known.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRef {
    pub device: String,
    pub thread: Option<String>,
    pub key: Option<String>,
}

/// Whether two records name the same session: the same device, and the same thread id when both carry one, else the
/// same listing key. The one copy of the "durable id or listing key on this device" match.
pub fn same_session(a: &SessionRef, b: &SessionRef) -> bool {
    if a.device != b.device {
        return false;
    }
    if let (Some(left), Some(right)) = (
        published(a.thread.as_deref()),
        published(b.thread.as_deref()),
    ) {
        return left == right;
    }
    matches!(
        (published(a.key.as_deref()), published(b.key.as_deref())),
        (Some(left), Some(right)) if left == right
    )
}
