use maho_core::session_manager::SessionManager;
use std::path::Path;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .expect("git");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

async fn session_starting_in(
    cwd: &Path,
    session_dir: &Path,
    _file: &Path,
) -> maho_core::agent_session::AgentSession {
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.to_string_lossy().into_owned()),
            agent_dir: Some(cwd.join(".agent").to_string_lossy().into_owned()),
            session_manager: Some(SessionManager::create(
                &cwd.to_string_lossy(),
                Some(&session_dir.to_string_lossy()),
                None,
            )),
            session_start_event: Some(maho_ext_api::SessionStartEvent {
                sizes_only: false,
                reason: maho_ext_api::SessionReason::Startup,
                initial_model_provenance: None,
                previous_session_file: None,
                attended: None,
            }),
            tools: Some(Vec::new()),
            ..Default::default()
        }),
    )
    .await;
    let session = result
        .expect("bounded startup")
        .expect("SDK startup")
        .session;
    session
        .bind_extensions(maho_core::agent_session::ExtensionBindings::default())
        .await;
    session
}

async fn identity_entries(
    session: &maho_core::agent_session::AgentSession,
    session_dir: &Path,
) -> Vec<serde_json::Value> {
    session.dispose().await;
    identity_entries_of(session_dir)
}

// 2026-11-26: a session that never recorded its repository was refused by the move import
// (`move_repository_mismatch`) even when the target held the same repository, so no lane could move.
#[tokio::test]
async fn session_start_records_the_checkout_identity_as_startup_bookkeeping() {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "root"]);
    let root = git(dir.path(), &["rev-list", "--max-parents=0", "HEAD"]);
    let file = dir.path().join("session.jsonl");
    let session_dir = dir.path().join("sessions");
    std::fs::create_dir_all(&session_dir).expect("sessions dir");
    let session = session_starting_in(dir.path(), &session_dir, &file).await;
    let entries = identity_entries(&session, &session_dir).await;
    assert_eq!(entries.len(), 1, "exactly one identity entry");
    assert_eq!(entries[0]["data"]["rootCommits"], serde_json::json!([root]));
    assert_eq!(entries[0]["startupBookkeeping"], serde_json::json!(true));
}

#[tokio::test]
async fn session_start_outside_any_repository_records_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("session.jsonl");
    let session_dir = dir.path().join("sessions");
    std::fs::create_dir_all(&session_dir).expect("sessions dir");
    let session = session_starting_in(dir.path(), &session_dir, &file).await;
    assert!(identity_entries(&session, &session_dir).await.is_empty());
}

#[tokio::test]
async fn resuming_a_session_that_already_recorded_its_identity_does_not_duplicate_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "root"]);
    let file = dir.path().join("session.jsonl");
    let session_dir = dir.path().join("sessions");
    std::fs::create_dir_all(&session_dir).expect("sessions dir");
    let first = session_starting_in(dir.path(), &session_dir, &file).await;
    first.dispose().await;
    assert_eq!(identity_entries_of(&session_dir).len(), 1);
    let cwd = dir.path().to_string_lossy().into_owned();
    let first_file = std::fs::read_dir(&session_dir)
        .expect("sessions dir")
        .next()
        .expect("one session file")
        .expect("dir entry")
        .path()
        .to_string_lossy()
        .into_owned();
    let second = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(dir.path().join(".agent").to_string_lossy().into_owned()),
        session_manager: Some(
            SessionManager::open(
                &first_file,
                Some(&session_dir.to_string_lossy()),
                Some(&cwd),
                None,
            )
            .expect("reopen"),
        ),
        tools: Some(Vec::new()),
        ..Default::default()
    })
    .await
    .expect("resume session")
    .session;
    second
        .bind_extensions(maho_core::agent_session::ExtensionBindings::default())
        .await;
    second.dispose().await;
    assert_eq!(identity_entries_of(&session_dir).len(), 1);
}

fn identity_entries_of(session_dir: &Path) -> Vec<serde_json::Value> {
    let mut entries = Vec::new();
    for item in std::fs::read_dir(session_dir).expect("sessions dir") {
        let file = item.expect("dir entry").path();
        if file.extension().is_some_and(|ext| ext == "jsonl") {
            entries.extend(
                std::fs::read_to_string(&file)
                    .expect("session file")
                    .lines()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .filter(|entry| entry["customType"] == "repository-identity"),
            );
        }
    }
    entries
}
