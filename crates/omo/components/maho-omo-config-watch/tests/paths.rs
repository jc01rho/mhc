use maho_omo_config_watch::paths::*;
use std::collections::BTreeMap;
#[test]
fn nested_targets_and_user_creation_are_root_anchored() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let work = home.join("work");
    let project = work.join("project");
    let cwd = project.join("child");
    std::fs::create_dir_all(&cwd)?;
    for dir in [&work, &project, &cwd] {
        std::fs::create_dir(dir.join(".omo"))?;
        std::fs::write(dir.join(".omo/omo.jsonc"), "{}")?;
    }
    let env = BTreeMap::from([(
        "SENPI_CODING_AGENT_DIR".into(),
        root.path().join("agent").to_string_lossy().into_owned(),
    )]);
    let r = resolve_omo_config_watch_target_resolution(&cwd, &home, &env);
    assert!(r.user_config_creation_watched);
    assert_eq!(r.user_config_creation_discovery, "watched");
    for dir in [&work, &project, &cwd] {
        let target = r
            .targets
            .iter()
            .find(|t| t.path == dir.join(".omo"))
            .expect("config target");
        assert_eq!(target.filter_globs, OMO_CONFIG_FILE_FILTER_GLOBS);
    }
    let user = r
        .targets
        .iter()
        .find(|t| t.path == home && t.filter_globs == USER_OMO_CONFIG_DIRECTORY_FILTER_GLOBS)
        .expect("user creation");
    assert!(user.filter_globs.iter().all(|g| g.starts_with('/')));
    Ok(())
}
#[test]
fn root_target_filters_exclude_protected_agent_subtree() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let cwd = home.join("work/project");
    std::fs::create_dir_all(&cwd)?;
    let env = BTreeMap::from([(
        "SENPI_CODING_AGENT_DIR".into(),
        home.join("custom-agent").to_string_lossy().into_owned(),
    )]);
    let r = resolve_omo_config_watch_target_resolution(&cwd, &home, &env);
    let target = r
        .targets
        .iter()
        .find(|t| t.path == home)
        .expect("root-anchored creation target");
    assert_eq!(target.filter_globs, OMO_CONFIG_DIRECTORY_FILTER_GLOBS);
    assert!(target.filter_globs.iter().all(|glob| glob.starts_with('/')));
    Ok(())
}
#[cfg(unix)]
#[test]
fn symlink_project_directory_not_watched() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let cwd = home.join("project");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&cwd)?;
    std::fs::create_dir(&outside)?;
    std::fs::write(outside.join("omo.jsonc"), "{}")?;
    std::os::unix::fs::symlink(&outside, cwd.join(".omo"))?;
    let r = resolve_omo_config_watch_target_resolution(&cwd, &home, &BTreeMap::new());
    assert!(!r.targets.iter().any(|t| t.path == cwd.join(".omo")));
    assert!(r.targets.iter().any(|t| t.path == cwd));
    Ok(())
}
#[test]
fn creation_target_contains_repair_filters() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let cwd = home.join("project");
    std::fs::create_dir_all(&cwd)?;
    let r = resolve_omo_config_watch_target_resolution(&cwd, &home, &BTreeMap::new());
    let t = r
        .targets
        .iter()
        .find(|t| t.path == cwd)
        .expect("creation target");
    assert_eq!(t.kind, "dir");
    assert_eq!(t.filter_globs, OMO_CONFIG_DIRECTORY_FILTER_GLOBS);
    assert_eq!(r.user_config_creation_discovery, "watched");
    Ok(())
}
#[test]
fn existing_project_directory_watched() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let cwd = home.join("project");
    std::fs::create_dir_all(cwd.join(".omo"))?;
    let r = resolve_omo_config_watch_target_resolution(&cwd, &home, &BTreeMap::new());
    assert!(r.targets.iter().any(|t| t.path == cwd.join(".omo")));
    assert!(r.targets.iter().any(|t| t.path == cwd));
    assert!(
        r.targets
            .iter()
            .any(|t| t.path == home && t.filter_globs == USER_OMO_CONFIG_DIRECTORY_FILTER_GLOBS)
    );
    Ok(())
}
#[test]
fn missing_user_config_creation_is_watched() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let cwd = home.join("project");
    std::fs::create_dir_all(&cwd)?;
    assert!(
        resolve_omo_config_watch_target_resolution(&cwd, &home, &BTreeMap::new())
            .user_config_creation_watched
    );
    Ok(())
}
#[test]
fn existing_user_config_watched() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let cwd = home.join("project");
    std::fs::create_dir_all(&cwd)?;
    std::fs::create_dir(home.join(".omo"))?;
    let mut env = BTreeMap::new();
    env.insert(
        "SENPI_CODING_AGENT_DIR".into(),
        root.path().join("agent").to_string_lossy().into_owned(),
    );
    let r = resolve_omo_config_watch_target_resolution(&cwd, &home, &env);
    assert!(r.user_config_creation_watched);
    assert!(r.targets.iter().any(|t| t.path == home.join(".omo")));
    Ok(())
}

