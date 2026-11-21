use crate::support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use maho_ext_api::SessionReason;
use maho_ext_host::ExtensionRunner;
use maho_omo_bundled_skills::{
    BundledSkillProvenance, BundledSkillsComponent, BundledSkillsComponentOptions,
    ContributedSkill, ENABLE_EVAL_INTEGRITY_ENV, HostCommandInfo, ResolveContributedSkillOptions,
    is_opt_in_skill_enabled, resolve_bundled_skill_provenance, resolve_contributed_skill,
};

const EXPECTED_SKILLS: [&str; 28] = [
    "adversarial-plan",
    "ast-grep",
    "browser",
    "coding-agent-sessions",
    "dag-library",
    "data-scientist",
    "debugging",
    "eval-integrity",
    "frontend",
    "git-master",
    "give-me-tips",
    "hyperplan",
    "init-deep",
    "lsp-setup",
    "omo-relay",
    "onboarding",
    "programming",
    "refactor",
    "remove-ai-slops",
    "review-work",
    "ultimate-browsing",
    "ultrawork",
    "ulw-execute",
    "ulw-loop",
    "ulw-plan",
    "ulw-research",
    "visual-qa",
    "visualize",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..")
}

fn overlay_root() -> PathBuf {
    repo_root().join("assets/omo-latest")
}

fn skills_root() -> PathBuf {
    overlay_root().join("skills")
}

fn extract_frontmatter_field(content: &str, key: &str) -> Option<String> {
    let normalized = content.replace("\r\n", "\n");
    let rest = normalized.strip_prefix("---\n")?;
    let (frontmatter, body) = rest.split_once("\n---\n")?;
    if body.trim().is_empty() {
        return None;
    }
    let prefix = format!("{key}:");
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if let Some(raw_val) = trimmed.strip_prefix(&prefix) {
            let unquoted = raw_val.trim().trim_matches('"').trim_matches('\'').trim();
            if !unquoted.is_empty() {
                return Some(unquoted.to_owned());
            }
        }
    }
    None
}

#[test]
fn all_29_bundled_entries_and_28_skill_frontmatters_and_manifest_are_valid() {
    let root = skills_root();
    assert!(root.is_dir(), "skills root missing: {}", root.display());

    let mut entries: Vec<String> = std::fs::read_dir(&root)
        .expect("read skills dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();

    let mut expected_entries: Vec<String> =
        EXPECTED_SKILLS.iter().map(|s| (*s).to_owned()).collect();
    expected_entries.push("AGENTS.md".to_owned());
    expected_entries.sort();
    assert_eq!(entries.len(), 29);
    assert_eq!(entries, expected_entries);

    for skill_name in EXPECTED_SKILLS {
        let skill_md = root.join(skill_name).join("SKILL.md");
        assert!(
            skill_md.is_file(),
            "missing SKILL.md for {skill_name}: {}",
            skill_md.display()
        );
        let content = std::fs::read_to_string(&skill_md).expect("read SKILL.md");
        let parsed_name = extract_frontmatter_field(&content, "name")
            .unwrap_or_else(|| panic!("{skill_name}: missing frontmatter name"));
        let parsed_desc = extract_frontmatter_field(&content, "description")
            .unwrap_or_else(|| panic!("{skill_name}: missing frontmatter description"));
        assert_eq!(parsed_name, skill_name, "{skill_name} name mismatch");
        assert!(
            parsed_desc.len() >= 10,
            "{skill_name} description too short"
        );
    }

    let manifest_raw =
        std::fs::read_to_string(overlay_root().join("manifest.json")).expect("read manifest.json");
    let manifest: serde_json::Value =
        serde_json::from_str(&manifest_raw).expect("parse manifest.json");
    let selection: Vec<&str> = manifest["selection"]
        .as_array()
        .expect("selection array")
        .iter()
        .map(|v| v.as_str().expect("string"))
        .collect();
    assert_eq!(selection, EXPECTED_SKILLS);

    let retired: BTreeSet<&str> = manifest["retired"]
        .as_array()
        .expect("retired array")
        .iter()
        .map(|v| v.as_str().expect("string"))
        .collect();
    assert!(!retired.contains("hyperplan"));
    assert!(retired.contains("mass-ulw"));
}

#[tokio::test]
async fn eval_integrity_is_off_by_default_and_enabled_when_opted_in() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let root = skills_root();

    let default_env = support::env_for_home(&home);
    assert!(!is_opt_in_skill_enabled("eval-integrity", &default_env));
    assert!(is_opt_in_skill_enabled("hyperplan", &default_env));
    assert!(is_opt_in_skill_enabled("omo-relay", &default_env));

    let mut runner_default = ExtensionRunner::from_static(
        vec![Box::new(BundledSkillsComponent::new(
            BundledSkillsComponentOptions {
                env: Some(default_env.clone()),
                skills_dir: Some(root.clone()),
            },
        ))],
        support::context(&home, None),
    );
    let discovered_default = runner_default
        .emit_resources_discover(home.clone(), SessionReason::Startup)
        .await
        .expect("discover default");
    let default_paths: Vec<&str> = discovered_default
        .skill_paths
        .iter()
        .map(|e| e.path.as_str())
        .collect();
    assert_eq!(default_paths.len(), 27);
    assert!(
        !default_paths
            .iter()
            .any(|p| p.contains("eval-integrity/SKILL.md"))
    );
    assert!(
        default_paths
            .iter()
            .any(|p| p.contains("hyperplan/SKILL.md"))
    );
    assert!(
        default_paths
            .iter()
            .any(|p| p.contains("omo-relay/SKILL.md"))
    );

    let mut opt_in_env = default_env.clone();
    opt_in_env.insert(ENABLE_EVAL_INTEGRITY_ENV.to_owned(), "1".to_owned());
    assert!(is_opt_in_skill_enabled("eval-integrity", &opt_in_env));

    let mut runner_opt_in = ExtensionRunner::from_static(
        vec![Box::new(BundledSkillsComponent::new(
            BundledSkillsComponentOptions {
                env: Some(opt_in_env.clone()),
                skills_dir: Some(root.clone()),
            },
        ))],
        support::context(&home, None),
    );
    let discovered_opt_in = runner_opt_in
        .emit_resources_discover(home.clone(), SessionReason::Startup)
        .await
        .expect("discover opt-in");
    assert_eq!(discovered_opt_in.skill_paths.len(), 28);
    assert!(
        discovered_opt_in
            .skill_paths
            .iter()
            .any(|e| e.path.contains("eval-integrity/SKILL.md"))
    );

    assert_eq!(
        resolve_contributed_skill(ResolveContributedSkillOptions {
            commands: None,
            name: "eval-integrity",
            path: root.join("eval-integrity/SKILL.md"),
            cwd: home.clone(),
            env: &default_env,
        }),
        ContributedSkill::Disabled
    );
    let eval_path = root.join("eval-integrity/SKILL.md");
    assert_eq!(
        resolve_contributed_skill(ResolveContributedSkillOptions {
            commands: None,
            name: "eval-integrity",
            path: eval_path.clone(),
            cwd: home.clone(),
            env: &opt_in_env,
        }),
        ContributedSkill::Contributed { path: eval_path }
    );
}

