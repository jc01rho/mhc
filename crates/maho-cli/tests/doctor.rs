use std::collections::BTreeMap;

use maho_cli::cli::doctor::{ReflectionLockFixOptions, doctor_lines, reflection_lock_fix};
use memory_core::locks::ProcessLiveness;
use memory_core::reflection::StaleLockProbe;
use memory_core::reflection::stale_holder_proof::ReflectionLockProbe;

#[test]
fn native_package_manifest_distinguishes_intact_tampered_and_missing_siblings() {
    use sha2::{Digest, Sha256};
    let root = tempfile::tempdir().expect("tempdir");
    let sibling = root.path().join("lsp-daemon");
    let bytes = b"native sibling fixture";
    std::fs::write(&sibling, bytes).expect("sibling");
    let manifest = serde_json::json!({"executables":[{"path":"lsp-daemon","sha256":format!("{:x}", Sha256::digest(bytes))}]});
    let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest");
    let manifest_path = root.path().join("manifest.json");
    std::fs::write(&manifest_path, &manifest_bytes).expect("manifest file");
    let intact = maho_cli::cli::doctor::package_lines(root.path());
    assert_eq!(intact.len(), 1);
    assert!(intact[0].starts_with("PASS "));
    assert_eq!(
        std::fs::read(&manifest_path).expect("unchanged manifest"),
        manifest_bytes
    );
    assert_eq!(std::fs::read(&sibling).expect("unchanged sibling"), bytes);
    std::fs::write(&sibling, b"tampered").expect("tamper fixture");
    assert!(maho_cli::cli::doctor::package_lines(root.path())[0].starts_with("FAIL "));
    std::fs::remove_file(&sibling).expect("remove fixture");
    assert!(maho_cli::cli::doctor::package_lines(root.path())[0].starts_with("FAIL "));
}

#[test]
fn invalid_config_is_reported_without_values() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(root.path().join(".omo")).expect("mkdir");
    let config_path = root.path().join(".omo").join("omo.jsonc");
    let config = r#"{"task":{"host_engine_policy":"sometimes"}}"#;
    std::fs::write(&config_path, config).expect("write");
    let env = BTreeMap::from([
        (String::from("HOME"), home.clone()),
        (String::from("USERPROFILE"), home.clone()),
    ]);
    let agent_dir = root
        .path()
        .join(".omo")
        .join("agent")
        .to_string_lossy()
        .into_owned();
    let lines = doctor_lines(root.path(), &env, &agent_dir);
    assert!(
        lines.iter().any(
            |line| line.starts_with("WARN config:") && line.contains("task.host_engine_policy")
        )
    );
    assert!(lines.iter().all(|line| !line.contains("sometimes")));
    assert_eq!(
        std::fs::read_to_string(config_path).expect("config unchanged"),
        config
    );
    assert!(
        !root
            .path()
            .join(".omo")
            .join("agent")
            .join("auth.json")
            .exists()
    );
}

#[test]
fn malformed_config_is_reported_as_not_loaded() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(root.path().join(".omo")).expect("mkdir");
    std::fs::write(root.path().join(".omo").join("omo.jsonc"), r#"{"task":{"#).expect("write");
    let env = BTreeMap::from([
        (String::from("HOME"), home.clone()),
        (String::from("USERPROFILE"), home.clone()),
    ]);
    let agent_dir = root
        .path()
        .join(".omo")
        .join("agent")
        .to_string_lossy()
        .into_owned();
    let lines = doctor_lines(root.path(), &env, &agent_dir);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("not loaded (JSONC parse error)"))
    );
}

#[test]
fn untrusted_project_resources_fail() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(root.path().join(".omo")).expect("mkdir");
    std::fs::write(root.path().join(".omo").join("settings.json"), "{}").expect("write");
    let env = BTreeMap::from([
        (String::from("HOME"), home.clone()),
        (String::from("USERPROFILE"), home.clone()),
    ]);
    let agent_dir = root
        .path()
        .join(".omo")
        .join("agent")
        .to_string_lossy()
        .into_owned();
    let lines = doctor_lines(root.path(), &env, &agent_dir);
    assert!(
        lines
            .iter()
            .any(|line| line == "WARN project resources: trust decision is not recorded")
    );
}

#[test]
fn missing_trust_store_does_not_create_agent_directory() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(root.path().join(".omo")).expect("mkdir");
    std::fs::write(root.path().join(".omo").join("settings.json"), "{}").expect("write");
    let env = BTreeMap::from([
        (String::from("HOME"), home.clone()),
        (String::from("USERPROFILE"), home.clone()),
    ]);
    let agent_dir = root
        .path()
        .join(".omo")
        .join("agent")
        .to_string_lossy()
        .into_owned();
    let lines = doctor_lines(root.path(), &env, &agent_dir);
    assert!(
        lines
            .iter()
            .any(|line| line == "WARN project resources: trust decision is not recorded")
    );
    assert!(!root.path().join(".omo").join("agent").exists());
}