#[test]
fn default_agent_protected_paths_are_never_covered() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let cwd = home.join("work/project/child");
    std::fs::create_dir_all(&cwd)?;
    let resolution = resolve_omo_config_watch_target_resolution(&cwd, &home, &BTreeMap::new());
    let agent = maho_omo_agent_home::resolve_agent_home::resolve_agent_home(
        &BTreeMap::new(),
        &home,
        &cwd,
        std::path::Path::exists,
    );
    for target in &resolution.targets {
        for protected in [
            agent.join("auth.json"),
            agent.join("sessions"),
            agent.join("logs"),
        ] {
            for glob in &target.filter_globs {
                let glob_path = target.path.join(glob.trim_start_matches('/'));
                assert!(!glob_path.starts_with(&protected));
                assert!(!protected.starts_with(&glob_path));
            }
        }
    }
    assert_eq!(
        resolution
            .targets
            .iter()
            .filter(|t| t.filter_globs == OMO_CONFIG_DIRECTORY_FILTER_GLOBS)
            .map(|t| t.path.clone())
            .collect::<Vec<_>>(),
        vec![
            home.clone(),
            cwd,
            home.join("work/project"),
            home.join("work"),
        ]
    );
    Ok(())
}

#[test]
fn runtime_subtrees_do_not_expand_config_filters() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let cwd = home.join("project");
    let config = cwd.join(".omo");
    std::fs::create_dir_all(config.join("senpi-task/children/child"))?;
    std::fs::write(config.join("omo.jsonc"), "{}")?;
    std::fs::write(
        config.join("senpi-task/children/child/transcript.jsonl"),
        "{}\n",
    )?;
    let resolution = resolve_omo_config_watch_target_resolution(&cwd, &home, &BTreeMap::new());
    let target = resolution
        .targets
        .iter()
        .find(|target| target.path == config)
        .expect("config target");
    assert_eq!(target.filter_globs, ["/omo.jsonc", "/omo.json"]);
    Ok(())
}

#[test]
fn every_ancestor_creation_target_is_root_anchored() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let cwd = home.join("work/project/child");
    std::fs::create_dir_all(&cwd)?;
    let env = BTreeMap::from([(
        "SENPI_CODING_AGENT_DIR".into(),
        root.path().join("agent").to_string_lossy().into_owned(),
    )]);
    let resolution = resolve_omo_config_watch_target_resolution(&cwd, &home, &env);
    let creation: Vec<_> = resolution
        .targets
        .iter()
        .filter(|t| t.filter_globs.iter().any(|g| g == "/.omo/omo.jsonc"))
        .collect();
    // Three ancestors below HOME plus HOME's user-config creation target.
    assert_eq!(creation.len(), 4);
    for target in creation {
        assert_eq!(
            target.filter_globs,
            ["/.omo", "/.omo/omo.jsonc", "/.omo/omo.json"]
        );
    }
    Ok(())
}

#[test]
fn user_creation_target_is_basename_anchored() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let cwd = home.join("project");
    std::fs::create_dir_all(&cwd)?;
    let env = BTreeMap::from([(
        "SENPI_CODING_AGENT_DIR".into(),
        root.path().join("agent").to_string_lossy().into_owned(),
    )]);
    let resolution = resolve_omo_config_watch_target_resolution(&cwd, &home, &env);
    let target = resolution
        .targets
        .iter()
        .find(|t| t.path == home && t.filter_globs.iter().any(|g| g == "/.omo"))
        .expect("user target");
    assert_eq!(
        target.filter_globs,
        ["/.omo", "/.omo/omo.jsonc", "/.omo/omo.json"]
    );
    Ok(())
}

type Plan9Fixture = (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    BTreeMap<String, String>,
);

