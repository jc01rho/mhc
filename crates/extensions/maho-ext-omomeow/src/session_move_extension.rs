//! Port of omo-nyang `src/session-move-extension.ts`: carries a task's attachments and lane state
//! through an engine session move, and the move-readiness RPC agentd polls before moving.

use maho_ext_api::{
    AbortSignal, BeforeAgentStartEventResult, EventKind, EventResult, ExtensionApi, ExtensionEvent,
    ExtensionFailure,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub type Environment = BTreeMap<String, String>;

pub fn process_environment() -> Environment {
    std::env::vars().collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Attachment {
    pub name: String,
    pub source_paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: u8,
    pub task_id: String,
    pub lane_state: Option<Map<String, Value>>,
    pub attachments: Vec<Attachment>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoredAttachment {
    pub name: String,
    pub source_paths: Vec<String>,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub version: u8,
    pub task_id: String,
    pub lane_state: Option<Map<String, Value>>,
    pub attachments: Vec<RestoredAttachment>,
}

fn failure(message: impl Into<String>) -> ExtensionFailure {
    ExtensionFailure::new(message)
}

fn valid_name(name: &str) -> Result<(), ExtensionFailure> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Err(failure(format!("Invalid attachment name: {name:?}")));
    }
    Ok(())
}

fn valid_task(task: &str) -> Result<(), ExtensionFailure> {
    let mut chars = task.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
        return Err(failure(format!("Invalid task id: {task:?}")));
    }
    Ok(())
}

fn validate_manifest(manifest: &Manifest) -> Result<(), ExtensionFailure> {
    if manifest.version != 1 {
        return Err(failure("Unsupported move manifest version"));
    }
    valid_task(&manifest.task_id)?;
    manifest.attachments.iter().try_for_each(|a| valid_name(&a.name))
}

pub fn snapshot_path(session_file: &Path) -> PathBuf {
    PathBuf::from(format!("{}.omomeow-move.json", session_file.display()))
}

fn optional_json(path: &Path) -> Result<Option<Value>, ExtensionFailure> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| failure(format!("{}: {error}", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(failure(format!("{}: {error}", path.display()))),
    }
}

pub fn snapshot(session_file: Option<&Path>) -> Result<Option<Snapshot>, ExtensionFailure> {
    let Some(session_file) = session_file else {
        return Ok(None);
    };
    let Some(value) = optional_json(&snapshot_path(session_file))? else {
        return Ok(None);
    };
    let restored: Snapshot = serde_json::from_value(value).map_err(|error| failure(error.to_string()))?;
    valid_task(&restored.task_id)?;
    Ok(Some(restored))
}

fn parent_is_tasks(session_file: &Path) -> bool {
    session_file
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "tasks")
}

/// The hub's OMOMEOW_HOME is its checkout, whereas a remote worker uses its state home. Imported
/// sessions live in the engine's session tree, so launchers supply the explicit state root.
fn state_directory(session_file: Option<&Path>, env: &Environment) -> Result<PathBuf, ExtensionFailure> {
    let from_session = session_file
        .filter(|file| parent_is_tasks(file))
        .and_then(|file| file.parent()?.parent().map(Path::to_path_buf));
    let directory = env
        .get("OMOMEOW_STATE_DIRECTORY")
        .map(PathBuf::from)
        .or(from_session)
        .or_else(|| env.get("OMOMEOW_HOME").map(PathBuf::from))
        .ok_or_else(|| failure("OMOMEOW_STATE_DIRECTORY is required for an imported task session"))?;
    Ok(std::path::absolute(&directory).unwrap_or(directory))
}

fn check(signal: &AbortSignal) -> Result<(), ExtensionFailure> {
    if signal.is_aborted() {
        return Err(failure("Session move aborted"));
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8], signal: &AbortSignal) -> Result<(), ExtensionFailure> {
    use std::io::Write;
    check(signal)?;
    let io = |error: std::io::Error| failure(format!("{}: {error}", path.display()));
    if let Some(parent) = path.parent() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(parent).map_err(io)?;
    }
    let temporary = PathBuf::from(format!("{}.{}.tmp", path.display(), uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(&temporary)?.write_all(bytes)?;
        Ok::<(), std::io::Error>(())
    })()
    .map_err(io)
    .and_then(|()| check(signal))
    .and_then(|()| std::fs::rename(&temporary, path).map_err(io));
    let _ = std::fs::remove_file(&temporary);
    result
}