#[test]
fn a_stored_anthropic_subscription_login_serves_its_pinned_category() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    let env = BTreeMap::from([
        (String::from("HOME"), home.clone()),
        (String::from("USERPROFILE"), home.clone()),
    ]);
    let agent = root.path().join(".omo").join("agent");
    std::fs::create_dir_all(&agent).expect("agent dir");
    std::fs::write(
        agent.join("auth.json"),
        r#"{"anthropic-subscription":{"type":"oauth","access":"fixture-access","refresh":"fixture-refresh","expires":4102444800000}}"#,
    )
    .expect("auth fixture");
    std::fs::write(
        root.path().join(".omo").join("omo.jsonc"),
        r#"{"categories":{"quick":{"model":"anthropic-subscription/claude-haiku-5-5"}}}"#,
    )
    .expect("config fixture");

    let lines = doctor_lines(root.path(), &env, &agent.to_string_lossy());

    let summary = lines
        .iter()
        .find(|line| line.contains("task categories:"))
        .expect("category summary");
    assert!(
        summary.starts_with("PASS ") || summary.contains("quick"),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("Category \"quick\"")),
        "{lines:?}"
    );
}

#[test]
fn category_report_is_warning_when_no_models_are_available() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    let env = BTreeMap::from([
        (String::from("HOME"), home.clone()),
        (String::from("USERPROFILE"), home.clone()),
    ]);
    let agent_dir = root
        .path()
        .join(".omo")
        .join("agent")
        .to_string_lossy()
        .into_owned();
    let lines = doctor_lines(root.path(), &env, &agent_dir);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("WARN task categories:"))
    );
    assert!(!root.path().join(".omo").join("agent").exists());
}

const FIX_NOW: f64 = 1_793_858_400_000.0;
const FIX_HOST: &str = "doctor-fix-host";

fn fix_host() -> String {
    FIX_HOST.to_string()
}

fn fix_now() -> f64 {
    FIX_NOW
}

fn every_pid_is_gone(_: u32) -> ProcessLiveness {
    ProcessLiveness::Dead
}

fn no_start_identity(_: u32) -> Option<String> {
    None
}

static FIX_HOST_FN: fn() -> String = fix_host;
static FIX_NOW_FN: fn() -> f64 = fix_now;
static EVERY_PID_IS_GONE: fn(u32) -> ProcessLiveness = every_pid_is_gone;
static NO_START_IDENTITY: fn(u32) -> Option<String> = no_start_identity;

fn fix_probe() -> StaleLockProbe<'static> {
    StaleLockProbe {
        base: ReflectionLockProbe {
            read_text: None,
            hostname: Some(&FIX_HOST_FN),
            pid_liveness: Some(&EVERY_PID_IS_GONE),
            process_start: Some(&NO_START_IDENTITY),
        },
        now: Some(&FIX_NOW_FN),
        list_directory: None,
        modified_at: None,
    }
}

fn write_launch_interrupted(memory_root: &std::path::Path, identity: &str) -> std::path::PathBuf {
    let runtime = memory_root.join("agents").join(identity).join("runtime");
    let reflection = runtime.join("reflection");
    std::fs::create_dir_all(&reflection).expect("reflection dir");
    let reserved_at =
        memory_core::support::time::format_rfc3339_millis((FIX_NOW - 10.0 * 60_000.0) as i64);
    std::fs::write(
        reflection.join("active.lock"),
        serde_json::json!({
            "runId": "reflection-run-64",
            "request": {"trigger": "manual", "conversationIds": [], "snapshots": []},
            "reservedAt": reserved_at,
            "launcherPid": 11096,
            "launcherHostname": FIX_HOST,
            "launcherProcessStart": null,
        })
        .to_string(),
    )
    .expect("active.lock");
    runtime
}

