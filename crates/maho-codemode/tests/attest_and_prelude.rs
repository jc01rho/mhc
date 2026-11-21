use maho_codemode::bridges::schema_bridge::EvalSchemaToolInfo;
use maho_codemode::config::settings::Languages;
use maho_codemode::extension::kernel_preludes::active_kernel_preludes;
use maho_codemode::extension::reachable_tools::{eval_only_tool_names, reachable_tool_names};
use maho_codemode::prompt::eval_prompt::{EvalPromptOptions, build_eval_prompt};
use maho_tools::definition::{KernelPrelude, ToolExposure};

fn tool(name: &str, exposure: ToolExposure, eval_only: bool, prelude: bool) -> EvalSchemaToolInfo {
    EvalSchemaToolInfo {
        name: name.into(),
        description: None,
        parameters: None,
        kernel_prelude: prelude.then(|| KernelPrelude {
            javascript: format!("globalThis.{name}_facade = {{}};"),
            python: String::new(),
            documentation: format!("{name}_facade: {name} helpers"),
            exports: vec![format!("{name}_facade")],
            ..Default::default()
        }),
        exposure: Some(exposure),
        eval_only,
    }
}

#[test]
fn an_eval_exposed_tool_withheld_from_the_model_keeps_its_kernel_prelude() {
    let all = vec![
        tool("read", ToolExposure::Direct, false, false),
        tool("task", ToolExposure::Eval, true, true),
        tool("computer", ToolExposure::Direct, false, true),
    ];
    let active = vec!["read".to_owned(), "eval".to_owned()];
    let preludes = active_kernel_preludes(&active, &all);
    assert_eq!(
        preludes
            .iter()
            .map(|prelude| prelude.exports.clone())
            .collect::<Vec<_>>(),
        vec![vec!["task_facade".to_owned()]]
    );
    assert!(reachable_tool_names(&active, &all).contains(&"task".to_owned()));
    assert!(!reachable_tool_names(&active, &all).contains(&"computer".to_owned()));
}

#[test]
fn the_eval_description_names_the_eval_only_tools() {
    let all = vec![
        tool("grep", ToolExposure::Direct, true, false),
        tool("read", ToolExposure::Direct, false, false),
        tool("bash", ToolExposure::Direct, true, false),
    ];
    let enabled = Languages { js: true };
    let with = build_eval_prompt(
        &enabled,
        &EvalPromptOptions {
            eval_only_tools: eval_only_tool_names(&all),
            ..Default::default()
        },
    )
    .unwrap();
    let line = with
        .description
        .lines()
        .find(|line| line.contains("Eval-only tools"))
        .expect("eval-only line");
    assert!(line.trim_end().ends_with(": grep, bash."), "{line}");
    let without = build_eval_prompt(&enabled, &EvalPromptOptions::default()).unwrap();
    assert!(!without.description.contains("Eval-only tools"));
}

#[cfg(unix)]
mod attestation {
    use maho_tools::session_spawn_attestation::{
        PidSession, SessionSpawnRecord, UnattestedReason, attest_session_spawn, resolve_pid_session,
    };
    use std::process::Stdio;

    /// A child that lives until its stdin closes, so the test controls its lifetime without waiting on time.
    fn spawn_child(session_env: &str) -> tokio::process::Child {
        tokio::process::Command::new("cat")
            .env("PI_SESSION_ID", session_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    async fn end(mut child: tokio::process::Child) {
        drop(child.stdin.take());
        child.wait().await.unwrap();
    }

    #[tokio::test]
    async fn a_spawned_child_resolves_to_its_attested_session_whatever_its_environment_says() {
        let dir = tempfile::tempdir().unwrap();
        let child = spawn_child("forged-session");
        let pid = child.id().unwrap();
        let attestation = attest_session_spawn(dir.path(), "real-session", pid);
        assert!(attestation.recorded());
        match resolve_pid_session(pid, dir.path()) {
            PidSession::Attested {
                session_id,
                attested_pid,
                host_pid,
                chain,
                ..
            } => {
                assert_eq!(session_id, "real-session");
                assert_eq!(attested_pid, pid);
                assert_eq!(host_pid, std::process::id());
                assert_eq!(chain, vec![pid]);
            }
            other => panic!("expected an attested pid, got {other:?}"),
        }
        attestation.release();
        assert!(!dir.path().join(format!("{pid}.json")).exists());
        assert_eq!(
            resolve_pid_session(pid, dir.path()),
            PidSession::Unattested {
                pid,
                reason: UnattestedReason::NoAttestedAncestor,
            }
        );
        end(child).await;
    }

    #[tokio::test]
    async fn a_forged_record_for_a_live_pid_is_not_believed() {
        let dir = tempfile::tempdir().unwrap();
        let child = spawn_child("s");
        let pid = child.id().unwrap();
        let attestation = attest_session_spawn(dir.path(), "real-session", pid);
        assert!(attestation.recorded());
        let path = dir.path().join(format!("{pid}.json"));
        let mut record: SessionSpawnRecord =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        record.session_id = "attacker".into();
        record.host_started_at_ms += 1000;
        std::fs::write(&path, serde_json::to_string(&record).unwrap()).unwrap();
        assert_eq!(
            resolve_pid_session(pid, dir.path()),
            PidSession::Unattested {
                pid,
                reason: UnattestedReason::NoAttestedAncestor,
            }
        );
        // The release removes only the record it wrote; the forged one stays for whoever owns it.
        attestation.release();
        assert!(path.exists());
        end(child).await;
    }

    #[tokio::test]
    async fn a_live_record_is_not_replaced_by_another_sessions_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let child = spawn_child("s");
        let pid = child.id().unwrap();
        let first = attest_session_spawn(dir.path(), "first", pid);
        assert!(first.recorded());
        let second = attest_session_spawn(dir.path(), "second", pid);
        assert!(!second.recorded());
        match resolve_pid_session(pid, dir.path()) {
            PidSession::Attested { session_id, .. } => assert_eq!(session_id, "first"),
            other => panic!("expected an attested pid, got {other:?}"),
        }
        end(child).await;
    }

    #[tokio::test]
    async fn a_pid_that_is_gone_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let child = spawn_child("s");
        let pid = child.id().unwrap();
        end(child).await;
        assert_eq!(
            resolve_pid_session(pid, dir.path()),
            PidSession::Unattested {
                pid,
                reason: UnattestedReason::ProcessNotFound,
            }
        );
    }

    #[tokio::test]
    async fn a_kernel_without_a_session_environment_is_not_recorded() {
        assert!(
            maho_codemode::kernels::shared::session_spawn_attestation::attest_kernel_spawn(
                None,
                Some(std::process::id()),
            )
            .is_none()
        );
        let env = maho_codemode::kernels::session_env::SessionEnvironment::new();
        assert!(
            maho_codemode::kernels::shared::session_spawn_attestation::attest_kernel_spawn(
                Some(&env),
                Some(std::process::id()),
            )
            .is_none()
        );
    }
}