fn copy_attachment(source: &Path, target: &Path, signal: &AbortSignal) -> Result<(), ExtensionFailure> {
    check(signal)?;
    let metadata = std::fs::symlink_metadata(source).map_err(|e| failure(format!("{}: {e}", source.display())))?;
    if !metadata.is_file() {
        return Err(failure(format!("Attachment is not a regular file: {}", source.display())));
    }
    let bytes = std::fs::read(source).map_err(|e| failure(format!("{}: {e}", source.display())))?;
    write_atomic(target, &bytes, signal)
}

pub fn export_session_move(
    dir: &Path,
    signal: &AbortSignal,
    session_file: Option<&Path>,
    env: &Environment,
) -> Result<(), ExtensionFailure> {
    check(signal)?;
    let previous = snapshot(session_file)?;
    let inferred = session_file
        .filter(|file| parent_is_tasks(file) && file.extension().is_some_and(|e| e == "jsonl"))
        .and_then(|file| file.file_stem()?.to_str().map(str::to_owned));
    let Some(task_id) = env
        .get("OMOMEOW_TASK_ID")
        .cloned()
        .or_else(|| previous.as_ref().map(|p| p.task_id.clone()))
        .or(inferred)
    else {
        return Ok(());
    };
    valid_task(&task_id)?;
    if previous.as_ref().is_some_and(|p| p.task_id != task_id) {
        return Err(failure("Launch task id does not match imported task"));
    }
    let lane_state = match env.get("OMOMEOW_LANE_STATE") {
        None => previous.as_ref().and_then(|p| p.lane_state.clone()),
        Some(text) => match serde_json::from_str::<Value>(text).map_err(|e| failure(e.to_string()))? {
            Value::Null => None,
            Value::Object(object) => Some(object),
            _ => return Err(failure("OMOMEOW_LANE_STATE must be an object or null")),
        },
    };
    let directory = state_directory(session_file, env)?.join("attachments").join(&task_id);
    let staged = dir.join("omo-nyang");
    // Accumulate only files copied successfully; publishing the manifest is the last export write.
    let mut attachments = Vec::new();
    match std::fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(failure(format!("{}: {error}", directory.display()))),
        Ok(metadata) if !metadata.is_dir() => {
            return Err(failure(format!("Attachment directory is not a directory: {}", directory.display())));
        }
        Ok(_) => {
            let mut names = std::fs::read_dir(&directory)
                .map_err(|e| failure(e.to_string()))?
                .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| failure(e.to_string()))?;
            names.sort();
            for name in names {
                valid_name(&name)?;
                let source = directory.join(&name);
                copy_attachment(&source, &staged.join("attachments").join(&name), signal)?;
                let mut source_paths = previous
                    .as_ref()
                    .and_then(|p| p.attachments.iter().find(|a| a.name == name))
                    .map(|a| a.source_paths.clone())
                    .unwrap_or_default();
                let source = source.to_string_lossy().into_owned();
                if !source_paths.contains(&source) {
                    source_paths.push(source);
                }
                attachments.push(Attachment { name, source_paths });
            }
        }
    }
    let manifest = Manifest { version: 1, task_id, lane_state, attachments };
    let bytes = serde_json::to_vec(&manifest).map_err(|e| failure(e.to_string()))?;
    write_atomic(&staged.join("manifest.json"), &bytes, signal)
}

