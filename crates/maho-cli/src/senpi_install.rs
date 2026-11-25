//! Native counterpart of omo-senpi `src/install/`.
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct InstallOptions {
    pub home_dir: PathBuf,
    pub agent_dir: PathBuf,
    pub repo_root: PathBuf,
    pub plugin_path: PathBuf,
    pub cli_path: PathBuf,
    pub validate_artifacts: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct LauncherLeftAlone {
    pub file: &'static str,
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct InstallResult {
    pub ok: bool,
    pub action: &'static str,
    pub agent_dir: PathBuf,
    pub settings_path: PathBuf,
    pub plugin_path: PathBuf,
    pub launcher_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub launchers_left_alone: Vec<LauncherLeftAlone>,
    pub changed: bool,
    pub backup_path: PathBuf,
    pub removed: Option<bool>,
}

const ARTIFACTS: &[&str] = &[
    "extensions/omo.js",
    "extensions/omo-task.js",
    "extensions/omo-computer-use.js",
    "extensions/omo-memory-doctor.js",
    "extensions/omo-memory-memfs.js",
    "extensions/omo-memory-palace.js",
    "extensions/omo-session-history.js",
    "extensions/omo-recall-store.js",
    "extensions/omo-telemetry-posthog.js",
    "extensions/omo-memory-sync.js",
    "extensions/loro_wasm_bg.wasm",
    "extensions/CaseFolding.txt",
    "extensions/omo-thread-client.js",
    "extensions/omo-feedback.js",
    "extensions/assets.generated.json",
    "extensions/omo-member.js",
    "extensions/memory-run-supervisor.mjs",
    "extensions/gateway-store-worker.mjs",
    "extensions/reflection-persona.md",
    "extensions/dream-persona.md",
    "extensions/facts-persona.md",
    "extensions/kibitzer-persona.md",
    "skills/ast-grep/SKILL.md",
    "skills/browser/SKILL.md",
    "skills/browser/runtime/omowright/index.js",
    "skills/browser/runtime/omowright/page-bundle.js",
    "skills/coding-agent-sessions/SKILL.md",
    "skills/debugging/SKILL.md",
    "skills/frontend/SKILL.md",
    "skills/git-master/SKILL.md",
    "skills/init-deep/SKILL.md",
    "skills/lsp-setup/SKILL.md",
    "skills/programming/SKILL.md",
    "skills/refactor/SKILL.md",
    "skills/remove-ai-slops/SKILL.md",
    "skills/review-work/SKILL.md",
    "skills/ultimate-browsing/SKILL.md",
    "skills/ultrawork/SKILL.md",
    "skills/ultrawork/references/commits.md",
    "skills/ultrawork/references/delegation-and-teams.md",
    "skills/ultrawork/references/qa-channels.md",
    "skills/ultrawork/references/review-gate.md",
    "skills/ultrawork/references/tui-visual-qa.md",
    "skills/ulw-execute/SKILL.md",
    "skills/ulw-loop/SKILL.md",
    "skills/ulw-plan/SKILL.md",
    "skills/ulw-research/SKILL.md",
    "skills/visual-qa/SKILL.md",
    "skills-conditional/x-search/SKILL.md",
    "runtime/agent-toolkit-sdk/sdk.js",
    "runtime/thread-sdk/sdk.js",
    "runtime/ast-grep-mcp/cli.js",
    "runtime/lsp-daemon/dist/cli.js",
    "runtime/lsp-daemon/dist/index.js",
    "runtime/lsp-daemon/dist/index.d.ts",
    "runtime/lsp-daemon/dist/daemon-client.js",
    "runtime/lsp-daemon/dist/daemon-client.d.ts",
    "runtime/lsp-daemon/dist/package.json",
    "runtime/lsp-daemon/dist/.omo-runtime-manifest.json",
    "scripts/install.mjs",
    "daemon-launch-spec.json",
];

pub fn install(options: &InstallOptions) -> Result<InstallResult, String> {
    install_with_platform(options, current_platform())
}

pub fn install_with_platform(
    options: &InstallOptions,
    platform: &str,
) -> Result<InstallResult, String> {
    if options.validate_artifacts {
        validate_plugin(&options.plugin_path)?;
    }
    let settings_path = options.agent_dir.join("settings.json");
    let mut settings = read_settings(&settings_path)?;
    let before = settings.clone();
    let mut packages = package_list(&settings)?;
    let mut seen = HashSet::new();
    packages.retain(|entry| {
        let full = resolve(&options.agent_dir, Path::new(entry));
        let shadows = ["packages/pi-goal", "packages/pi-webfetch"]
            .iter()
            .map(|p| resolve(&options.repo_root, Path::new(p)))
            .any(|p| p == full);
        let current = resolve(Path::new("."), &options.plugin_path);
        let package_name = fs::read_to_string(full.join("package.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_owned));
        !shadows
            && (full == current || package_name.as_deref() != Some("@code-yeongyu/omo-senpi"))
            && seen.insert(entry.clone())
    });
    let plugin = resolve(Path::new("."), &options.plugin_path)
        .to_string_lossy()
        .into_owned();
    if !packages.contains(&plugin) {
        packages.push(plugin.clone());
    }
    settings.insert(
        "packages".into(),
        Value::Array(packages.into_iter().map(Value::String).collect()),
    );
    let backup_path = write_settings(&settings_path, &settings)?;
    let launcher_path = install_launcher(options, platform)?;
    let launchers_left_alone = left_alone_report(&options.home_dir, platform);
    Ok(InstallResult {
        ok: true,
        action: "install",
        agent_dir: options.agent_dir.clone(),
        settings_path,
        plugin_path: PathBuf::from(plugin),
        launcher_path,
        launchers_left_alone,
        changed: settings != before,
        backup_path,
        removed: None,
    })
}

pub fn uninstall(options: &InstallOptions) -> Result<InstallResult, String> {
    uninstall_with_platform(options, current_platform())
}

pub fn uninstall_with_platform(
    options: &InstallOptions,
    platform: &str,
) -> Result<InstallResult, String> {
    let settings_path = options.agent_dir.join("settings.json");
    let mut settings = read_settings(&settings_path)?;
    let before = settings.clone();
    let packages = package_list(&settings)?;
    let plugin = resolve(Path::new("."), &options.plugin_path)
        .to_string_lossy()
        .into_owned();
    let next: Vec<_> = packages
        .into_iter()
        .filter(|entry| entry != &plugin)
        .collect();
    let removed = next.len() != package_list(&settings)?.len();
    settings.insert(
        "packages".into(),
        Value::Array(next.into_iter().map(Value::String).collect()),
    );
    let backup_path = write_settings(&settings_path, &settings)?;
    remove_launcher(&options.home_dir)?;
    let launchers_left_alone = left_alone_report(&options.home_dir, platform);
    Ok(InstallResult {
        ok: true,
        action: "uninstall",
        agent_dir: options.agent_dir.clone(),
        settings_path,
        plugin_path: PathBuf::from(plugin),
        launcher_path: None,
        launchers_left_alone,
        changed: settings != before,
        backup_path,
        removed: Some(removed),
    })
}

fn read_settings(path: &Path) -> Result<Map<String, Value>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .as_object()
            .cloned()
            .ok_or_else(|| format!("{} must contain a JSON object", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(e.to_string()),
    }
}

fn package_list(settings: &Map<String, Value>) -> Result<Vec<String>, String> {
    match settings.get("packages") {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "Senpi settings packages must be an array of strings".to_owned())
            })
            .collect(),
        _ => Err("Senpi settings packages must be an array of strings".into()),
    }
}

fn write_settings(path: &Path, settings: &Map<String, Value>) -> Result<PathBuf, String> {
    let parent = path.parent().ok_or("settings path has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let base = format!("{}.{}.backup", path.display(), stamp);
    let mut backup = PathBuf::from(&base);
    for i in 1..1000 {
        if backup.exists() {
            backup = PathBuf::from(format!("{}-{i}.backup", base.trim_end_matches(".backup")));
        } else {
            break;
        }
    }
    if path.exists() {
        fs::copy(path, &backup).map_err(|e| e.to_string())?;
    } else {
        fs::write(&backup, "{}\n").map_err(|e| e.to_string())?;
    }
    let temp = path.with_extension(format!(
        "{}.{}.tmp",
        path.extension().and_then(|s| s.to_str()).unwrap_or("json"),
        std::process::id()
    ));
    fs::write(
        &temp,
        format!(
            "{}\n",
            serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?
        ),
    )
    .map_err(|e| e.to_string())?;
    fs::rename(temp, path).map_err(|e| e.to_string())?;
    Ok(backup)
}

fn validate_plugin(root: &Path) -> Result<(), String> {
    for artifact in ARTIFACTS {
        if !root.join(artifact).is_file() {
            return Err(format!(
                "Packed omo-senpi plugin is missing required runtime artifacts at {}",
                root.display()
            ));
        }
    }
    let runtime = root.join("runtime/ast-grep-mcp/cli.js");
    let manifest_path = root.join("runtime/ast-grep-mcp/manifest.json");
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).map_err(|e| format!("Packed omo-senpi plugin ast-grep MCP runtime integrity error at {}: manifest is missing or unreadable: {e}", runtime.display()))?).map_err(|e| format!("Packed omo-senpi plugin ast-grep MCP runtime integrity error at {}: manifest is invalid: {e}", runtime.display()))?;
    let bytes = fs::read(&runtime).map_err(|e| format!("Packed omo-senpi plugin ast-grep MCP runtime integrity error at {}: runtime unreadable: {e}", runtime.display()))?;
    use sha2::Digest;
    let actual = format!("{:x}", sha2::Sha256::digest(bytes));
    if manifest.get("sha256").and_then(Value::as_str) != Some(&actual) {
        return Err(format!(
            "Packed omo-senpi plugin ast-grep MCP runtime integrity error at {}: sha256 mismatch",
            runtime.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = manifest.get("mode").and_then(Value::as_u64) {
            let actual_mode = fs::metadata(&runtime)
                .map_err(|e| e.to_string())?
                .permissions()
                .mode()
                & 0o777;
            if actual_mode != mode as u32 {
                return Err(format!(
                    "Packed omo-senpi plugin ast-grep MCP runtime integrity error at {}: mode mismatch",
                    runtime.display()
                ));
            }
        }
    }
    Ok(())
}

/// The OmO icon as a base64 SVG data URI: the `logo` the launcher puts in `SENPI_BRAND`
/// (upstream `omo-native/bin/lib/brand-logo.generated.js`, omo v5.1.52).
const OMO_BRAND_LOGO: &str = "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAxMDI0IDEwMjQiPgogIDxwYXRoIGZpbGw9IiNGNEY0RjQiIGQ9Ik01MTIuMDAgMTAwLjUwIEM1OTEuMTMgMTAwLjUwIDY5Ny42NiAxMDIuMzcgNzQ5LjM5IDEwNi4yNyBDODAxLjExIDExMC4xNyA4MDIuMzUgMTE1Ljg0IDgyMi4zNSAxMjMuOTAgQzg0Mi4zNCAxMzEuOTYgODU2LjQwIDE0MS42OCA4NjkuMzYgMTU0LjY0IEM4ODIuMzIgMTY3LjYwIDg5Mi4wNCAxODEuNjYgOTAwLjEwIDIwMS42NSBDOTA4LjE2IDIyMS42NSA5MTMuODMgMjIyLjg5IDkxNy43MyAyNzQuNjEgQzkyMS42MyAzMjYuMzQgOTIzLjUwIDQzMi44NyA5MjMuNTAgNTEyLjAwIEM5MjMuNTAgNTkxLjEzIDkyMS42MyA2OTcuNjYgOTE3LjczIDc0OS4zOSBDOTEzLjgzIDgwMS4xMSA5MDguMTYgODAyLjM1IDkwMC4xMCA4MjIuMzUgQzg5Mi4wNCA4NDIuMzQgODgyLjMyIDg1Ni40MCA4NjkuMzYgODY5LjM2IEM4NTYuNDAgODgyLjMyIDg0Mi4zNCA4OTIuMDQgODIyLjM1IDkwMC4xMCBDODAyLjM1IDkwOC4xNiA4MDEuMTEgOTEzLjgzIDc0OS4zOSA5MTcuNzMgQzY5Ny42NiA5MjEuNjMgNTkxLjEzIDkyMy41MCA1MTIuMDAgOTIzLjUwIEM0MzIuODcgOTIzLjUwIDMyNi4zNCA5MjEuNjMgMjc0LjYxIDkxNy43MyBDMjIyLjg5IDkxMy44MyAyMjEuNjUgOTA4LjE2IDIwMS42NSA5MDAuMTAgQzE4MS42NiA4OTIuMDQgMTY3LjYwIDg4Mi4zMiAxNTQuNjQgODY5LjM2IEMxNDEuNjggODU2LjQwIDEzMS45NiA4NDIuMzQgMTIzLjkwIDgyMi4zNSBDMTE1Ljg0IDgwMi4zNSAxMTAuMTcgODAxLjExIDEwNi4yNyA3NDkuMzkgQzEwMi4zNyA2OTcuNjYgMTAwLjUwIDU5MS4xMyAxMDAuNTAgNTEyLjAwIEMxMDAuNTAgNDMyLjg3IDEwMi4zNyAzMjYuMzQgMTA2LjI3IDI3NC42MSBDMTEwLjE3IDIyMi44OSAxMTUuODQgMjIxLjY1IDEyMy45MCAyMDEuNjUgQzEzMS45NiAxODEuNjYgMTQxLjY4IDE2Ny42MCAxNTQuNjQgMTU0LjY0IEMxNjcuNjAgMTQxLjY4IDE4MS42NiAxMzEuOTYgMjAxLjY1IDEyMy45MCBDMjIxLjY1IDExNS44NCAyMjIuODkgMTEwLjE3IDI3NC42MSAxMDYuMjcgQzMyNi4zNCAxMDIuMzcgNDMyLjg3IDEwMC41MCA1MTIuMDAgMTAwLjUwWiIvPgogIDxnIHRyYW5zZm9ybT0ic2NhbGUoMC41KSI+CiAgICA8ZyB0cmFuc2Zvcm09InRyYW5zbGF0ZSgwLjAwMDAwMCwyMDQ4LjAwMDAwMCkgc2NhbGUoMC4wNTAwMDAsLTAuMDUwMDAwKSI+CiAgICAgIDxwYXRoIGZpbGw9IiMwNDE2MTciIGZpbGwtcnVsZT0iZXZlbm9kZCIgZD0iTTkyNzAgMzIxNjcgYy0zMDkgLTY0IC00NTYgLTE1MiAtNjIxIC0zNzMgLTU2MiAtNzQ4IC03NzYgLTI2ODkKLTU0NyAtNDk4NCAxMTggLTExODcgMzI1IC0yNjAwIDU3MCAtMzg5MCAxNDMgLTc1MyAxNTIgLTY1MyAtMTI4IC0xMzgwIC00MDMKLTEwNDUgLTY0MyAtMTg3MyAtODEyIC0yODAwIC04NTMgLTQ2ODkgMTk3OSAtNzk4NiA3ODU4IC05MTQ4IDExNDAgLTIyNSAyNTE0Ci0zOTcgMzIwMiAtNDAwIGwzNjEgLTIgNzkgNTUgYzI2NCAxODIgMjEwIDY0MiAtMTExIDk0MiAtMzg3IDM2MiAtOTUwIDUzMgotMjgwMSA4NDUgLTMwMDQgNTA3IC00MjQ5IDk2NSAtNTUyMCAyMDMwIC0xMzU4IDExMzggLTIwMTIgMjg3OCAtMTgwOSA0ODEwCjU0NSA1MTk1IDY2MTcgODcyNiAxMzUyOSA3ODY2IDU1MDYgLTY4NCA5NDgwIC00MjU1IDk0ODAgLTg1MTggMCAtMjYwNyAtMTQ2MgotNDUyMiAtNDE1NyAtNTQ0OCAtOTI3IC0zMTggLTE1MzQgLTQ1OSAtMzIwMyAtNzQwIC0xODUxIC0zMTMgLTI0MTQgLTQ4MwotMjgwMSAtODQ1IC0zMjEgLTMwMCAtMzc1IC03NjAgLTExMSAtOTQyIGw3OSAtNTUgMzYxIDIgYzY4OCAzIDIwNjIgMTc1IDMyMDIKNDAwIDU4NzkgMTE2MiA4NzExIDQ0NTkgNzg1OCA5MTQ4IC0xNjkgOTI3IC00MDkgMTc1NSAtODEyIDI4MDAgLTI4MCA3MjcKLTI3MSA2MjcgLTEyOCAxMzgwIDI0NSAxMjkwIDQ1MiAyNzAzIDU3MCAzODkwIDI3MSAyNzIwIC03NyA0ODYxIC04NTUgNTI1OQotNzU4IDM4OCAtMjMxMCAtMjM4IC01MDIxIC0yMDI0IC04NjggLTU3MSAtNzY0IC01NTcgLTE0ODkgLTIxMSAtMzI3MSAxNTU4Ci02NzU1IDE1NTggLTEwMDI2IDAgLTcyNSAtMzQ2IC02MjEgLTM2MCAtMTQ4OSAyMTEgLTIzNzEgMTU2MiAtMzk1OSAyMjc4Ci00NzA4IDIxMjJ6IG00MTMyIC0zMzQ1IGMxNzUgLTE3NSAxMjQgLTI2OCAtNDA5IC03MzkgLTEwMjggLTkxMCAtMTk5NSAtMjA1MwotMjcxMyAtMzIwOSAtOTcgLTE1NiAtMjAwIC0zMTggLTIyOCAtMzU5IC0yMjEgLTMyMSAtNjQ2IC03NiAtNDY5IDI3MCAxNTMKMzAxIDg4MyAxMzM0IDEzODggMTk2NSA1NzggNzIzIDE0NjYgMTYxNCAyMDI5IDIwMzUgMTkxIDE0MiAyODggMTUxIDQwMiAzN3oKbTE0NTA3IC0xIGM0OTYgLTM0MSAxNDgyIC0xMzIzIDIwODAgLTIwNzEgNTA1IC02MzEgMTIzNSAtMTY2NCAxMzg4IC0xOTY1CjE3NyAtMzQ2IC0yNDggLTU5MSAtNDY5IC0yNzAgLTI4IDQxIC0xMzEgMjAzIC0yMjggMzU5IC03MTggMTE1NiAtMTY4NSAyMjk5Ci0yNzEzIDMyMDkgLTUzNSA0NzMgLTU4NSA1NjQgLTQwNyA3NDIgMTAwIDEwMCAxOTkgOTkgMzQ5IC00eiBNMTk5MTAgMjUzMDggYy00MDM5IC0yMDUgLTcyMTIgLTE2NjYgLTkwODYgLTQxODQgLTE4NDAgLTI0NzMgLTE3NTMKLTU3OTQgMTk2IC03NTQwIDExNjEgLTEwNDAgMjQ0NyAtMTUxOCA1NDIwIC0yMDE1IDEzNzQgLTIyOSAxOTk4IC0zODAgMjQ1NgotNTkxIDc1NCAtMzQ4IDExNjMgLTEwMDAgMTAzMyAtMTY1MCAtMTAgLTQ5IC0xIC03MiA0NyAtMTI0IGw2MCAtNjQgNDQ0IDAKNDQ0IDAgNjAgNjQgYzQ4IDUyIDU3IDc1IDQ3IDEyNCAtMTMwIDY1MCAyNzkgMTMwMiAxMDMzIDE2NTAgNDU4IDIxMSAxMDgyCjM2MiAyNDU2IDU5MSAyOTczIDQ5NyA0MjU5IDk3NSA1NDIwIDIwMTUgMjA4NCAxODY2IDIwMjYgNTM3NCAtMTMxIDc5NTYKLTIwNjMgMjQ2OSAtNTk5NSAzOTY2IC05ODk5IDM3Njh6IG0tNDY0NCAtNDM3NCBjMTM1NiAtMzYwIDIyNzIgLTE5MDkgMTk5OAotMzM3OCAtNDI2IC0yMjc4IC0yODU2IC0zMTQ0IC00NDI0IC0xNTc2IC0xNDk3IDE0OTYgLTkyNCA0MjE4IDEwMzEgNDg5OSAzOTMKMTM3IDk5NiAxNjEgMTM5NSA1NXogbTExNDgwIDM1IGMxNDkxIC0yNjEgMjUwNCAtMTkyMSAyMTU0IC0zNTMyIC00ODcgLTIyNDcKLTMxMDggLTMwMjQgLTQ1MjUgLTEzNDEgLTE3MTkgMjA0MiAtMTI5IDUzMTAgMjM3MSA0ODczeiBtLTY5NDggLTI5ODAgYzE3MgotNDAgMzQxIC0xMzMgNDk1IC0yNzMgOTUgLTg2IDE0MyAtMTE2IDE4NyAtMTE2IDQ0IDAgOTIgMzAgMTg3IDExNiA3MTggNjUwCjE3MTYgMTg4IDE4NTQgLTg2MCAzNSAtMjY3IDE4IC05NjggLTI1IC0xMDQ4IC0xMTkgLTIxOSAtMzUwIC0yNTEgLTQ5MSAtNjgKLTY4IDg5IC03MyAxMjQgLTg1IDY3MCAtMTMgNjIzIC0yNSA2NzUgLTIwMCA4MzggLTIxMiAxOTggLTUxNiAxOTkgLTczNSAyCi0xODAgLTE2MSAtMTkyIC0yMTIgLTIwNSAtODQwIC0xMiAtNTQ2IC0xNyAtNTgxIC04NSAtNjcwIC0xMjIgLTE1OSAtMzA4Ci0xNTkgLTQzMCAwIC02OCA4OSAtNzMgMTI0IC04NSA2NzAgLTEzIDYyOCAtMjUgNjc5IC0yMDUgODQwIC0yMTkgMTk3IC01MjMKMTk2IC03MzUgLTIgLTE3NSAtMTYzIC0xODcgLTIxNSAtMjAwIC04MzggLTEyIC01NDYgLTE3IC01ODEgLTg1IC02NzAgLTE0MQotMTgzIC0zNzIgLTE1MSAtNDkxIDY4IC03NCAxMzYgLTU0IDEwMzkgMzAgMTMzMiAxNzAgNTk0IDc1OCA5NzggMTMwNCA4NDl6IE0xNDM0MCAyMDI5MCBjLTE4NTggLTM3MiAtMjIzMCAtMzIyOCAtNTQ2IC00MTkxIDEyODAgLTczMiAyNzg2IDMzOAoyNzg2IDE5ODEgMCAxMzkxIC0xMDY4IDI0NDUgLTIyNDAgMjIxMHogTTI1OTkwIDIwMjkxIGMtMTczMyAtMzQzIC0yMjA4IC0yOTgxIC03MzIgLTQwNjkgMTI4NyAtOTQ5IDI5ODEgMTA4CjI5ODIgMTg2MCAwIDEzOTIgLTEwNzAgMjQ0MyAtMjI1MCAyMjA5eiIvPgogICAgPC9nPgogIDwvZz4KPC9zdmc+Cg==";

const LAUNCHER_HEADER: &str = "#!/usr/bin/env node\n// omo-local-launcher v2: generated by the omo-senpi installer. Delete this file to remove the local launcher.\n";
const LEGACY_OMO_SENPI_HEADER: &str = "#!/usr/bin/env node\n// omo-local-launcher: generated by the omo-senpi installer. Delete this file to remove the local launcher.\n";
const LEGACY_SHORT_HEADER: &str =
    "#!/usr/bin/env node\n// omo-local-launcher: generated by installer\n";
const LEGACY_MAHO_HEADER: &str =
    "#!/usr/bin/env node\n// omo-local-launcher: generated by the maho installer.\n";
const OWNED_HEADERS: &[&str] = &[
    LAUNCHER_HEADER,
    LEGACY_OMO_SENPI_HEADER,
    LEGACY_SHORT_HEADER,
    LEGACY_MAHO_HEADER,
];
const LAUNCHER_CMD_SHIM: &str = "@echo off\r\nnode \"%~dp0omo\" %*\r\n";
const MAX_SHIM_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
enum LauncherOwnership {
    Absent,
    Owned,
    Foreign { reason: String },
}

fn current_platform() -> &'static str {
    if cfg!(windows) { "win32" } else { "linux" }
}

fn local_launcher_path(home: &Path) -> PathBuf {
    home.join(".local/bin/omo")
}

fn local_launcher_cmd_path(home: &Path) -> PathBuf {
    home.join(".local/bin/omo.cmd")
}

fn read_prefix(path: &Path, length: usize) -> std::io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut buffer = Vec::with_capacity(length);
    file.take(length as u64).read_to_end(&mut buffer)?;
    Ok(buffer)
}