fn plan9_fixture() -> Result<Plan9Fixture, Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let project = home.join("project");
    let cwd = project.join("src");
    std::fs::create_dir_all(&cwd)?;
    std::fs::create_dir_all(project.join(".omo"))?;
    std::fs::write(project.join(".omo/omo.jsonc"), "{}")?;
    let env = BTreeMap::from([(
        "SENPI_CODING_AGENT_DIR".into(),
        root.path().join("agent").to_string_lossy().into_owned(),
    )]);
    Ok((root, home, project, cwd, env))
}

#[test]
fn plan9_project_filesystem_skips_project_and_ancestor_watches_but_keeps_native_user_config()
-> Result<(), Box<dyn std::error::Error>> {
    let (_root, home, project, cwd, env) = plan9_fixture()?;
    let user = home.join(".omo");
    std::fs::create_dir_all(&user)?;
    let resolution = resolve_omo_config_watch_target_resolution_with(&cwd, &home, &env, &|path| {
        Some(if path == user {
            0x0102_1994
        } else {
            PLAN9_FILE_SYSTEM_TYPE
        })
    });
    assert!(
        resolution
            .targets
            .iter()
            .any(|t| t.path == user && t.filter_globs.iter().any(|g| g == "/omo.jsonc"))
    );
    assert!(
        !resolution
            .targets
            .iter()
            .any(|t| t.path == project.join(".omo"))
    );
    assert!(
        !resolution
            .targets
            .iter()
            .any(|t| t.filter_globs.iter().any(|g| g == "/.omo"))
    );
    Ok(())
}

#[test]
fn native_project_filesystem_preserves_project_and_ancestor_watches()
-> Result<(), Box<dyn std::error::Error>> {
    let (_root, home, project, cwd, env) = plan9_fixture()?;
    let resolution =
        resolve_omo_config_watch_target_resolution_with(&cwd, &home, &env, &|_| Some(0x0102_1994));
    assert!(resolution.targets.iter().any(
        |t| t.path == project.join(".omo") && t.filter_globs.iter().any(|g| g == "/omo.jsonc")
    ));
    assert!(
        resolution
            .targets
            .iter()
            .any(|t| t.filter_globs.iter().any(|g| g == "/.omo"))
    );
    Ok(())
}

#[test]
fn plan9_user_config_filesystem_skips_the_user_config_watch()
-> Result<(), Box<dyn std::error::Error>> {
    let (_root, home, _project, cwd, env) = plan9_fixture()?;
    let user = home.join(".omo");
    std::fs::create_dir_all(&user)?;
    let resolution = resolve_omo_config_watch_target_resolution_with(&cwd, &home, &env, &|_| {
        Some(PLAN9_FILE_SYSTEM_TYPE)
    });
    assert!(!resolution.targets.iter().any(|t| t.path == user));
    assert!(
        !resolution
            .targets
            .iter()
            .any(|t| t.filter_globs.iter().any(|g| g == "/.omo"))
    );
    Ok(())
}

#[test]
fn protected_paths_reject_unanchored_and_covering_globs_but_permit_safe_anchored_globs() {
    let agent = std::path::PathBuf::from("/home/u/.senpi/agent");
    let protected = [
        agent.join("auth.json"),
        agent.join("sessions"),
        agent.join("logs"),
    ];
    let target = |path: &str, globs: &[&str]| OmoConfigWatchTarget {
        path: path.into(),
        kind: "dir",
        filter_globs: globs.iter().map(|g| (*g).to_owned()).collect(),
    };
    assert!(is_senpi_restricted_target(
        &target("/home/u", &["*.jsonc"]),
        &protected
    ));
    assert!(is_senpi_restricted_target(
        &target("/home/u", &["/.senpi"]),
        &protected
    ));
    assert!(is_senpi_restricted_target(
        &target("/home/u/.senpi/agent/sessions/x", &["/omo.jsonc"]),
        &protected
    ));
    assert!(!is_senpi_restricted_target(
        &target("/home/u", &["/.omo", "/.omo/omo.jsonc", "/.omo/omo.json"]),
        &protected
    ));
}

struct BoundFixture {
    _root: tempfile::TempDir,
    home: std::path::PathBuf,
    work: std::path::PathBuf,
    project: std::path::PathBuf,
    cwd: std::path::PathBuf,
    env: BTreeMap<String, String>,
}

fn bound_fixture() -> Result<BoundFixture, Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let work = home.join("work");
    let project = work.join("project");
    let cwd = project.join("child");
    std::fs::create_dir_all(&cwd)?;
    std::fs::create_dir_all(home.join(".omo"))?;
    let env = BTreeMap::from([(
        "SENPI_CODING_AGENT_DIR".into(),
        root.path()
            .join("senpi-agent")
            .to_string_lossy()
            .into_owned(),
    )]);
    Ok(BoundFixture {
        _root: root,
        home,
        work,
        project,
        cwd,
        env,
    })
}

