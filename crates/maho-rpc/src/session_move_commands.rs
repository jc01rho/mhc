//! RPC move transaction dispatch. File custody stays in the engine ledger.
use crate::session_move_wire::{MOVE_INLINE_MAX_BYTES, MOVE_INLINE_MAX_FILES};
use base64::Engine as _;
use maho_core::session_move::{
    bundle::{self, RepositoryIdentity},
    import::{StatusFile, StatusFileKind},
    inline_bundle::InlineBundleFile,
    landed_file::LandedFile,
    move_ledger::{DeclineReason, MoveLedger, MoveRecord, MoveStatus},
    ownership::{RefusedState, SessionOwnership, session_ownership},
    prepare::{PrepareMoveOptions, prepare_move},
    repository_check::read_repository_identity,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn wire(record: MoveRecord) -> Value {
    match record {
        MoveRecord::Ready { expires_at } => json!({"state":"ready","expires_at":expires_at}),
        MoveRecord::Status(status) => status_wire(status),
    }
}

fn status_wire(status: MoveStatus) -> Value {
    match status {
        MoveStatus::Receiving => json!({"state":"receiving"}),
        MoveStatus::AcceptedRevoked => json!({"state":"accepted","revoked":true}),
        MoveStatus::Accepted => json!({"state":"accepted"}),
        MoveStatus::Declined { reason } => json!({"state":"declined","reason":reason.as_str()}),
        MoveStatus::Failed { reason } => json!({"state":"failed","reason":reason.as_str()}),
    }
}

fn wire_landed(landed: Option<LandedFile>) -> Option<Value> {
    match landed? {
        LandedFile::Gone => Some(json!({"landed_file":"gone"})),
        LandedFile::File {
            session_path,
            session_id,
            cwd,
        } => Some(json!({
            "session_path": session_path,
            "session_id": session_id,
            "cwd": cwd,
        })),
    }
}

fn merge_landed(mut base: Value, landed: Option<Value>) -> Value {
    if let (Some(base_obj), Some(Value::Object(extra))) = (base.as_object_mut(), landed) {
        for (k, v) in extra {
            base_obj.insert(k, v);
        }
    }
    base
}

fn refusal_with_extra(command: &Value, code: &str, detail: &str, extra: Option<Value>) -> Value {
    let error_data = merge_landed(json!({"detail": detail}), extra);
    json!({"id":command["id"],"type":"response","command":"move_session","success":false,"error":code,"errorCode":code,"errorData":error_data})
}

fn is_padded_base64(value: &str) -> bool {
    if value.len() % 4 != 0 {
        return false;
    }
    let bytes = value.as_bytes();
    let pad = bytes
        .iter()
        .rev()
        .take(2)
        .take_while(|&&b| b == b'=')
        .count();
    bytes[..bytes.len() - pad]
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

fn well_shaped_repository(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let Some(roots) = obj.get("rootCommits").and_then(Value::as_array) else {
        return false;
    };
    if !roots.iter().all(Value::is_string) {
        return false;
    }
    matches!(
        obj.get("originUrl"),
        None | Some(Value::Null) | Some(Value::String(_))
    )
}

fn parse_wire_repository(value: &Value) -> Option<RepositoryIdentity> {
    bundle::parse_repository_identity(value)
}

fn parse_inline_files(value: &Value) -> Result<Vec<InlineBundleFile>, String> {
    let Some(items) = value.as_array() else {
        return Err(format!(
            "files must be a list of at most {MOVE_INLINE_MAX_FILES} {{ path, sha256, base64 }} entries."
        ));
    };
    if items.len() > MOVE_INLINE_MAX_FILES {
        return Err(format!(
            "files must be a list of at most {MOVE_INLINE_MAX_FILES} {{ path, sha256, base64 }} entries."
        ));
    }
    let mut files = Vec::with_capacity(items.len());
    let mut total_bytes = 0usize;
    for item in items {
        let Some(obj) = item.as_object() else {
            return Err("each file must be an object with path, sha256 and base64.".into());
        };
        let (Some(path), Some(sha256), Some(base64_str)) = (
            obj.get("path").and_then(Value::as_str),
            obj.get("sha256").and_then(Value::as_str),
            obj.get("base64").and_then(Value::as_str),
        ) else {
            return Err(
                "each file needs path and sha256 as strings and base64 as padded base64.".into(),
            );
        };
        let pad = base64_str
            .as_bytes()
            .iter()
            .rev()
            .take(2)
            .take_while(|&&b| b == b'=')
            .count();
        let approx_decoded = (base64_str.len() / 4) * 3 - pad.min((base64_str.len() / 4) * 3);
        total_bytes = total_bytes.saturating_add(approx_decoded);
        if total_bytes > MOVE_INLINE_MAX_BYTES {
            return Err(format!(
                "files hold more than the {MOVE_INLINE_MAX_BYTES} bytes an inline import takes; send the bundle folder instead."
            ));
        }
        if !is_padded_base64(base64_str)
            || base64::engine::general_purpose::STANDARD
                .decode(base64_str)
                .is_err()
        {
            return Err(
                "each file needs path and sha256 as strings and base64 as padded base64.".into(),
            );
        }
        files.push(InlineBundleFile {
            path: path.to_owned(),
            sha256: sha256.to_owned(),
            base64: base64_str.to_owned(),
        });
    }
    Ok(files)
}

const OPEN_FIELDS_THE_IMPORT_SETS: &[&str] = &["id", "type", "sessionPath", "cwd", "admissionId"];

pub fn validate_move_request(command: &Value) -> Result<(), String> {
    let op = command["op"].as_str().unwrap_or_default();
    if op == "import" {
        let has_files = command.get("files").is_some();
        let has_dir = command.get("bundleDir").is_some();
        if has_files && has_dir {
            return Err("import takes bundleDir or files, not both.".into());
        }
        if has_files {
            parse_inline_files(&command["files"])?;
            let cwd = text(command, "cwd")?;
            if !Path::new(cwd).has_root() {
                return Err("import with files needs cwd as an absolute path.".into());
            }
        } else {
            let dir = text(command, "bundleDir")?;
            let cwd = text(command, "cwd")?;
            if !Path::new(dir).has_root() || !Path::new(cwd).has_root() {
                return Err("import needs bundleDir and cwd as absolute paths.".into());
            }
        }
        if command.get("from").is_some_and(|value| !value.is_string()) {
            return Err("from must be a string.".into());
        }
        if let Some(sdir) = command.get("sessionDir") {
            let sdir_str = sdir
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("sessionDir must be an absolute path.")?;
            if !Path::new(sdir_str).has_root() {
                return Err("sessionDir must be an absolute path.".into());
            }
        }
        if let Some(open) = command.get("open") {
            let valid = open.as_bool() == Some(true)
                || open.as_object().is_some_and(|obj| {
                    OPEN_FIELDS_THE_IMPORT_SETS
                        .iter()
                        .all(|field| !obj.contains_key(*field))
                });
            if !valid {
                return Err(
                    "open must be true or open_session fields other than id, type, sessionPath, cwd and admissionId.".into(),
                );
            }
        }
        return Ok(());
    }
    if op == "identity" {
        let sp = command
            .get("sessionPath")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let cwd = command
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        if sp.is_some() && cwd.is_some() {
            return Err(
                "identity takes sessionPath, a session file of this device, or cwd, a folder, never both.".into(),
            );
        }
        if sp.is_some() {
            return Ok(());
        }
        let Some(cwd) = cwd else {
            return Err(
                "identity needs sessionPath, a session file of this device, or cwd, a folder."
                    .into(),
            );
        };
        if !Path::new(cwd).has_root() {
            return Err("identity cwd must be an absolute path.".into());
        }
        return Ok(());
    }
    if !bundle::is_move_id(&command["moveId"]) {
        return Err("moveId must be a plain id.".into());
    }
    match op {
        "prepare" => {
            let cwd = command.get("cwd");
            if let Some(cwd_val) = cwd {
                let cwd_str = cwd_val
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or("prepare cwd must be a non-empty string.")?;
                if !Path::new(cwd_str).has_root() {
                    return Err("prepare cwd must be an absolute path.".into());
                }
            }
            if let Some(repo) = command.get("repository") {
                if cwd.is_none() {
                    return Err(
                        "prepare needs cwd, the folder the move lands in, together with repository."
                            .into(),
                    );
                }
                if !well_shaped_repository(repo) {
                    return Err(
                        "prepare repository must be { rootCommits: string[], originUrl?: string }, as the identity op answers it.".into(),
                    );
                }
            }
        }
        "cancel" => {}
        "status" => {
            let has_sp = command.get("sessionPath").is_some();
            let has_landing = command.get("landing").is_some();
            if has_sp && has_landing {
                return Err("status takes sessionPath or landing, not both.".into());
            }
            if has_sp {
                text(command, "sessionPath")?;
            }
            if let Some(landing) = command.get("landing") {
                let Some(obj) = landing.as_object() else {
                    return Err(
                        "landing must be an object: cwd, sessionFileName and, if the import named one, sessionDir.".into(),
                    );
                };
                let cwd = obj
                    .get("cwd")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && Path::new(s).has_root())
                    .ok_or("landing.cwd must be the absolute folder the import was given.")?;
                let _ = cwd;
                let sfn = obj
                    .get("sessionFileName")
                    .and_then(Value::as_str)
                    .filter(|s| bundle::is_session_file_name(s))
                    .ok_or(
                        "landing.sessionFileName must be the session's plain file name, as the bundle's manifest names it.",
                    )?;
                let _ = sfn;
                if let Some(sdir) = obj.get("sessionDir") {
                    let sdir_str = sdir
                        .as_str()
                        .filter(|s| !s.is_empty() && Path::new(s).has_root())
                        .ok_or("landing.sessionDir must be an absolute path.")?;
                    let _ = sdir_str;
                }
            }
        }
        "export" => {
            text(command, "sessionPath")?;
            text(command, "target")?;
            if command.get("droppedUserMessages").is_some_and(|value| {
                value
                    .as_array()
                    .is_none_or(|items| items.iter().any(|item| !item.is_string()))
            }) {
                return Err("droppedUserMessages must contain strings.".into());
            }
            if let Some(inline) = command.get("inline")
                && inline.as_bool() != Some(true)
            {
                return Err("inline must be true or omitted.".into());
            }
            if let Some(cap) = command.get("inlineMaxBytes")
                && cap.as_u64().is_none()
            {
                return Err("inlineMaxBytes must be a non-negative integer.".into());
            }
        }
        "finish" => {
            text(command, "sessionPath")?;
            let answer = &command["answer"];
            let valid = match answer["state"].as_str() {
                Some("receiving" | "accepted") => true,
                Some("declined") => matches!(
                    answer["reason"].as_str(),
                    Some(
                        "no_record"
                            | "expired"
                            | "cancelled"
                            | "chunk_without_ready"
                            | "repository"
                    )
                ),
                Some("failed") => matches!(
                    answer["reason"].as_str(),
                    Some("bundle_invalid" | "import_failed")
                ),
                _ => false,
            };
            if !valid {
                return Err("finish needs the receiver's answer from status.".into());
            }
        }
        "revoke" => {
            text(command, "sessionPath")?;
        }
        _ => return Err("invalid move operation".into()),
    }
    Ok(())
}

async fn session_dir_refusal(session_dir: Option<&str>) -> Option<&'static str> {
    let dir = session_dir?;
    let mut cur = Path::new(dir);
    loop {
        if let Ok(meta) = tokio::fs::metadata(cur).await {
            if !meta.is_dir() {
                return Some("sessionDir must be a folder: a file is in its place or above it.");
            }
            return None;
        }
        cur = cur.parent()?;
    }
}