fn classify_launcher(
    path: &Path,
    prefix_bytes: usize,
    is_owned: impl FnOnce(&[u8], u64) -> bool,
    foreign_reason: &'static str,
) -> LauncherOwnership {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LauncherOwnership::Absent;
        }
        Err(error) => {
            return LauncherOwnership::Foreign {
                reason: format!("it cannot be inspected ({error})"),
            };
        }
    };
    if metadata.file_type().is_symlink() {
        return LauncherOwnership::Foreign {
            reason: "it is a symbolic link to another file".to_owned(),
        };
    }
    if !metadata.file_type().is_file() {
        return LauncherOwnership::Foreign {
            reason: "it is not a regular file".to_owned(),
        };
    }
    if metadata.len() > MAX_SHIM_BYTES {
        return LauncherOwnership::Foreign {
            reason: format!(
                "it is {} bytes, larger than any launcher this installer writes",
                metadata.len()
            ),
        };
    }
    let prefix = match read_prefix(path, prefix_bytes) {
        Ok(prefix) => prefix,
        Err(error) => {
            return LauncherOwnership::Foreign {
                reason: format!("it cannot be read ({error})"),
            };
        }
    };
    if is_owned(&prefix, metadata.len()) {
        LauncherOwnership::Owned
    } else {
        LauncherOwnership::Foreign {
            reason: foreign_reason.to_owned(),
        }
    }
}