fn creation_paths(resolution: &OmoConfigWatchTargetResolution) -> Vec<std::path::PathBuf> {
    resolution
        .targets
        .iter()
        .filter(|t| t.filter_globs.iter().any(|g| g == "/.omo"))
        .map(|t| t.path.clone())
        .collect()
}

#[test]
fn creation_watches_stop_below_the_user_config_parent() -> Result<(), Box<dyn std::error::Error>> {
    let f = bound_fixture()?;
    let r = resolve_omo_config_watch_target_resolution(&f.cwd, &f.home, &f.env);
    assert_eq!(creation_paths(&r), vec![f.cwd, f.project, f.work]);
    Ok(())
}

#[test]
fn creation_watches_stop_at_a_project_root_holding_git_while_higher_omo_stays_watched()
-> Result<(), Box<dyn std::error::Error>> {
    let f = bound_fixture()?;
    std::fs::create_dir(f.project.join(".git"))?;
    std::fs::create_dir(f.work.join(".omo"))?;
    std::fs::write(f.work.join(".omo/omo.jsonc"), "{}")?;
    let r = resolve_omo_config_watch_target_resolution(&f.cwd, &f.home, &f.env);
    assert_eq!(creation_paths(&r), vec![f.cwd, f.project]);
    assert!(
        r.targets
            .iter()
            .any(|t| t.path == f.work.join(".omo")
                && t.filter_globs.iter().any(|g| g == "/omo.jsonc"))
    );
    Ok(())
}

#[test]
fn creation_watches_stop_at_a_git_file() -> Result<(), Box<dyn std::error::Error>> {
    let f = bound_fixture()?;
    std::fs::write(
        f.project.join(".git"),
        "gitdir: /somewhere/else/.git/worktrees/project\n",
    )?;
    let r = resolve_omo_config_watch_target_resolution(&f.cwd, &f.home, &f.env);
    assert_eq!(creation_paths(&r), vec![f.cwd, f.project]);
    Ok(())
}

#[test]
fn creation_watches_stop_at_the_nearest_repo_root() -> Result<(), Box<dyn std::error::Error>> {
    let f = bound_fixture()?;
    std::fs::create_dir(f.work.join(".git"))?;
    std::fs::create_dir(f.cwd.join(".git"))?;
    let r = resolve_omo_config_watch_target_resolution(&f.cwd, &f.home, &f.env);
    assert_eq!(creation_paths(&r), vec![f.cwd]);
    Ok(())
}

#[test]
fn cwd_at_home_adds_no_ancestor_creation_target() -> Result<(), Box<dyn std::error::Error>> {
    let f = bound_fixture()?;
    let r = resolve_omo_config_watch_target_resolution(&f.home, &f.home, &f.env);
    assert!(creation_paths(&r).is_empty());
    assert!(
        r.targets
            .iter()
            .any(|t| t.path == f.home.join(".omo")
                && t.filter_globs.iter().any(|g| g == "/omo.jsonc"))
    );
    Ok(())
}

#[test]
fn cwd_outside_home_and_any_repo_never_watches_the_filesystem_root()
-> Result<(), Box<dyn std::error::Error>> {
    let f = bound_fixture()?;
    let outside = f.home.parent().expect("home parent").join("elsewhere/a/b");
    std::fs::create_dir_all(&outside)?;
    let paths = creation_paths(&resolve_omo_config_watch_target_resolution(
        &outside, &f.home, &f.env,
    ));
    assert_eq!(paths.first(), Some(&outside));
    assert!(paths.iter().all(|path| path.parent().is_some()));
    Ok(())
}

#[test]
fn cwd_outside_home_never_watches_a_directory_containing_home()
-> Result<(), Box<dyn std::error::Error>> {
    let f = bound_fixture()?;
    let home_parent = f.home.parent().expect("home parent").to_owned();
    let shared = home_parent.join("shared/runs/x");
    std::fs::create_dir_all(&shared)?;
    let paths = creation_paths(&resolve_omo_config_watch_target_resolution(
        &shared, &f.home, &f.env,
    ));
    assert_eq!(
        paths,
        vec![
            shared.clone(),
            home_parent.join("shared/runs"),
            home_parent.join("shared"),
        ]
    );
    assert!(!paths.contains(&home_parent));
    Ok(())
}
