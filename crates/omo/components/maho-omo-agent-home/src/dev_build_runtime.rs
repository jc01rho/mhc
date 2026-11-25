use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::account_home::{PlatformFacts, user_record_home};

/// Maps an agent dir to the store path one of its stores lives at (`<agent dir>/gateway`, ...).
pub type StoreLocator = fn(&Path) -> PathBuf;

pub fn process_env() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

/// An omob dev build stamps `omoBuild` into the runtime payload's package.json and the launcher
/// points `OMO_PACKAGE_DIR` at that payload; release payloads carry no stamp.
pub fn is_dev_build_runtime(env: &BTreeMap<String, String>) -> bool {
    let Some(package_dir) = env
        .get("OMO_PACKAGE_DIR")
        .filter(|dir| !dir.trim().is_empty())
    else {
        return false;
    };
    let Ok(text) = fs::read_to_string(Path::new(package_dir).join("package.json")) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    manifest
        .get("omoBuild")
        .and_then(|stamp| stamp.get("command"))
        .and_then(Value::as_str)
        .is_some_and(|command| !command.is_empty())
}

/// Every home a released omo on this account may resolve its agent dir under: `HOME`, `USERPROFILE`
/// and the OS user record's home can differ, so each one counts (omo#14622 review M2). `None` when
/// the record cannot be read: then no agent dir can be proven isolated.
pub fn account_homes(facts: &dyn PlatformFacts) -> Option<Vec<PathBuf>> {
    let record = user_record_home(facts)?;
    let mut homes = Vec::new();
    for candidate in [facts.env("HOME"), facts.env("USERPROFILE"), Some(record)] {
        if let Some(home) = candidate
            && !home.is_empty()
        {
            homes.push(PathBuf::from(home));
        }
    }
    Some(homes)
}

fn shared_agent_dirs(homes: Option<&[PathBuf]>) -> Option<Vec<PathBuf>> {
    homes.map(|homes| {
        homes
            .iter()
            .flat_map(|home| {
                [
                    home.join(".omo").join("agent"),
                    home.join(".omo"),
                    home.join(".senpi").join("agent"),
                ]
            })
            .collect()
    })
}

/// True when `agent_dir` may be one of the agent homes every released omo on this account shares
/// (`~/.omo/agent`, the flat `~/.omo`, `~/.senpi/agent`) under any of the account's homes, or when
/// one of `stores` maps it to the same path one of them uses: a `gateway/` linked into the
/// release's (review M8), or only its `gateway/gateway.sqlite` linked to the release database
/// (review M9). With the account's homes unknown (`None`) it answers true: a store that cannot be
/// proven isolated is treated as shared.
pub fn is_shared_agent_dir(
    agent_dir: &Path,
    homes: Option<&[PathBuf]>,
    stores: &[StoreLocator],
) -> bool {
    let Some(candidates) = shared_agent_dirs(homes) else {
        return true;
    };
    candidates.iter().any(|candidate| {
        same_location(candidate, agent_dir)
            || stores
                .iter()
                .any(|store| same_location(&store(candidate.as_path()), &store(agent_dir)))
    })
}

/// True when `file`, a store file already opened (its path with links resolved), is the file
/// `store` names under one of the shared agent dirs, by filesystem identity (review M9): a shared
/// store kept outside the default layout is recognized by the file itself. Answers true when the
/// account's homes are unknown, as [`is_shared_agent_dir`] does.
pub fn is_shared_store_file(file: &Path, store: StoreLocator, homes: Option<&[PathBuf]>) -> bool {
    let Some(candidates) = shared_agent_dirs(homes) else {
        return true;
    };
    candidates
        .iter()
        .any(|candidate| same_location(&store(candidate.as_path()), file))
}

/// Two spellings name one store when the filesystem says so (review M3): an existing path is
/// compared by its device and inode, so a case-insensitive volume's `.OMO\agent` and `.omo\agent`
/// match. A path that does not exist yet falls back to its canonical spelling, case-folded where
/// the platform's default volumes ignore case.
fn same_location(left: &Path, right: &Path) -> bool {
    if let (Some(a), Some(b)) = (identity(left), identity(right)) {
        return a == b;
    }
    fold(&canonical(left, 0)) == fold(&canonical(right, 0))
}

fn fold(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    if cfg!(any(windows, target_os = "macos")) {
        text.to_lowercase()
    } else {
        text
    }
}

#[cfg(unix)]
fn identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn identity(_path: &Path) -> Option<(u64, u64)> {
    None
}