fn launcher_ownership(path: &Path) -> LauncherOwnership {
    let max_header = OWNED_HEADERS
        .iter()
        .map(|header| header.len())
        .max()
        .unwrap_or(0);
    classify_launcher(
        path,
        max_header,
        |prefix, _| {
            OWNED_HEADERS
                .iter()
                .any(|header| prefix.starts_with(header.as_bytes()))
        },
        "it does not start with this installer's launcher header",
    )
}

fn launcher_cmd_ownership(path: &Path) -> LauncherOwnership {
    classify_launcher(
        path,
        LAUNCHER_CMD_SHIM.len(),
        |prefix, size| {
            size == LAUNCHER_CMD_SHIM.len() as u64 && prefix == LAUNCHER_CMD_SHIM.as_bytes()
        },
        "it does not match this installer's wrapper",
    )
}

fn left_alone_report(home: &Path, platform: &str) -> Vec<LauncherLeftAlone> {
    let mut left_alone = Vec::new();
    let omo_path = local_launcher_path(home);
    if let LauncherOwnership::Foreign { reason } = launcher_ownership(&omo_path) {
        left_alone.push(LauncherLeftAlone {
            file: "omo",
            reason: format!(
                "{} was left alone: it is not a launcher this installer wrote ({reason})",
                omo_path.display()
            ),
            path: omo_path,
        });
    }
    if platform == "win32" {
        let cmd_path = local_launcher_cmd_path(home);
        if let LauncherOwnership::Foreign { reason } = launcher_cmd_ownership(&cmd_path) {
            left_alone.push(LauncherLeftAlone {
                file: "omo.cmd",
                reason: format!(
                    "{} was left alone: it is not a wrapper this installer wrote ({reason}); the command omo still resolves to it",
                    cmd_path.display()
                ),
                path: cmd_path,
            });
        }
    }
    left_alone
}