fn files_under(root: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                out.insert(name, Vec::new());
                walk(root, &path, out);
            } else {
                out.insert(name, std::fs::read(&path).expect("file"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn doctor_fix_releases_the_lock_of_a_launcher_that_died_before_its_run_started() {
    let root = tempfile::tempdir().expect("tempdir");
    let runtime = write_launch_interrupted(root.path(), "agent-a");

    let result = reflection_lock_fix(&ReflectionLockFixOptions {
        memory_root: root.path(),
        dry_run: false,
        probe: fix_probe(),
    });

    assert!(!result.failed);
    assert!(
        result.lines.contains(
            &"FIXED agent-a active.lock run reflection-run-64 (launch-interrupted)".to_string()
        ),
        "{:?}",
        result.lines
    );
    assert!(!runtime.join("reflection").join("active.lock").exists());
    assert!(
        runtime
            .join("reflection/runs/reflection-run-64/abandoned.json")
            .exists()
    );
}

#[test]
fn doctor_fix_dry_run_prints_the_decision_and_changes_nothing() {
    let root = tempfile::tempdir().expect("tempdir");
    write_launch_interrupted(root.path(), "agent-a");
    let before = files_under(root.path());

    let result = reflection_lock_fix(&ReflectionLockFixOptions {
        memory_root: root.path(),
        dry_run: true,
        probe: fix_probe(),
    });

    assert!(
        result.lines.contains(
            &"WOULD FIX agent-a active.lock run reflection-run-64 (launch-interrupted)".to_string()
        ),
        "{:?}",
        result.lines
    );
    assert_eq!(files_under(root.path()), before);
}

#[test]
fn doctor_fix_with_nothing_to_fix_says_so() {
    let root = tempfile::tempdir().expect("tempdir");

    let applied = reflection_lock_fix(&ReflectionLockFixOptions {
        memory_root: root.path(),
        dry_run: false,
        probe: fix_probe(),
    });
    let previewed = reflection_lock_fix(&ReflectionLockFixOptions {
        memory_root: root.path(),
        dry_run: true,
        probe: fix_probe(),
    });

    assert_eq!(
        applied.lines,
        vec!["PASS no stale reflection locks".to_string()]
    );
    assert_eq!(previewed.lines, vec!["INFO nothing to fix".to_string()]);
    assert!(!applied.failed && !previewed.failed);
}

#[test]
fn doctor_fix_reclaims_a_runtime_lock_whose_owner_is_gone_and_counts_it() {
    let root = tempfile::tempdir().expect("tempdir");
    let runtime = root.path().join("agents/agent-a/runtime");
    let locks = runtime.join("locks");
    std::fs::create_dir_all(&locks).expect("locks dir");
    let record = serde_json::json!({
        "pid": 2_000_000_000u32,
        "process_start": "gone",
        "hostname": memory_core::support::host::hostname(),
        "nonce": "n",
        "created_at": "2026-11-05T06:00:00.000Z",
        "purpose": "recall-sidecar",
    });
    std::fs::write(locks.join("recall-sidecar.x.lock"), format!("{record}\n")).expect("lock");

    let result = reflection_lock_fix(&ReflectionLockFixOptions {
        memory_root: root.path(),
        dry_run: false,
        probe: fix_probe(),
    });

    assert!(
        result
            .lines
            .contains(&"INFO agent-a locks reclaimed 1, kept 0".to_string()),
        "{:?}",
        result.lines
    );
    assert!(!locks.join("recall-sidecar.x.lock").exists());
}

#[test]
fn doctor_fix_keeps_a_lock_owned_by_another_host_and_still_fixes_its_sibling() {
    let root = tempfile::tempdir().expect("tempdir");
    let other = write_launch_interrupted(root.path(), "agent-other");
    std::fs::write(
        other.join("reflection/active.lock"),
        serde_json::json!({
            "runId": "reflection-run-64",
            "request": {"trigger": "manual", "conversationIds": [], "snapshots": []},
            "reservedAt": "2026-11-05T05:50:00.000Z",
            "launcherPid": 11096,
            "launcherHostname": "some-other-host",
            "launcherProcessStart": null,
        })
        .to_string(),
    )
    .expect("other host lock");
    let sibling = write_launch_interrupted(root.path(), "agent-sibling");

    let result = reflection_lock_fix(&ReflectionLockFixOptions {
        memory_root: root.path(),
        dry_run: false,
        probe: fix_probe(),
    });

    assert!(other.join("reflection/active.lock").exists());
    assert!(!sibling.join("reflection/active.lock").exists());
    assert!(
        result.lines.iter().any(|line| line.starts_with(
            "KEPT agent-other active.lock run reflection-run-64 (it is owned by host some-other-host"
        )),
        "{:?}",
        result.lines
    );
    assert!(result.lines.contains(
        &"FIXED agent-sibling active.lock run reflection-run-64 (launch-interrupted)".to_string()
    ));
}

/// omo 5.1.81 keeps ChatGPT sign-ins only inside `accounts[]`; doctor must count that provider as connected,
/// as the engine does, instead of failing the whole store.
#[test]
fn a_chatgpt_login_stored_only_in_accounts_serves_its_categories() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().to_string_lossy().into_owned();
    let env = BTreeMap::from([
        (String::from("HOME"), home.clone()),
        (String::from("USERPROFILE"), home.clone()),
    ]);
    let agent = root.path().join(".omo").join("agent");
    std::fs::create_dir_all(&agent).expect("agent dir");
    std::fs::write(
        agent.join("auth.json"),
        r#"{"chatgpt-subscription":{"type":"oauth","expires":4102444800000,"accountId":"fixture","accounts":[{"name":"login","source":"login","access":"fixture-access","refresh":"fixture-refresh","expires":4102444800000,"route":"siwc"}]}}"#,
    )
    .expect("auth fixture");
    std::fs::write(
        root.path().join(".omo").join("omo.jsonc"),
        r#"{"categories":{"quick":{"model":"chatgpt-subscription/gpt-6-luna-fast"}}}"#,
    )
    .expect("config fixture");

    let lines = doctor_lines(root.path(), &env, &agent.to_string_lossy());

    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("FAIL credentials")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("Category \"quick\"")),
        "{lines:?}"
    );
}