fn absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

/// The canonical spelling of a path, symlinks resolved even where their target does not exist yet.
fn canonical(path: &Path, depth: usize) -> PathBuf {
    if let Ok(real) = fs::canonicalize(path) {
        return real;
    }
    let target = absolute(path);
    if depth > 40 {
        return target;
    }
    if let Ok(metadata) = fs::symlink_metadata(&target)
        && metadata.file_type().is_symlink()
        && let Ok(link) = fs::read_link(&target)
    {
        let base = target.parent().map(Path::to_path_buf).unwrap_or_default();
        return canonical(&base.join(link), depth + 1);
    }
    match (target.parent(), target.file_name()) {
        (Some(parent), Some(name)) => canonical(parent, depth + 1).join(name),
        _ => target,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_home::fake::FakeFacts;

    fn gateway(agent_dir: &Path) -> PathBuf {
        agent_dir.join("gateway")
    }

    fn homes(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(|path| PathBuf::from(*path)).collect()
    }

    fn env_with(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn the_agent_dir_under_a_userprofile_that_differs_from_home_is_shared() {
        let both = homes(&["/shell-home", "/profile-home"]);
        assert!(is_shared_agent_dir(
            Path::new("/profile-home/.omo/agent"),
            Some(&both),
            &[]
        ));
    }

    #[test]
    fn each_shared_agent_dir_spelling_is_shared() {
        let account = homes(&["/account-home"]);
        for dir in [
            "/account-home/.omo/agent",
            "/account-home/.omo",
            "/account-home/.senpi/agent",
        ] {
            assert!(
                is_shared_agent_dir(Path::new(dir), Some(&account), &[]),
                "{dir}"
            );
        }
    }

    #[test]
    fn an_isolated_agent_dir_is_not_shared() {
        let both = homes(&["/shell-home", "/profile-home"]);
        assert!(!is_shared_agent_dir(
            Path::new("/tmp/isolated-agent"),
            Some(&both),
            &[]
        ));
    }

    #[test]
    fn unknown_account_homes_count_every_agent_dir_as_shared() {
        assert!(is_shared_agent_dir(
            Path::new("/tmp/isolated-agent"),
            None,
            &[]
        ));
        assert!(is_shared_store_file(
            Path::new("/tmp/isolated.sqlite"),
            gateway,
            None
        ));
    }

    #[test]
    fn homes_come_from_home_userprofile_and_the_user_record() {
        let mut facts = FakeFacts::linux_passwd(1000, "me:x:1000:1000::/home/me:/bin/sh\n");
        facts
            .env
            .insert("HOME".to_owned(), "/alternate-home".to_owned());
        facts
            .env
            .insert("USERPROFILE".to_owned(), "/profile-home".to_owned());
        assert_eq!(
            account_homes(&facts),
            Some(homes(&["/alternate-home", "/profile-home", "/home/me"]))
        );
        let found = account_homes(&facts).expect("homes");
        assert!(is_shared_agent_dir(
            Path::new("/home/me/.omo/agent"),
            Some(&found),
            &[]
        ));
        assert!(!is_shared_agent_dir(
            Path::new("/tmp/isolated-agent"),
            Some(&found),
            &[]
        ));
    }

    #[test]
    fn an_unreadable_or_unmatched_user_record_leaves_no_homes_so_the_store_is_shared() {
        let unmatched = FakeFacts::linux_passwd(1000, "root:x:0:0::/root:/bin/sh\n");
        let unreadable = FakeFacts::new(crate::account_home::Platform::Linux, Some(1000));
        for facts in [unmatched, unreadable] {
            let found = account_homes(&facts);
            assert_eq!(found, None);
            assert!(is_shared_agent_dir(
                Path::new("/tmp/isolated-agent"),
                found.as_deref(),
                &[]
            ));
        }
    }

    #[test]
    fn a_blank_home_variable_is_not_a_home() {
        let mut facts = FakeFacts::linux_passwd(1000, "me:x:1000:1000::/home/me:/bin/sh\n");
        facts.env.insert("HOME".to_owned(), String::new());
        assert_eq!(account_homes(&facts), Some(homes(&["/home/me"])));
    }

    #[test]
    fn a_dev_build_is_recognised_only_by_an_omo_build_stamp_naming_its_command() {
        let cases: [(&str, Option<&str>, bool); 8] = [
            ("no OMO_PACKAGE_DIR", None, false),
            ("a blank OMO_PACKAGE_DIR", Some("blank"), false),
            ("no package.json", Some("missing"), false),
            ("a package.json that is not JSON", Some("{"), false),
            ("a release manifest", Some(r#"{"version":"5.2.0"}"#), false),
            (
                "a stamp without a command",
                Some(r#"{"omoBuild":{}}"#),
                false,
            ),
            (
                "a stamp with an empty command",
                Some(r#"{"omoBuild":{"command":""}}"#),
                false,
            ),
            (
                "a stamp naming its command",
                Some(r#"{"omoBuild":{"command":"omob"}}"#),
                true,
            ),
        ];
        for (name, manifest, expected) in cases {
            let payload = tempfile::tempdir().expect("payload dir");
            let env = match manifest {
                None => BTreeMap::new(),
                Some("blank") => env_with(&[("OMO_PACKAGE_DIR", "  ")]),
                Some(text) => {
                    if text != "missing" {
                        fs::write(payload.path().join("package.json"), text).expect("manifest");
                    }
                    env_with(&[(
                        "OMO_PACKAGE_DIR",
                        payload.path().to_str().expect("utf-8 path"),
                    )])
                }
            };
            assert_eq!(is_dev_build_runtime(&env), expected, "{name}");
        }
    }

    #[cfg(unix)]
    mod links {
        use super::*;
        use std::os::unix::fs::symlink;

        fn temp_home() -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::tempdir().expect("temp home");
            let home = fs::canonicalize(dir.path()).expect("canonical home");
            (dir, home)
        }

        #[test]
        fn an_agent_dir_that_links_to_the_shared_store_is_shared() {
            let (_guard, home) = temp_home();
            fs::create_dir_all(home.join(".omo/agent")).expect("shared agent dir");
            symlink(home.join(".omo/agent"), home.join("linked-agent")).expect("link");
            let account = [home.clone()];
            assert!(is_shared_agent_dir(
                &home.join("linked-agent"),
                Some(&account),
                &[]
            ));
        }

        #[test]
        fn a_separate_agent_dir_whose_gateway_links_to_the_shared_one_is_shared_by_store() {
            let (_guard, home) = temp_home();
            fs::create_dir_all(home.join(".omo/agent/gateway")).expect("shared gateway");
            fs::create_dir_all(home.join("isolated")).expect("isolated dir");
            symlink(
                home.join(".omo/agent/gateway"),
                home.join("isolated/gateway"),
            )
            .expect("link");
            let account = [home.clone()];
            let stores: [StoreLocator; 1] = [gateway];
            assert!(!is_shared_agent_dir(
                &home.join("isolated"),
                Some(&account),
                &[]
            ));
            assert!(is_shared_agent_dir(
                &home.join("isolated"),
                Some(&account),
                &stores
            ));
        }

        #[test]
        fn a_gateway_link_to_a_shared_gateway_dir_that_does_not_exist_yet_is_shared() {
            let (_guard, home) = temp_home();
            fs::create_dir_all(home.join("isolated")).expect("isolated dir");
            symlink(
                home.join(".omo/agent/gateway"),
                home.join("isolated/gateway"),
            )
            .expect("dangling link");
            let account = [home.clone()];
            let stores: [StoreLocator; 1] = [gateway];
            assert!(is_shared_agent_dir(
                &home.join("isolated"),
                Some(&account),
                &stores
            ));
        }

        #[test]
        fn an_opened_file_is_compared_with_the_shared_store_by_identity() {
            let (_guard, home) = temp_home();
            fs::create_dir_all(home.join(".omo/agent")).expect("shared agent dir");
            fs::create_dir_all(home.join("data")).expect("data dir");
            let kept_elsewhere = home.join("data/thread.sqlite");
            fs::write(&kept_elsewhere, b"").expect("database");
            symlink(&kept_elsewhere, home.join(".omo/agent/gateway")).expect("link");
            let unrelated = home.join("data/unrelated.sqlite");
            fs::write(&unrelated, b"").expect("unrelated database");
            let account = [home.clone()];
            assert!(is_shared_store_file(
                &kept_elsewhere,
                gateway,
                Some(&account)
            ));
            assert!(!is_shared_store_file(&unrelated, gateway, Some(&account)));
        }
    }

    #[test]
    fn a_path_that_does_not_exist_yet_is_compared_by_its_normalized_spelling() {
        let account = homes(&["/account-home"]);
        assert!(is_shared_agent_dir(
            Path::new("/account-home/x/../.omo/./agent"),
            Some(&account),
            &[]
        ));
    }
}
