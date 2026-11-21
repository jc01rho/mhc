//! Port of senpi `packages/coding-agent/src/cli/session-of-pid-command.ts` (senpi#6972, omo#14255).
//!
//! `session-of-pid <pid> --json`: which session spawned `pid`, from the kernel's ancestry and the
//! spawning engine's records under this agent directory, never from an environment. One version-1
//! JSON record; exit 0 when the pid is attested, 1 when it is not, 2 on invalid arguments. A metadata
//! command, answered before settings, migrations, extensions or hosts: it starts no host and writes
//! nothing.

use std::collections::HashMap;
use std::path::Path;

use maho_tools::session_spawn_attestation::{PidSession, resolve_pid_session, session_spawns_dir};
use serde_json::json;

/// One versioned JSON record and the exit code for `session-of-pid <pid> --json`; `None` for any other
/// argv.
pub fn session_of_pid_command_output(
    args: &[String],
    env: &HashMap<String, String>,
    home_dir: &str,
) -> Option<(String, i32)> {
    if args.first().map(String::as_str) != Some("session-of-pid") {
        return None;
    }
    let pid = args
        .get(1)
        .and_then(|pid| pid.parse::<u32>().ok())
        .filter(|pid| *pid > 0);
    let (Some(pid), 3, Some("--json")) = (pid, args.len(), args.get(2).map(String::as_str)) else {
        let record = json!({
            "version": 1,
            "ok": false,
            "code": "invalid_arguments",
            "message": format!("Usage: {} session-of-pid <pid> --json", maho_core::config::app_name()),
        });
        return Some((format!("{record}\n"), 2));
    };
    // `get_agent_dir()` panics on the test guard; resolve and check here so the failure is a record,
    // never a panic message on stderr.
    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_owned());
    let env_override = maho_core::brand::env_value("CODING_AGENT_DIR", env);
    let dir = maho_core::config::resolve_agent_dir(&cwd, home_dir, env_override.as_deref());
    if let Err(error) = maho_core::config::assert_agent_dir_allowed_in_tests(
        &dir,
        &maho_core::config::config_dir_name(),
        env,
    ) {
        let record = json!({
            "version": 1,
            "ok": false,
            "code": "selection_failed",
            "message": error.to_string(),
        });
        return Some((format!("{record}\n"), 1));
    }
    let (record, attested) = describe(&resolve_pid_session(
        pid,
        &session_spawns_dir(Path::new(&dir)),
    ));
    Some((format!("{record}\n"), if attested { 0 } else { 1 }))
}

fn describe(session: &PidSession) -> (serde_json::Value, bool) {
    match session {
        PidSession::Attested {
            pid,
            session_id,
            attested_pid,
            host_pid,
            chain,
        } => (
            json!({
                "version": 1,
                "ok": true,
                "attested": true,
                "pid": pid,
                "sessionId": session_id,
                "attestedPid": attested_pid,
                "hostPid": host_pid,
                "chain": chain,
            }),
            true,
        ),
        PidSession::Unattested { pid, reason } => (
            json!({
                "version": 1,
                "ok": true,
                "attested": false,
                "pid": pid,
                "reason": reason.as_str(),
            }),
            false,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn answer(args: &[&str], env: &HashMap<String, String>) -> (serde_json::Value, i32) {
        let (text, code) =
            session_of_pid_command_output(&argv(args), env, "/home/u").expect("answered");
        assert_eq!(text.trim().lines().count(), 1);
        (serde_json::from_str(&text).expect("json"), code)
    }

    #[test]
    fn other_commands_are_not_answered() {
        for args in [
            &[][..],
            &["config", "agent-dir", "--json"][..],
            &["--session-of-pid", "1", "--json"][..],
            &["list", "session-of-pid"][..],
        ] {
            assert!(
                session_of_pid_command_output(&argv(args), &HashMap::new(), "/home/u").is_none(),
                "{args:?}"
            );
        }
    }

    #[test]
    fn malformed_arguments_are_one_typed_record_with_exit_two() {
        for args in [
            &["session-of-pid"][..],
            &["session-of-pid", "--json"][..],
            &["session-of-pid", "42"][..],
            &["session-of-pid", "42", "--help"][..],
            &["session-of-pid", "42", "--json", "extra"][..],
            &["session-of-pid", "abc", "--json"][..],
            &["session-of-pid", "0", "--json"][..],
            &["session-of-pid", "-5", "--json"][..],
            &["session-of-pid", "1.5", "--json"][..],
            &["session-of-pid", "--json", "42"][..],
        ] {
            let (record, code) = answer(args, &HashMap::new());
            assert_eq!(code, 2, "{args:?}");
            assert_eq!(record["ok"], false);
            assert_eq!(record["version"], 1);
            assert_eq!(record["code"], "invalid_arguments");
            assert!(
                record["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("session-of-pid <pid> --json")),
                "{record}"
            );
        }
    }

    #[test]
    fn a_pid_with_no_process_is_unattested_with_exit_one() {
        let (record, code) = answer(&["session-of-pid", "4294967295", "--json"], &HashMap::new());
        assert_eq!(code, 1);
        assert_eq!(
            record,
            json!({
                "version": 1,
                "ok": true,
                "attested": false,
                "pid": 4_294_967_295_u32,
                "reason": "process_not_found",
            })
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_recorded_child_is_attested_and_the_answer_names_its_session() {
        let agent_dir = tempfile::tempdir().expect("agent dir");
        let env = HashMap::from([(
            format!(
                "{}_CODING_AGENT_DIR",
                maho_core::brand::brand_profile().env_prefix
            ),
            agent_dir.path().to_string_lossy().into_owned(),
        )]);
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep starts");
        let pid = child.id();
        let attestation = maho_tools::session_spawn_attestation::attest_session_spawn(
            &session_spawns_dir(agent_dir.path()),
            "session-1",
            pid,
        );
        assert!(attestation.recorded());

        let pid_text = pid.to_string();
        let (record, code) = answer(&["session-of-pid", pid_text.as_str(), "--json"], &env);
        let _killed = child.kill();
        let _waited = child.wait();
        attestation.release();

        assert_eq!(code, 0);
        assert_eq!(
            record,
            json!({
                "version": 1,
                "ok": true,
                "attested": true,
                "pid": pid,
                "sessionId": "session-1",
                "attestedPid": pid,
                "hostPid": std::process::id(),
                "chain": [pid],
            })
        );
    }
}