pub fn import_session_move(
    dir: &Path,
    signal: &AbortSignal,
    session_file: Option<&Path>,
    env: &Environment,
) -> Result<(), ExtensionFailure> {
    check(signal)?;
    let staged = dir.join("omo-nyang");
    let Some(value) = optional_json(&staged.join("manifest.json"))? else {
        return Ok(());
    };
    let manifest: Manifest = serde_json::from_value(value).map_err(|e| failure(e.to_string()))?;
    validate_manifest(&manifest)?;
    let session_file =
        session_file.ok_or_else(|| failure("A persistent session file is required to restore task state"))?;
    if env.get("OMOMEOW_TASK_ID").is_some_and(|id| !id.is_empty() && *id != manifest.task_id) {
        return Err(failure("Launch task id does not match moved task"));
    }
    let directory = state_directory(Some(session_file), env)?
        .join("attachments")
        .join(&manifest.task_id);
    let attachments = manifest
        .attachments
        .iter()
        .map(|a| RestoredAttachment {
            name: a.name.clone(),
            source_paths: a.source_paths.clone(),
            path: directory.join(&a.name).to_string_lossy().into_owned(),
        })
        .collect::<Vec<_>>();
    for attachment in &attachments {
        copy_attachment(&staged.join("attachments").join(&attachment.name), Path::new(&attachment.path), signal)?;
    }
    // Repeating these replacements is intentional: a failed delivery can have restored only some
    // bytes. This sidecar is also the durable identity on an engine-selected session path.
    let restored = Snapshot {
        version: manifest.version,
        task_id: manifest.task_id,
        lane_state: manifest.lane_state,
        attachments,
    };
    let bytes = serde_json::to_vec(&restored).map_err(|e| failure(e.to_string()))?;
    write_atomic(&snapshot_path(session_file), &bytes, signal)
}

pub fn moved_state_prompt(system_prompt: &str, restored: &Snapshot) -> String {
    let state = serde_json::to_string(restored).unwrap_or_default();
    format!("{system_prompt}\n\nOmOMeow moved task state (attachment sourcePaths now resolve to path):\n{state}")
}

pub fn register_session_move_extension(api: &mut ExtensionApi, env: Arc<Environment>) {
    let export_env = Arc::clone(&env);
    api.on(
        EventKind::SessionMoveExport,
        Arc::new(move |event, ctx| {
            let env = Arc::clone(&export_env);
            Box::pin(async move {
                if let ExtensionEvent::SessionMoveExport { dir, signal, .. } = event {
                    export_session_move(dir, signal, ctx.session_manager.session_file(), &env)?;
                }
                Ok(EventResult::None)
            })
        }),
    );
    api.on(
        EventKind::SessionMoveImport,
        Arc::new(move |event, ctx| {
            let env = Arc::clone(&env);
            Box::pin(async move {
                if let ExtensionEvent::SessionMoveImport { dir, signal, .. } = event {
                    import_session_move(dir, signal, ctx.session_manager.session_file(), &env)?;
                }
                Ok(EventResult::None)
            })
        }),
    );
    api.on(
        EventKind::BeforeAgentStart,
        Arc::new(|event, ctx| {
            Box::pin(async move {
                let ExtensionEvent::BeforeAgentStart(start) = event else {
                    return Ok(EventResult::None);
                };
                // Historical messages keep their original paths; give every resumed turn the current
                // paths without rewriting transcript entries or appending a duplicate message.
                Ok(match snapshot(ctx.session_manager.session_file())? {
                    Some(restored) => EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                        system_prompt: Some(moved_state_prompt(&start.system_prompt, &restored)),
                        ..Default::default()
                    }),
                    None => EventResult::None,
                })
            })
        }),
    );
}

/// Sources with live extension-owned activity, tracked from the engine's full per-source
/// `wake_source_state` snapshots so the RPC also covers work active before agentd reconnected.
#[derive(Default)]
pub struct ActiveSources(Mutex<Vec<String>>);

impl ActiveSources {
    pub fn observe(&self, value: &Value) {
        let (Some(source), Some(count)) = (
            value.get("source").and_then(Value::as_str),
            value.get("activeCount").and_then(Value::as_f64),
        ) else {
            return;
        };
        let mut active = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if count > 0.0 {
            if !active.iter().any(|s| s == source) {
                active.push(source.to_owned());
            }
        } else {
            active.retain(|s| s != source);
        }
    }

    pub fn readiness(&self) -> Value {
        json!({ "activeSources": *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) })
    }
}