async fn landed_when_fenced(ledger: &MoveLedger, error: &str) -> Option<Value> {
    let rest = error
        .strip_prefix("move_fenced: ")
        .unwrap_or(error)
        .strip_prefix("Move ")?;
    let move_id = rest.split_whitespace().next()?;
    let record = ledger.current_record(move_id).ok().flatten()?;
    if !matches!(
        record,
        MoveRecord::Status(MoveStatus::Accepted | MoveStatus::AcceptedRevoked)
    ) {
        return None;
    }
    let (status, landed) =
        maho_core::session_move::import::answer_move_status_and_landing(ledger, move_id, None)
            .await
            .ok()?;
    let mut out = wire_landed(landed)?;
    if matches!(status, MoveStatus::AcceptedRevoked)
        && let Some(obj) = out.as_object_mut()
    {
        obj.insert("revoked".into(), Value::Bool(true));
    }
    Some(out)
}

pub async fn move_session(agent_dir: &str, hosted_paths: &[String], command: &Value) -> Value {
    if let Err(error) = validate_move_request(command) {
        return refusal_with_extra(command, "invalid_move_request", &error, None);
    }
    let op = command["op"].as_str().unwrap_or_default();
    if op != "import" && op != "identity" && !bundle::is_move_id(&command["moveId"]) {
        return refusal_with_extra(
            command,
            "invalid_move_request",
            "moveId must be a plain id: a letter or digit, then letters, digits, '.', '_' or '-', at most 128 characters.",
            None,
        );
    }
    if op == "import"
        && let Some(not_a_folder) =
            session_dir_refusal(command.get("sessionDir").and_then(Value::as_str)).await
    {
        return refusal_with_extra(command, "invalid_move_request", not_a_folder, None);
    }
    let root = Path::new(agent_dir).join("session-moves");
    let ledger = MoveLedger::new(root.join("ledger"));
    let id = command["moveId"].as_str().unwrap_or_default();
    let result: Result<Value, String> = async {
        match op {
            "prepare" => {
                let cwd = command.get("cwd").and_then(Value::as_str).map(Path::new);
                let repo = command.get("repository").and_then(parse_wire_repository);
                prepare_move(PrepareMoveOptions {
                    ledger: &ledger,
                    move_id: id,
                    cwd,
                    repository: repo.as_ref(),
                })
                .await
                .map(wire)
            }
            "identity" => {
                let repo = if let Some(cwd) = command.get("cwd").and_then(Value::as_str) {
                    let resolved = maho_core::session_move::session_lock::real_session_path_async(
                        Path::new(cwd),
                    )
                    .await;
                    read_repository_identity(&resolved).await?
                } else {
                    let sp = text(command, "sessionPath")?;
                    let sessions_root = Path::new(agent_dir).join("sessions");
                    maho_core::session_move::identity::read_session_repository_identity(
                        &sessions_root,
                        Path::new(sp),
                    )
                    .await?
                };
                Ok(match repo {
                    None => json!({}),
                    Some(repo) => {
                        let mut r = json!({"rootCommits": repo.root_commits});
                        if let Some(url) = repo.origin_url {
                            r["originUrl"] = Value::String(url);
                        }
                        json!({"repository": r})
                    }
                })
            }
            "cancel" => ledger
                .decline(id, DeclineReason::Cancelled)
                .await
                .map(wire)
                .map_err(|error| error.to_string()),
            "status" => {
                let asked_buf: Option<(PathBuf, StatusFileKind)> = if let Some(sp) =
                    command.get("sessionPath").and_then(Value::as_str)
                {
                    Some((PathBuf::from(sp), StatusFileKind::Copy))
                } else if let Some(landing) = command.get("landing").and_then(Value::as_object) {
                    let cwd = landing
                        .get("cwd")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let sfn = landing
                        .get("sessionFileName")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let sdir = match landing.get("sessionDir").and_then(Value::as_str) {
                        Some(dir) => PathBuf::from(dir),
                        None => {
                            PathBuf::from(maho_core::session_manager::get_default_session_dir_path(
                                cwd, agent_dir,
                            ))
                        }
                    };
                    Some((sdir.join(sfn), StatusFileKind::Landing))
                } else {
                    None
                };
                let asked = asked_buf.as_ref().map(|(file, kind)| StatusFile {
                    file: file.as_path(),
                    kind: *kind,
                });
                let (status, landed) =
                    maho_core::session_move::import::answer_move_status_and_landing(
                        &ledger, id, asked,
                    )
                    .await?;
                Ok(merge_landed(status_wire(status), wire_landed(landed)))
            }
            "export" => {
                let path = &ordered_real_path(text(command, "sessionPath")?).await;
                closed(hosted_paths, &path.to_string_lossy())?;
                let entries = load_entries(path).await;
                if !matches!(
                    session_ownership(&entries),
                    SessionOwnership::Refused(ref owner)
                        if owner.state == RefusedState::Moving && owner.move_id == id
                ) {
                    return Err("move_not_released: release the session for this move first".into());
                }
                let destination = root.join("outgoing").join(id);
                let dropped = command["droppedUserMessages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                let state =
                    maho_core::session_move::extension_state::sealed_outgoing_state_dir(path, id)
                        .await?;
                let guard = || closed(hosted_paths, &path.to_string_lossy());
                let inline_max_bytes =
                    (command.get("inline").and_then(Value::as_bool) == Some(true)).then(|| {
                        command
                            .get("inlineMaxBytes")
                            .and_then(Value::as_u64)
                            .map_or(MOVE_INLINE_MAX_BYTES, |n| {
                                (n as usize).min(MOVE_INLINE_MAX_BYTES)
                            })
                    });
                let (manifest, inline_files) =
                    maho_core::session_move::export::export_session_for_move_and_read(
                        maho_core::session_move::export::ExportSessionForMoveOptions {
                            session_file: path,
                            move_id: id,
                            target: text(command, "target")?,
                            dropped_user_messages: &dropped,
                            bundle_dir: &destination,
                            extension_state_dir: state.as_deref(),
                            resolve_extension_state: None,
                            guard: Some(&guard),
                            locked_guard: Some(&guard),
                        },
                        |written| {
                            let written = written.to_owned();
                            async move {
                                match inline_max_bytes {
                                    Some(max_bytes) => {
                                        maho_core::session_move::inline_bundle::read_inline_bundle(
                                            &written,
                                            max_bytes,
                                            MOVE_INLINE_MAX_FILES,
                                        )
                                        .await
                                    }
                                    None => Ok(None),
                                }
                            }
                        },
                    )
                    .await?;
                let files = 1
                    + manifest.sidecars.len()
                    + manifest
                        .payloads
                        .iter()
                        .map(|payload| payload.files.len())
                        .sum::<usize>();
                let mut out = json!({
                    "move_id": id,
                    "session_id": manifest.session_id,
                    "bundle_dir": destination,
                    "files": files,
                });
                if let Some(inline) = inline_files {
                    out["inline_files"] = serde_json::to_value(inline).unwrap_or(Value::Null);
                }
                Ok(out)
            }
            "finish" => {
                let path = &ordered_real_path(text(command, "sessionPath")?).await;
                let guard = || closed(hosted_paths, &path.to_string_lossy());
                let answer =
                    maho_core::session_move::move_ledger::parse_move_status(&command["answer"])
                        .ok_or("invalid_move_request: invalid move answer")?;
                maho_core::session_move::finish::finish_move(
                    maho_core::session_move::finish::FinishMoveOptions {
                        session_file: path,
                        bundle_dir: &root.join("outgoing").join(id),
                        move_id: id,
                        answer: &answer,
                        guard: Some(&guard),
                    },
                )
                .await?;
                let entries = load_entries(path).await;
                let state = match session_ownership(&entries) {
                    SessionOwnership::Writable => "writable",
                    SessionOwnership::Refused(owner) => {
                        if owner.state == RefusedState::Moved {
                            "moved"
                        } else {
                            "moving"
                        }
                    }
                };
                Ok(json!({"ownership":state}))
            }
            "import" => {
                let cwd = Path::new(text(command, "cwd")?);
                let dir = match command.get("sessionDir").and_then(Value::as_str) {
                    Some(sdir) => PathBuf::from(sdir),
                    None => {
                        PathBuf::from(maho_core::session_manager::get_default_session_dir_with(
                            &cwd.to_string_lossy(),
                            agent_dir,
                        ))
                    }
                };
                let from = command["from"].as_str();
                let outcome = if command.get("files").is_some() {
                    let files = parse_inline_files(&command["files"])?;
                    let incoming = root.join("incoming");
                    maho_core::session_move::inline_bundle::with_staged_inline_bundle(
                        &files,
                        &incoming,
                        |bundle_dir, sent| {
                            let ledger_ref = &ledger;
                            let dir_ref = &dir;
                            async move {
                                maho_core::session_move::import::import_moved_session_with_sent(
                                    maho_core::session_move::import::ImportMovedSessionOptions {
                                        bundle_dir: &bundle_dir,
                                        target_cwd: cwd,
                                        ledger: ledger_ref,
                                        session_dir: Some(dir_ref),
                                        from,
                                        read_identity: None,
                                    },
                                    Some(&sent),
                                )
                                .await
                            }
                        },
                    )
                    .await?
                } else {
                    maho_core::session_move::import::import_moved_session(
                        maho_core::session_move::import::ImportMovedSessionOptions {
                            bundle_dir: Path::new(text(command, "bundleDir")?),
                            target_cwd: cwd,
                            ledger: &ledger,
                            session_dir: Some(&dir),
                            from,
                            read_identity: None,
                        },
                    )
                    .await?
                };
                let entries = load_entries(&outcome).await;
                Ok(json!({
                    "session_id": maho_core::session_move::ownership::header_session_id(&entries),
                    "session_path": outcome,
                }))
            }
            "revoke" => {
                let path = &ordered_real_path(text(command, "sessionPath")?).await;
                let guard = || closed(hosted_paths, &path.to_string_lossy());
                maho_core::session_move::revoke::revoke_imported_move(
                    maho_core::session_move::revoke::RevokeMoveOptions {
                        session_file: path,
                        move_id: id,
                        ledger: &ledger,
                        guard: Some(&guard),
                    },
                )
                .await?;
                Ok(json!({"ownership":"moved"}))
            }
            _ => Err("invalid_move_request: invalid move operation".into()),
        }
    }
    .await;
    match result {
        Ok(data) => {
            json!({"id":command["id"],"type":"response","command":"move_session","success":true,"data":data})
        }
        Err(error) => {
            let code = error
                .split(':')
                .next()
                .filter(|code| {
                    matches!(
                        *code,
                        "invalid_move_request"
                            | "session_open_here"
                            | "move_not_released"
                            | "move_mismatch"
                            | "move_bundle_invalid"
                            | "move_repository_mismatch"
                            | "move_target_occupied"
                            | "move_already_moved"
                            | "move_fenced"
                    )
                })
                .unwrap_or(if error.contains("can no longer be imported") {
                    "move_fenced"
                } else if error.contains("could not be verified") {
                    "move_bundle_invalid"
                } else {
                    "move_failed"
                });
            let extra = if code == "move_fenced" {
                landed_when_fenced(&ledger, &error).await
            } else {
                None
            };
            refusal_with_extra(command, code, &error, extra)
        }
    }
}

/// senpi `orderedRealPath`: resolves a session file's real path off the async workers, serialized per resolved path so
/// the move steps for one file enter the per-file move lock in the order they were called.
async fn ordered_real_path(path: &str) -> PathBuf {
    static CHAINS: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<PathBuf, std::sync::Arc<tokio::sync::Mutex<()>>>,
        >,
    > = std::sync::OnceLock::new();
    let key = std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
    let chain = std::sync::Arc::clone(
        CHAINS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key.clone())
            .or_default(),
    );
    let turn = chain.lock().await;
    let resolved = tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_| PathBuf::from(path));
    drop(turn);
    let mut chains = CHAINS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if chains.get(&key).is_some_and(|tail| {
        std::sync::Arc::ptr_eq(tail, &chain) && std::sync::Arc::strong_count(tail) == 2
    }) {
        chains.remove(&key);
    }
    resolved
}