fn install_launcher(options: &InstallOptions, platform: &str) -> Result<Option<PathBuf>, String> {
    let path = local_launcher_path(&options.home_dir);
    if matches!(launcher_ownership(&path), LauncherOwnership::Foreign { .. }) {
        return Ok(None);
    }
    let parent = path.parent().ok_or("launcher path has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let brand = serde_json::json!({
        "name": "OmO",
        "logo": OMO_BRAND_LOGO,
        "command": "omo",
        "displayVersion": "local",
        "configDir": ".omo",
        "flatLayout": false,
        "envPrefix": "OMO",
        "userAgent": "omo",
        "originator": "omo",
        "changelog": {"path": options.plugin_path.join("CHANGELOG.md")},
        "extensions": [options.plugin_path],
        "update": {
            "packageName": "omo-ai",
            "distTag": "latest",
            "command": "bun add -g omo-ai",
            "changelogUrl": "https://github.com/code-yeongyu/oh-my-openagent/releases"
        }
    });
    let mut script = String::from(LAUNCHER_HEADER);
    script.push_str(&format!(
        "import {{ spawn }} from 'node:child_process';\nimport {{ join }} from 'node:path';\nconst env={{...process.env,SENPI_BRAND:JSON.stringify({}),OMO_NATIVE:'1',OMO_CODING_AGENT_DIR:process.env.OMO_CODING_AGENT_DIR??process.env.SENPI_CODING_AGENT_DIR??process.env.PI_CODING_AGENT_DIR??join(process.env.HOME,'.omo','agent'),SENPI_CODING_AGENT_DIR:process.env.OMO_CODING_AGENT_DIR??process.env.SENPI_CODING_AGENT_DIR??process.env.PI_CODING_AGENT_DIR??join(process.env.HOME,'.omo','agent')}};\nenv.OMO_BIN=process.argv[1];\nconst child=spawn(process.execPath,[{},'--extension',{},...process.argv.slice(2)],{{env,stdio:'inherit'}});\nchild.on('close',(code,signal)=>{{if(signal)process.kill(process.pid,signal);else process.exitCode=code??1;}});\n",
        serde_json::to_string(&brand).map_err(|e| e.to_string())?,
        serde_json::to_string(&options.cli_path).map_err(|e| e.to_string())?,
        serde_json::to_string(&options.plugin_path).map_err(|e| e.to_string())?
    ));
    fs::write(&path, script).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    if platform == "win32" {
        let cmd = local_launcher_cmd_path(&options.home_dir);
        if !matches!(
            launcher_cmd_ownership(&cmd),
            LauncherOwnership::Foreign { .. }
        ) {
            fs::write(&cmd, LAUNCHER_CMD_SHIM).map_err(|e| e.to_string())?;
        }
    }
    Ok(Some(path))
}