#[test]
fn resolve_bundled_skill_provenance_distinguishes_shadowed_and_unshadowed_entries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let builtin = temp.path().join("builtin/onboarding/SKILL.md");
    let user_copy = temp.path().join("user/onboarding/SKILL.md");
    std::fs::create_dir_all(builtin.parent().expect("parent")).expect("mkdir builtin");
    std::fs::create_dir_all(user_copy.parent().expect("parent")).expect("mkdir user");
    std::fs::write(&builtin, "---\nname: onboarding\n---\n").expect("write builtin");
    std::fs::write(&user_copy, "---\nname: onboarding\n---\n").expect("write user");

    assert_eq!(
        resolve_bundled_skill_provenance(None, "onboarding", &builtin),
        BundledSkillProvenance { shadowed: false }
    );
    assert_eq!(
        resolve_bundled_skill_provenance(Some(&[]), "onboarding", &builtin),
        BundledSkillProvenance { shadowed: false }
    );

    let no_path_cmd = [HostCommandInfo {
        name: "skill:onboarding".to_owned(),
        description: None,
        source: "skill".to_owned(),
        source_info_path: None,
    }];
    assert_eq!(
        resolve_bundled_skill_provenance(Some(&no_path_cmd), "onboarding", &builtin),
        BundledSkillProvenance { shadowed: false }
    );

    let own_cmd = [HostCommandInfo {
        name: "skill:onboarding".to_owned(),
        description: None,
        source: "skill".to_owned(),
        source_info_path: Some(builtin.to_string_lossy().into_owned()),
    }];
    assert_eq!(
        resolve_bundled_skill_provenance(Some(&own_cmd), "onboarding", &builtin),
        BundledSkillProvenance { shadowed: false }
    );

    let user_cmd = [HostCommandInfo {
        name: "skill:onboarding".to_owned(),
        description: None,
        source: "skill".to_owned(),
        source_info_path: Some(user_copy.to_string_lossy().into_owned()),
    }];
    assert_eq!(
        resolve_bundled_skill_provenance(Some(&user_cmd), "onboarding", &builtin),
        BundledSkillProvenance { shadowed: true }
    );
}

