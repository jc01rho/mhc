//! Path-bound import, staged payload checks, and file-authoritative status.
use super::bundle::*;
use super::landed_file::{LandedFile, outcome_in, read_landing};
use super::move_landing::LandingOutcome;
use super::move_ledger::{DeclineReason, FailReason, MoveLedger, MoveStatus};
use super::move_steps::{MoveStep, move_step};
use super::ownership::*;
use super::repository_check::is_checkout_of_repository;
use serde_json::json;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub type RepositoryIdentityReader<'a> =
    &'a dyn Fn(&Path) -> Result<Option<RepositoryIdentity>, String>;
pub struct ImportMovedSessionOptions<'a> {
    pub bundle_dir: &'a Path,
    pub target_cwd: &'a Path,
    pub ledger: &'a MoveLedger,
    pub session_dir: Option<&'a Path>,
    pub from: Option<&'a str>,
    pub read_identity: Option<RepositoryIdentityReader<'a>>,
}
pub async fn import_outcome(file: &Path, move_id: &str) -> LandingOutcome {
    outcome_in(&super::entries(file).await, move_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFileKind {
    Copy,
    Landing,
}

#[derive(Debug, Clone, Copy)]
pub struct StatusFile<'a> {
    pub file: &'a Path,
    pub kind: StatusFileKind,
}

pub async fn answer_move_status(
    ledger: &MoveLedger,
    move_id: &str,
    session_file: Option<&Path>,
) -> Result<MoveStatus, String> {
    let asked = session_file.map(|file| StatusFile {
        file,
        kind: StatusFileKind::Copy,
    });
    let (status, _) = answer_move_status_and_landing(ledger, move_id, asked).await?;
    Ok(status)
}

pub async fn answer_move_status_and_landing(
    ledger: &MoveLedger,
    move_id: &str,
    asked: Option<StatusFile<'_>>,
) -> Result<(MoveStatus, Option<LandedFile>), String> {
    let seen_file: Arc<Mutex<Option<LandedFile>>> = Arc::new(Mutex::new(None));
    let seen_landing: Arc<Mutex<Option<LandedFile>>> = Arc::new(Mutex::new(None));
    let seen_file_for_reader = seen_file.clone();
    let landed = move |target: String| -> std::pin::Pin<
        Box<dyn std::future::Future<Output = LandingOutcome> + Send>,
    > {
        let move_id = move_id.to_owned();
        let seen_file = seen_file_for_reader.clone();
        Box::pin(async move {
            let target_path = PathBuf::from(&target);
            let (outcome, file) = read_landing(&target_path, &move_id).await;
            *seen_file
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(file);
            outcome
        })
    };
    let status = match asked {
        Some(asked) if super::path_exists(asked.file).await => {
            crate::session_sidecar_store::serialize_by_key(
                &super::session_lock::move_lock_key(asked.file).await,
                async {
                    let (outcome, file) = read_landing(asked.file, move_id).await;
                    if outcome == LandingOutcome::Absent {
                        if asked.kind == StatusFileKind::Copy {
                            return Err(format!(
                                "move_mismatch: {} is not the copy this device imported for move {move_id}; nothing was answered",
                                asked.file.display()
                            ));
                        }
                        return ledger
                            .answer_status_with(move_id, false, false, Some(&landed))
                            .await
                            .map_err(|e| e.to_string());
                    }
                    if asked.kind == StatusFileKind::Landing {
                        *seen_landing
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(file);
                    }
                    ledger
                        .answer_status_with(
                            move_id,
                            true,
                            outcome == LandingOutcome::Revoked,
                            Some(&landed),
                        )
                        .await
                        .map_err(|e| e.to_string())
                },
            )
            .await?
        }
        _ => ledger
            .answer_status_with(move_id, false, false, Some(&landed))
            .await
            .map_err(|e| e.to_string())?,
    };
    let is_accepted = matches!(status, MoveStatus::Accepted | MoveStatus::AcceptedRevoked);
    let mut where_landed = seen_file
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .or_else(|| {
            seen_landing
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        });
    if is_accepted
        && where_landed.is_none()
        && let Ok(landing) = ledger.landing_of(move_id)
        && let Some(target) = landing.target
    {
        let (_, file) = read_landing(Path::new(&target), move_id).await;
        where_landed = Some(file);
    }
    Ok((status, if is_accepted { where_landed } else { None }))
}

pub async fn import_moved_session(
    options: ImportMovedSessionOptions<'_>,
) -> Result<PathBuf, String> {
    import_moved_session_with_sent(options, None).await
}

pub async fn import_moved_session_with_sent(
    options: ImportMovedSessionOptions<'_>,
    sent: Option<&HashMap<String, String>>,
) -> Result<PathBuf, String> {
    let manifest = read_manifest(options.bundle_dir).map_err(|e| e.to_string())?;
    let cwd = super::session_lock::real_session_path_async(options.target_cwd).await;
    let session_dir = match options.session_dir {
        Some(dir) => super::session_lock::real_session_path_async(dir).await,
        None => PathBuf::from(crate::session_manager::get_default_session_dir(
            &cwd.to_string_lossy(),
        )),
    };
    let target = session_dir.join(&manifest.session_file_name);
    // Once this move's file landed, the move records no failure: only its `accepted` write can have been lost.
    let landed = || async {
        super::path_exists(&target).await
            && import_outcome(&target, &manifest.move_id).await != LandingOutcome::Absent
    };
    if landed().await {
        options
            .ledger
            .confirm_landed(
                &manifest.move_id,
                &target.to_string_lossy(),
                import_outcome(&target, &manifest.move_id).await == LandingOutcome::Revoked,
            )
            .await
            .map_err(|e| e.to_string())?;
        return Ok(target);
    }
    if let Err(error) = verify_bundle_with_sent_async(options.bundle_dir, &manifest, sent).await {
        options
            .ledger
            .fail_unless_landed(&manifest.move_id, FailReason::BundleInvalid, landed())
            .await
            .map_err(|e| e.to_string())?;
        return Err(error.to_string());
    }
    options
        .ledger
        .require_readied_for(&manifest.move_id, &cwd)
        .map_err(|e| e.to_string())?;
    if !is_checkout_of_repository(&cwd, manifest.repository.as_ref(), options.read_identity).await?
    {
        if !landed().await {
            options
                .ledger
                .decline(&manifest.move_id, DeclineReason::Repository)
                .await
                .map_err(|e| e.to_string())?;
        }
        return Err(format!(
            "move_repository_mismatch: {} is not the same repository the session was recorded in ({})",
            cwd.display(),
            manifest.source_cwd
        ));
    }
    if let Err(error) = options
        .ledger
        .begin_receive_in(&manifest.move_id, &target.to_string_lossy(), Some(&cwd))
        .await
    {
        let err_str = error.to_string();
        if (err_str.contains("move_fenced") || err_str.contains("can no longer be imported"))
            && landed().await
        {
            options
                .ledger
                .confirm_landed(
                    &manifest.move_id,
                    &target.to_string_lossy(),
                    import_outcome(&target, &manifest.move_id).await == LandingOutcome::Revoked,
                )
                .await
                .map_err(|e| e.to_string())?;
            return Ok(target);
        }
        return Err(err_str);
    }
    move_step(MoveStep::ImportReceived)?;
    let staging = session_dir.join(".move-staging").join(&manifest.move_id);
    let result: Result<PathBuf, String> = async {
        let staged = stage(
            options.bundle_dir,
            &manifest,
            &staging,
            options.from.unwrap_or_default(),
        )
        .await?;
        let file = options
            .ledger
            .commit(
                &manifest.move_id,
                &target.to_string_lossy(),
                land(&staged, &target, &cwd, &manifest),
                async {
                    replace_predecessor(&target, &manifest.session_id, &session_dir).await?;
                    // Before the rename: a crash here re-copies on retry, and after it the state is already in place.
                    place_extension_state(
                        &staging,
                        &manifest,
                        &super::extension_state::moved_state_dir(&target, &manifest.move_id)?,
                    )
                    .await
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        move_step(MoveStep::ImportAccepted)?;
        super::remove(&staging).await?;
        Ok(file)
    }
    .await;
    if let Err(error) = &result {
        if (error.contains("move_fenced") || error.contains("can no longer be imported"))
            && landed().await
        {
            let _ = super::remove(&staging).await;
            options
                .ledger
                .confirm_landed(
                    &manifest.move_id,
                    &target.to_string_lossy(),
                    import_outcome(&target, &manifest.move_id).await == LandingOutcome::Revoked,
                )
                .await
                .map_err(|e| e.to_string())?;
            return Ok(target);
        }
        if !error.contains("move_fenced") && !error.contains("can no longer be imported") {
            let reason = if error.contains("could not be verified") {
                FailReason::BundleInvalid
            } else {
                FailReason::ImportFailed
            };
            options
                .ledger
                .fail_unless_landed(&manifest.move_id, reason, landed())
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    result
}
async fn replace_predecessor(
    target: &Path,
    session_id: &str,
    session_dir: &Path,
) -> Result<(), String> {
    if !super::path_exists(target).await {
        return Ok(());
    }
    let entries = super::entries(target).await;
    if header_session_id(&entries) != Some(session_id)
        || session_ownership(&entries) == SessionOwnership::Writable
    {
        return Err(format!(
            "move_target_occupied: a writable session already sits at {}",
            target.display()
        ));
    }
    let extensions = session_dir.join("extensions");
    if super::path_exists(&extensions).await {
        let mut entries = tokio::fs::read_dir(extensions)
            .await
            .map_err(|e| e.to_string())?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
            if entry.file_type().await.map_err(|e| e.to_string())?.is_dir() {
                super::remove(&entry.path().join(format!(
                    "{}.json",
                    crate::session_sidecar_store::encoded_session_id(session_id)
                )))
                .await?;
            }
        }
    }
    super::remove(&session_dir.join("resident-blobs").join(session_id)).await?;
    super::remove(target).await
}
pub async fn stage_payloads(
    bundle_dir: &Path,
    manifest: &MoveManifest,
    staging: &Path,
) -> Result<(), String> {
    for file in manifest.payloads.iter().flat_map(|p| &p.files) {
        let to = staging.join(".payloads").join(&file.path);
        super::copy(&bundle_dir.join(&file.path), &to).await?;
        if sha256_file_async(&to).await.map_err(|e| e.to_string())? != file.sha256 {
            return Err(
                MoveBundleInvalidError::new(format!("{} changed in transit", file.path))
                    .to_string(),
            );
        }
    }
    Ok(())
}
pub async fn place_extension_state(
    staging: &Path,
    manifest: &MoveManifest,
    dir: &Path,
) -> Result<(), String> {
    let files: Vec<_> = manifest
        .payloads
        .iter()
        .filter(|p| p.kind == "extension-state")
        .flat_map(|p| &p.files)
        .collect();
    if files.is_empty() {
        return Ok(());
    }
    super::remove(dir).await?;
    for file in files {
        super::copy(
            &staging.join(".payloads").join(&file.path),
            &dir.join(
                file.path
                    .strip_prefix(EXTENSION_STATE_PREFIX)
                    .ok_or("invalid extension state path")?,
            ),
        )
        .await?;
    }
    Ok(())
}
async fn stage(
    bundle_dir: &Path,
    manifest: &MoveManifest,
    staging: &Path,
    from: &str,
) -> Result<PathBuf, String> {
    let file = staging.join(&manifest.session_file_name);
    if !super::path_exists(&file).await
        || import_outcome(&file, &manifest.move_id).await == LandingOutcome::Absent
    {
        super::remove(staging).await?;
        super::copy(&bundle_dir.join(&manifest.session.path), &file).await?;
        for item in &manifest.sidecars {
            super::copy(
                &bundle_dir.join(&item.path),
                &staging.join(item.path.strip_prefix("files/").unwrap_or(&item.path)),
            )
            .await?;
        }
        stage_payloads(bundle_dir, manifest, staging).await?;
        super::append_custom(
            &file,
            SESSION_MOVED_IN,
            json!({"move_id":manifest.move_id,"session_id":manifest.session_id,"from":from,"bundle_sha256":manifest.session.sha256,"moved_in_at":crate::session_manager::now_iso_millis()}),
        )
        .await?;
    }
    super::requeue::requeue_input(&file, &manifest.move_id, &manifest.dropped_user_messages)
        .await?;
    Ok(file)
}
async fn land(
    staged: &Path,
    target: &Path,
    cwd: &Path,
    manifest: &MoveManifest,
) -> Result<PathBuf, String> {
    let content = tokio::fs::read_to_string(staged)
        .await
        .map_err(|e| e.to_string())?;
    let (first, rest) = content.split_once('\n').unwrap_or((&content, ""));
    let mut header: serde_json::Value = serde_json::from_str(first).map_err(|e| e.to_string())?;
    header["cwd"] = json!(cwd.to_string_lossy());
    let dir = target.parent().ok_or("missing target directory")?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| e.to_string())?;
    let temp = dir.join(format!(
        ".{}.rebind-{}.tmp",
        manifest.session_file_name,
        std::process::id()
    ));
    tokio::fs::write(&temp, format!("{header}\n{rest}"))
        .await
        .map_err(|e| e.to_string())?;
    for item in &manifest.sidecars {
        let relative = item.path.strip_prefix("files/").unwrap_or(&item.path);
        super::copy(
            &staged
                .parent()
                .ok_or("missing staging directory")?
                .join(relative),
            &dir.join(relative),
        )
        .await?;
    }
    tokio::fs::rename(temp, target)
        .await
        .map_err(|e| e.to_string())?;
    super::remove(staged).await?;
    Ok(target.into())
}