pub fn register_move_readiness(api: &mut ExtensionApi) -> Result<(), ExtensionFailure> {
    let active = Arc::new(ActiveSources::default());
    let observer = Arc::clone(&active);
    let subscription = api
        .events
        .on("wake_source_state", Arc::new(move |value| observer.observe(value)));
    let _release = api.runtime.track_subscription(Box::new(move || drop(subscription)));
    api.rpc_handle(
        "omomeow.move-readiness",
        Arc::new(move |_| {
            let readiness = active.readiness();
            Box::pin(async move { Ok(readiness) })
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omomeow-move-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn env(pairs: &[(&str, &str)]) -> Environment {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn export_then_import_moves_attachments_and_lane_state() {
        let root = scratch("roundtrip");
        let source_state = root.join("src-state");
        let session = source_state.join("tasks").join("task-1.jsonl");
        std::fs::create_dir_all(source_state.join("attachments/task-1")).unwrap();
        std::fs::create_dir_all(source_state.join("tasks")).unwrap();
        std::fs::write(source_state.join("attachments/task-1/a.txt"), b"alpha").unwrap();
        std::fs::write(&session, b"").unwrap();
        let move_dir = root.join("move");
        let signal = AbortSignal::default();
        export_session_move(&move_dir, &signal, Some(&session), &env(&[("OMOMEOW_LANE_STATE", r#"{"lane":3}"#)])).unwrap();
        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(move_dir.join("omo-nyang/manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest.task_id, "task-1");
        assert_eq!(manifest.lane_state.unwrap()["lane"], 3);
        assert_eq!(manifest.attachments[0].name, "a.txt");

        let target_state = root.join("dst-state");
        let target_session = root.join("engine/s.jsonl");
        std::fs::create_dir_all(target_session.parent().unwrap()).unwrap();
        let target_env = env(&[("OMOMEOW_STATE_DIRECTORY", target_state.to_str().unwrap())]);
        import_session_move(&move_dir, &signal, Some(&target_session), &target_env).unwrap();
        assert_eq!(std::fs::read(target_state.join("attachments/task-1/a.txt")).unwrap(), b"alpha");
        let restored = snapshot(Some(&target_session)).unwrap().unwrap();
        assert_eq!(restored.attachments[0].path, target_state.join("attachments/task-1/a.txt").to_string_lossy());
        assert!(moved_state_prompt("BASE", &restored).starts_with("BASE\n\nOmOMeow moved task state"));
    }

    #[test]
    fn import_refuses_a_different_task_and_bad_names() {
        let root = scratch("refuse");
        std::fs::create_dir_all(root.join("move/omo-nyang")).unwrap();
        std::fs::write(
            root.join("move/omo-nyang/manifest.json"),
            r#"{"version":1,"taskId":"t1","laneState":null,"attachments":[]}"#,
        )
        .unwrap();
        let signal = AbortSignal::default();
        let session = root.join("s.jsonl");
        let error = import_session_move(&root.join("move"), &signal, Some(&session), &env(&[("OMOMEOW_TASK_ID", "t2"), ("OMOMEOW_STATE_DIRECTORY", "/x")]))
            .unwrap_err();
        assert!(error.message.contains("does not match moved task"));
        std::fs::write(
            root.join("move/omo-nyang/manifest.json"),
            r#"{"version":1,"taskId":"t1","laneState":null,"attachments":[{"name":"../x","sourcePaths":[]}]}"#,
        )
        .unwrap();
        assert!(import_session_move(&root.join("move"), &signal, Some(&session), &env(&[("OMOMEOW_STATE_DIRECTORY", "/x")])).is_err());
    }

    #[test]
    fn export_without_identity_writes_nothing() {
        let root = scratch("noid");
        let signal = AbortSignal::default();
        export_session_move(&root.join("move"), &signal, Some(&root.join("other/s.jsonl")), &env(&[])).unwrap();
        assert!(!root.join("move/omo-nyang/manifest.json").exists());
    }

    #[test]
    fn readiness_tracks_active_sources() {
        let active = ActiveSources::default();
        active.observe(&json!({"source":"monitor","activeCount":2}));
        active.observe(&json!({"source":"ask-user","activeCount":1}));
        active.observe(&json!({"source":"monitor","activeCount":0}));
        active.observe(&json!({"bad":true}));
        assert_eq!(active.readiness(), json!({"activeSources":["ask-user"]}));
    }
}