fn find_bun() -> Option<PathBuf> {
    // An absolute path is required: the scaffolder test replaces PATH with an empty
    // directory, so a bare "bun" would no longer resolve at spawn time.
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("bun");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let candidate = Path::new(&home).join(".bun/bin/bun");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[test]
fn three_pass_reference_validator_and_scaffolder_crlf_and_junk_detector_contracts() {
    let Some(bun) = find_bun() else {
        return;
    };
    let repo = repo_root();

    let clean_audit = Command::new(&bun)
        .arg(repo.join("tools/check-skill-refs.mjs"))
        .arg("--repo-root")
        .arg(&repo)
        .current_dir(&repo)
        .output()
        .expect("run check-skill-refs on repo");
    assert!(
        clean_audit.status.success(),
        "check-skill-refs failed on shipped skills:\n{}{}",
        String::from_utf8_lossy(&clean_audit.stdout),
        String::from_utf8_lossy(&clean_audit.stderr)
    );

    let temp = tempfile::tempdir().expect("tempdir");
    let broken_skill = temp.path().join("broken-skill");
    std::fs::create_dir_all(&broken_skill).expect("mkdir broken");
    std::fs::write(
        broken_skill.join("SKILL.md"),
        "---\nname: broken-skill\ndescription: Broken fixture\n---\n\nRun `task(category=\"quik\", load_skills=[\"git_master\"])` and `task(subagent_type=\"explroe\")` and read `references/no-such-file.md`.\n",
    )
    .expect("write broken SKILL.md");

    let broken_audit = Command::new(&bun)
        .arg(repo.join("tools/check-skill-refs.mjs"))
        .arg("--repo-root")
        .arg(&repo)
        .arg(&broken_skill)
        .current_dir(&repo)
        .output()
        .expect("run check-skill-refs on broken fixture");
    assert!(!broken_audit.status.success());
    let broken_stdout = String::from_utf8_lossy(&broken_audit.stdout);
    assert!(
        broken_stdout
            .contains("[skill-name] unresolved \"git_master\" (did you mean \"git-master\"?)")
    );
    assert!(
        broken_stdout.contains("[agent-name] unresolved \"explroe\" (did you mean \"explore\"?)")
    );
    assert!(
        broken_stdout.contains("[category-name] unresolved \"quik\" (did you mean \"quick\"?)")
    );
    assert!(broken_stdout.contains("[skill-path] unresolved \"references/no-such-file.md\""));

    let crlf_go_dir = temp.path().join("go-scaffolder");
    let crlf_templates = crlf_go_dir.join("templates");
    std::fs::create_dir_all(&crlf_templates).expect("mkdir templates");
    let shared_dir = temp.path().join("shared");
    std::fs::create_dir_all(&shared_dir).expect("mkdir shared");
    std::fs::copy(
        repo.join("assets/omo-latest/skills/programming/scripts/shared/run-tool.ts"),
        shared_dir.join("run-tool.ts"),
    )
    .expect("copy run-tool.ts");
    std::fs::copy(
        repo.join("assets/omo-latest/skills/programming/scripts/go/new-project.ts"),
        crlf_go_dir.join("new-project.ts"),
    )
    .expect("copy go/new-project.ts");

    let src_templates = repo.join("assets/omo-latest/skills/programming/scripts/go/templates");
    for entry in std::fs::read_dir(&src_templates).expect("read go templates") {
        let entry = entry.expect("template entry");
        let raw = std::fs::read_to_string(entry.path()).expect("read template");
        let crlf = raw.replace("\r\n", "\n").replace('\n', "\r\n");
        std::fs::write(crlf_templates.join(entry.file_name()), crlf).expect("write crlf template");
    }

    let empty_bin = temp.path().join("empty-bin");
    std::fs::create_dir_all(&empty_bin).expect("empty bin");
    let out_parent = temp.path().join("out");
    std::fs::create_dir_all(&out_parent).expect("out parent");

    let scaffold_out = Command::new(&bun)
        .arg(crlf_go_dir.join("new-project.ts"))
        .arg("demosvc")
        .arg("--path")
        .arg(&out_parent)
        .env("PATH", &empty_bin)
        .output()
        .expect("run go new-project.ts");
    assert!(
        scaffold_out.status.success(),
        "go scaffolder failed: {}",
        String::from_utf8_lossy(&scaffold_out.stderr)
    );
    for rel in [
        "go.mod",
        ".golangci.yml",
        "cmd/server/main.go",
        "AGENTS.md",
        "README.md",
    ] {
        let bytes = std::fs::read(out_parent.join("demosvc").join(rel)).expect("read generated");
        assert!(
            !bytes.contains(&b'\r'),
            "generated file {rel} contained CR byte"
        );
    }

    let junk_fixture = temp.path().join("sample.test.ts");
    std::fs::write(
        &junk_fixture,
        "const doc = readFileSync(\"SKILL.md\", \"utf8\");\nexpect(doc).toContain(\"pinned prose\");\nexpect(x).toBe(x);\nawait Bun.sleep(50);\nawait Bun.sleep(50); // test-junk-ok: fixed-sleep debounce window\n",
    )
    .expect("write junk fixture");
    let junk_out = Command::new(&bun)
        .arg(
            repo.join("assets/omo-latest/skills/programming/scripts/typescript/check-test-junk.ts"),
        )
        .arg("--json")
        .arg(&junk_fixture)
        .output()
        .expect("run check-test-junk.ts");
    assert_eq!(junk_out.status.code(), Some(1));
    let parsed: serde_json::Value =
        serde_json::from_slice(&junk_out.stdout).expect("parse check-test-junk json");
    let rules: Vec<&str> = parsed["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .map(|f| f["rule"].as_str().expect("rule string"))
        .collect();
    assert_eq!(rules, vec!["prose-pin", "self-compare", "fixed-sleep"]);
}