fn text<'a>(command: &'a Value, key: &str) -> Result<&'a str, String> {
    command[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("invalid_move_request: missing {key}"))
}

fn closed(hosted_paths: &[String], session_path: &str) -> Result<(), String> {
    let target = std::fs::canonicalize(session_path).ok();
    if hosted_paths.iter().any(|path| {
        path == session_path
            || target
                .as_ref()
                .is_some_and(|target| std::fs::canonicalize(path).ok().as_ref() == Some(target))
    }) {
        return Err("session_open_here: session is open on this host".into());
    }
    Ok(())
}

async fn load_entries(path: &Path) -> Vec<Value> {
    maho_core::session_entry_file::load_entries_from_file_async(path, None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ordered_real_path_preserves_call_order_and_drops_its_chain() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        let path = file.to_string_lossy().into_owned();
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for index in 0..8 {
            let path = path.clone();
            let order = std::sync::Arc::clone(&order);
            tasks.push(tokio::spawn(async move {
                let resolved = ordered_real_path(&path).await;
                order.lock().unwrap().push(index);
                resolved
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap(), std::fs::canonicalize(&file).unwrap());
        }
        assert_eq!(*order.lock().unwrap(), (0..8).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn identity_prepare_inline_export_import_and_status_landing_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path().join("agent");
        let sessions_dir = agent_dir.join("sessions").join("--repo--");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let repo_dir = temp.path().join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let session_file = sessions_dir.join("2026-11-11_s1.jsonl");
        std::fs::write(
            &session_file,
            format!(
                "{}\n{}\n",
                json!({"type":"session","id":"s1","cwd":repo_dir.to_string_lossy()}),
                json!({"type":"custom","customType":"session_released","data":{"reason":"move","move_id":"m-inline","session_id":"s1","target":"dev-b","released_at":"2026-11-11T00:00:00.000Z"}})
            ),
        )
        .unwrap();

        let id_resp = move_session(
            &agent_dir.to_string_lossy(),
            &[],
            &json!({
                "id": "1",
                "op": "identity",
                "sessionPath": session_file.to_string_lossy(),
            }),
        )
        .await;
        assert_eq!(id_resp["success"], true);
        assert_eq!(id_resp["data"], json!({}));

        let outside_resp = move_session(
            &agent_dir.to_string_lossy(),
            &[],
            &json!({
                "id": "2",
                "op": "identity",
                "sessionPath": temp.path().join("outside.jsonl").to_string_lossy(),
            }),
        )
        .await;
        assert_eq!(outside_resp["success"], false);
        assert_eq!(outside_resp["errorCode"], "move_failed");

        let export_resp = move_session(
            &agent_dir.to_string_lossy(),
            &[],
            &json!({
                "id": "3",
                "op": "export",
                "sessionPath": session_file.to_string_lossy(),
                "moveId": "m-inline",
                "target": "dev-b",
                "inline": true,
            }),
        )
        .await;
        assert_eq!(export_resp["success"], true);
        let inline_files = export_resp["data"]["inline_files"]
            .as_array()
            .expect("inline_files")
            .clone();
        assert!(!inline_files.is_empty());
        assert_eq!(inline_files[0]["path"], "manifest.json");

        let receiver_agent = temp.path().join("receiver-agent");
        let target_session_dir = temp.path().join("custom-sessions");
        let prep1 = move_session(
            &receiver_agent.to_string_lossy(),
            &[],
            &json!({
                "id": "4",
                "op": "prepare",
                "moveId": "m-inline",
            }),
        )
        .await;
        assert_eq!(prep1["success"], true);
        assert_eq!(prep1["data"]["state"], "ready");

        let status_landing = move_session(
            &receiver_agent.to_string_lossy(),
            &[],
            &json!({
                "id": "5",
                "op": "status",
                "moveId": "m-inline",
                "landing": {
                    "cwd": repo_dir.to_string_lossy(),
                    "sessionFileName": "2026-11-11_s1.jsonl",
                    "sessionDir": target_session_dir.to_string_lossy(),
                },
            }),
        )
        .await;
        assert_eq!(status_landing["success"], true);
        assert_eq!(status_landing["data"]["state"], "receiving");
    }
}