fn remove_launcher(home: &Path) -> Result<bool, String> {
    let path = local_launcher_path(home);
    if !matches!(launcher_ownership(&path), LauncherOwnership::Owned) {
        return Ok(false);
    }
    fs::remove_file(&path).map_err(|e| e.to_string())?;
    let cmd = local_launcher_cmd_path(home);
    if matches!(launcher_cmd_ownership(&cmd), LauncherOwnership::Owned) {
        fs::remove_file(&cmd).map_err(|e| e.to_string())?;
    }
    Ok(true)
}

fn resolve(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn install_preserves_settings_and_deduplicates_plugin_path() {
        let root = tempfile::tempdir().expect("temp");
        let home = root.path().join("home");
        let agent = root.path().join("agent");
        let plugin = root.path().join("plugin");
        fs::create_dir_all(&agent).expect("agent");
        let initial_settings = serde_json::json!({
            "theme": "dark",
            "packages": ["keep", "keep", plugin.to_string_lossy()]
        });
        fs::write(
            agent.join("settings.json"),
            serde_json::to_string(&initial_settings).expect("settings json"),
        )
        .expect("settings");
        let options = InstallOptions {
            home_dir: home,
            agent_dir: agent.clone(),
            repo_root: root.path().into(),
            plugin_path: plugin.clone(),
            cli_path: root.path().join("cli.js"),
            validate_artifacts: false,
        };
        install(&options).expect("install");
        let value: Value =
            serde_json::from_slice(&fs::read(agent.join("settings.json")).expect("written"))
                .expect("json");
        assert_eq!(value["theme"], "dark");
        assert_eq!(
            value["packages"],
            serde_json::json!(["keep", plugin.to_string_lossy()])
        );
    }

    #[test]
    fn launcher_brand_carries_the_omo_logo_and_injected_extension() {
        let root = tempfile::tempdir().expect("temp");
        let plugin = root.path().join("plugin");
        let options = InstallOptions {
            home_dir: root.path().join("home"),
            agent_dir: root.path().join("agent"),
            repo_root: root.path().into(),
            plugin_path: plugin.clone(),
            cli_path: root.path().join("cli.js"),
            validate_artifacts: false,
        };
        let path = install_launcher(&options, "linux")
            .expect("launcher")
            .expect("path");
        let script = fs::read_to_string(path).expect("script");
        assert!(script.contains("\"logo\":\"data:image/svg+xml;base64,"));
        let marker = "SENPI_BRAND:JSON.stringify(";
        let start = script.find(marker).expect("SENPI_BRAND") + marker.len();
        let end = script[start..].find("),OMO_NATIVE").expect("end of brand") + start;
        let brand: Value = serde_json::from_str(&script[start..end]).expect("brand json");
        assert_eq!(brand["extensions"], serde_json::json!([plugin]));
    }

    #[test]
    fn compiled_binary_directory_and_oversized_launchers_are_left_alone_and_reported() {
        let root = tempfile::tempdir().expect("temp");
        let home = root.path().join("home");
        let agent = root.path().join("agent");
        let plugin = root.path().join("plugin");
        fs::create_dir_all(home.join(".local/bin")).expect("bin");
        let launcher_path = local_launcher_path(&home);

        let mut binary = vec![0x7f, 0x45, 0x4c, 0x46, 0x02, 0x01, 0x01, 0x00];
        binary.extend(vec![0x90_u8; 4096]);
        binary.extend_from_slice(LEGACY_SHORT_HEADER.as_bytes());
        binary.extend(vec![0x00_u8; 4096]);
        fs::write(&launcher_path, &binary).expect("binary");

        let options = InstallOptions {
            home_dir: home.clone(),
            agent_dir: agent,
            repo_root: root.path().into(),
            plugin_path: plugin,
            cli_path: root.path().join("cli.js"),
            validate_artifacts: false,
        };
        let installed = install_with_platform(&options, "linux").expect("install");
        assert_eq!(installed.launcher_path, None);
        assert_eq!(fs::read(&launcher_path).expect("read"), binary);
        assert_eq!(
            installed
                .launchers_left_alone
                .iter()
                .map(|entry| (entry.file, entry.path.clone()))
                .collect::<Vec<_>>(),
            vec![("omo", launcher_path.clone())]
        );

        let uninstalled = uninstall_with_platform(&options, "linux").expect("uninstall");
        assert!(launcher_path.exists());
        assert_eq!(
            uninstalled
                .launchers_left_alone
                .iter()
                .map(|entry| (entry.file, entry.path.clone()))
                .collect::<Vec<_>>(),
            vec![("omo", launcher_path.clone())]
        );

        fs::remove_file(&launcher_path).expect("rm");
        let mut oversized = LEGACY_SHORT_HEADER.as_bytes().to_vec();
        oversized.extend(vec![b'a'; 128 * 1024]);
        fs::write(&launcher_path, &oversized).expect("oversized");
        let oversized_res = install_with_platform(&options, "linux").expect("install");
        assert_eq!(oversized_res.launcher_path, None);
        assert_eq!(oversized_res.launchers_left_alone.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_dangling_symlink_launchers_are_reported_as_foreign() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("temp");
        let home = root.path().join("home");
        let agent = root.path().join("agent");
        let plugin = root.path().join("plugin");
        fs::create_dir_all(home.join(".local/bin")).expect("bin");
        let launcher_path = local_launcher_path(&home);
        symlink(home.join("gone/omo"), &launcher_path).expect("dangling");

        let options = InstallOptions {
            home_dir: home,
            agent_dir: agent,
            repo_root: root.path().into(),
            plugin_path: plugin,
            cli_path: root.path().join("cli.js"),
            validate_artifacts: false,
        };
        let res = install_with_platform(&options, "linux").expect("install");
        assert_eq!(res.launcher_path, None);
        assert_eq!(res.launchers_left_alone.len(), 1);
        assert!(
            fs::symlink_metadata(&launcher_path)
                .expect("lstat")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn windows_install_reports_exact_left_alone_launchers() {
        let foreign_cmd = "@echo off\r\nC:\\other\\omo.exe %*\r\n";

        for (omo_owned, cmd_owned, expected) in [
            (true, false, vec!["omo.cmd"]),
            (false, true, vec!["omo"]),
            (false, false, vec!["omo", "omo.cmd"]),
            (true, true, vec![]),
        ] {
            let root = tempfile::tempdir().expect("temp");
            let home = root.path().join("home");
            let agent = root.path().join("agent");
            let plugin = root.path().join("plugin");
            fs::create_dir_all(home.join(".local/bin")).expect("bin");
            let options = InstallOptions {
                home_dir: home.clone(),
                agent_dir: agent,
                repo_root: root.path().into(),
                plugin_path: plugin,
                cli_path: root.path().join("cli.js"),
                validate_artifacts: false,
            };
            if omo_owned {
                install_launcher(&options, "linux").expect("own omo");
            } else {
                fs::write(local_launcher_path(&home), "#!/bin/sh\necho foreign\n").expect("omo");
            }
            fs::write(
                local_launcher_cmd_path(&home),
                if cmd_owned {
                    LAUNCHER_CMD_SHIM
                } else {
                    foreign_cmd
                },
            )
            .expect("cmd");

            let res = install_with_platform(&options, "win32").expect("install");
            let actual: Vec<&str> = res
                .launchers_left_alone
                .iter()
                .map(|entry| entry.file)
                .collect();
            assert_eq!(actual, expected);
            assert_eq!(
                res.launcher_path,
                if omo_owned {
                    Some(local_launcher_path(&home))
                } else {
                    None
                }
            );
        }
    }
}
