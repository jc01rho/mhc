use super::bundle::{
    MANIFEST_FILE, MoveBundleInvalidError, leaves_bundle, listed_files, parse_manifest,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineBundleFile {
    pub path: String,
    pub sha256: String,
    pub base64: String,
}

const STAGING_PREFIX: &str = "inline-";
const STALE_STAGING_MS: u64 = 60 * 60_000;
const MAX_SEGMENT_BYTES: usize = 255;
const MAX_PATH_BYTES: usize = 1024;

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(bytes);
    hex::encode(hash.finalize())
}

pub async fn read_inline_bundle(
    bundle_dir: &Path,
    max_bytes: usize,
    max_files: usize,
) -> Result<Option<Vec<InlineBundleFile>>, String> {
    let found_paths = super::files_under(bundle_dir).await?;
    if found_paths.len() > max_files {
        return Ok(None);
    }
    let mut rel_paths = Vec::with_capacity(found_paths.len());
    for file in found_paths {
        let rel = file
            .strip_prefix(bundle_dir)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        if rel != MANIFEST_FILE {
            rel_paths.push(rel);
        }
    }
    rel_paths.sort();
    let mut paths = Vec::with_capacity(1 + rel_paths.len());
    paths.push(MANIFEST_FILE.to_owned());
    paths.extend(rel_paths);

    let mut total_bytes = 0usize;
    for rel in &paths {
        let meta = tokio::fs::metadata(bundle_dir.join(rel))
            .await
            .map_err(|e| e.to_string())?;
        total_bytes = total_bytes.saturating_add(meta.len() as usize);
        if total_bytes > max_bytes {
            return Ok(None);
        }
    }

    let mut files = Vec::with_capacity(paths.len());
    for rel in paths {
        let content = tokio::fs::read(bundle_dir.join(&rel))
            .await
            .map_err(|e| e.to_string())?;
        let digest = sha256_bytes(&content);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&content);
        files.push(InlineBundleFile {
            path: rel,
            sha256: digest,
            base64: encoded,
        });
    }
    Ok(Some(files))
}

fn segments_of(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).collect()
}

fn shown(path: &str) -> String {
    let truncated = if path.len() > 120 {
        format!("{}...", &path[..120])
    } else {
        path.to_owned()
    };
    serde_json::to_string(&truncated).unwrap_or_else(|_| format!("\"{truncated}\""))
}

fn stays_in_bundle(path: &str) -> bool {
    if leaves_bundle(path) || path.contains('\0') || path.len() > MAX_PATH_BYTES {
        return false;
    }
    segments_of(path)
        .into_iter()
        .all(|segment| !segment.is_empty() && segment != "." && segment.len() <= MAX_SEGMENT_BYTES)
}

fn refuse_file_as_folder(sent: &HashMap<String, String>) -> Result<(), MoveBundleInvalidError> {
    let files: HashSet<String> = sent
        .keys()
        .map(|path| segments_of(path).join("/"))
        .collect();
    for path in &files {
        let parts: Vec<&str> = path.split('/').collect();
        let mut folder = String::new();
        for segment in parts.iter().take(parts.len().saturating_sub(1)) {
            if folder.is_empty() {
                folder = (*segment).to_owned();
            } else {
                folder = format!("{folder}/{segment}");
            }
            if files.contains(&folder) {
                return Err(MoveBundleInvalidError::new(format!(
                    "{} was sent as a file and as a folder",
                    shown(&folder)
                )));
            }
        }
    }
    Ok(())
}

pub fn sent_checksums(
    files: &[InlineBundleFile],
) -> Result<HashMap<String, String>, MoveBundleInvalidError> {
    let mut sent = HashMap::with_capacity(files.len());
    for file in files {
        if !stays_in_bundle(&file.path) {
            return Err(MoveBundleInvalidError::new(format!(
                "a sent path cannot be a file inside the bundle: {}",
                shown(&file.path)
            )));
        }
        if sent.contains_key(&file.path) {
            return Err(MoveBundleInvalidError::new(format!(
                "{} was sent twice",
                shown(&file.path)
            )));
        }
        sent.insert(file.path.clone(), file.sha256.clone());
    }
    let Some(manifest_file) = files.iter().find(|file| file.path == MANIFEST_FILE) else {
        return Err(MoveBundleInvalidError::new("no manifest was sent"));
    };
    let manifest_bytes = base64::engine::general_purpose::STANDARD
        .decode(&manifest_file.base64)
        .map_err(|_| MoveBundleInvalidError::new("the manifest changed in transit"))?;
    if sha256_bytes(&manifest_bytes) != manifest_file.sha256 {
        return Err(MoveBundleInvalidError::new(
            "the manifest changed in transit",
        ));
    }
    let manifest_text = String::from_utf8(manifest_bytes)
        .map_err(|e| MoveBundleInvalidError::new(format!("manifest unreadable: {e}")))?;
    let parsed_manifest = parse_manifest(&manifest_text)?;
    let mut listed: HashSet<String> = HashSet::new();
    listed.insert(MANIFEST_FILE.to_owned());
    for file in listed_files(&parsed_manifest) {
        listed.insert(file.path);
    }
    for file in files {
        if !listed.contains(&file.path) {
            return Err(MoveBundleInvalidError::new(format!(
                "{} was sent but the manifest does not list it",
                shown(&file.path)
            )));
        }
    }
    refuse_file_as_folder(&sent)?;
    Ok(sent)
}

async fn remove_stale_staging(incoming: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(incoming).await else {
        return;
    };
    let cutoff = Duration::from_millis(STALE_STAGING_MS);
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(STAGING_PREFIX) {
            continue;
        }
        let path = entry.path();
        let is_stale = tokio::fs::metadata(&path)
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
            .is_some_and(|age| age > cutoff);
        if is_stale {
            let _ = super::remove(&path).await;
        }
    }
}

pub async fn with_staged_inline_bundle<T, F, Fut>(
    files: &[InlineBundleFile],
    incoming: &Path,
    use_bundle: F,
) -> Result<T, String>
where
    F: FnOnce(PathBuf, HashMap<String, String>) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let sent =
        sent_checksums(files).map_err(|e| format!("{}: {e}", MoveBundleInvalidError::CODE))?;
    tokio::fs::create_dir_all(incoming)
        .await
        .map_err(|e| e.to_string())?;
    remove_stale_staging(incoming).await;
    let dir = incoming.join(format!("{STAGING_PREFIX}{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let write_res: Result<(), String> = async {
        for file in files {
            let mut to = dir.clone();
            for seg in file.path.split('/') {
                to.push(seg);
            }
            if let Some(parent) = to.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&file.base64)
                .map_err(|e| e.to_string())?;
            tokio::fs::write(&to, bytes)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    .await;
    if let Err(err) = write_res {
        let _ = super::remove(&dir).await;
        return Err(err);
    }
    let result = use_bundle(dir.clone(), sent).await;
    let _ = super::remove(&dir).await;
    result
}
