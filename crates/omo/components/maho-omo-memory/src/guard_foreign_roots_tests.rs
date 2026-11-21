use super::*;
use std::collections::BTreeMap;

const AGENTS: &str = "/m/memory/agents";
const TRANSIENT: &str = "/m/memory/transient-runs";

fn dir(name: &str) -> AreaEntry {
    AreaEntry {
        name: name.into(),
        is_file: false,
    }
}

fn file(name: &str) -> AreaEntry {
    AreaEntry {
        name: name.into(),
        is_file: true,
    }
}

fn listing(
    entries: BTreeMap<&'static str, io::Result<Vec<AreaEntry>>>,
) -> impl Fn(&Path) -> io::Result<Vec<AreaEntry>> {
    move |area: &Path| match entries.get(area.to_string_lossy().as_ref()) {
        None => Err(io::Error::from(io::ErrorKind::NotFound)),
        Some(Ok(listed)) => Ok(listed.clone()),
        Some(Err(error)) => Err(io::Error::from(error.kind())),
    }
}

fn paths(values: &[&str]) -> Vec<PathBuf> {
    values.iter().map(PathBuf::from).collect()
}

#[test]
fn a_single_identity_is_clear() {
    let read = listing(BTreeMap::from([(AGENTS, Ok(vec![dir("global")]))]));

    let scan = scan_foreign_roots(
        &paths(&[AGENTS, TRANSIENT]),
        &paths(&["/m/memory/agents/global"]),
        &read,
    );

    assert_eq!(scan, ForeignRootScan::Clear);
}

#[test]
fn a_second_identity_reports_the_area_without_naming_it() {
    let read = listing(BTreeMap::from([(
        AGENTS,
        Ok(vec![dir("global"), dir("other")]),
    )]));

    let scan = scan_foreign_roots(
        &paths(&[AGENTS, TRANSIENT]),
        &paths(&["/m/memory/agents/global"]),
        &read,
    );

    assert_eq!(
        scan,
        ForeignRootScan::Foreign {
            area: PathBuf::from(AGENTS)
        }
    );
}

#[test]
fn regular_files_are_not_identities() {
    let read = listing(BTreeMap::from([
        (AGENTS, Ok(vec![dir("global"), file("desktop.ini")])),
        (TRANSIENT, Ok(vec![file("Thumbs.db")])),
    ]));

    let scan = scan_foreign_roots(
        &paths(&[AGENTS, TRANSIENT]),
        &paths(&["/m/memory/agents/global"]),
        &read,
    );

    assert_eq!(scan, ForeignRootScan::Clear);
}

#[test]
fn an_entry_differing_only_by_case_is_foreign() {
    let read = listing(BTreeMap::from([("/m/agents", Ok(vec![dir("Global")]))]));

    let scan = scan_foreign_roots(&paths(&["/m/agents"]), &paths(&["/m/agents/global"]), &read);

    assert_eq!(
        scan,
        ForeignRootScan::Foreign {
            area: PathBuf::from("/m/agents")
        }
    );
}

#[test]
fn a_second_identity_under_the_runs_own_token_is_foreign() {
    let token_agents = "/m/memory/transient-runs/tok-1/agents";
    let read = listing(BTreeMap::from([
        (TRANSIENT, Ok(vec![dir("tok-1")])),
        (token_agents, Ok(vec![dir("global"), dir("other")])),
    ]));

    let scan = scan_foreign_roots(
        &paths(&[token_agents, AGENTS, TRANSIENT]),
        &paths(&["/m/memory/transient-runs/tok-1/agents/global"]),
        &read,
    );

    assert_eq!(
        scan,
        ForeignRootScan::Foreign {
            area: PathBuf::from(token_agents)
        }
    );
}

#[test]
fn only_the_runs_own_token_counts_as_own() {
    let token_agents = "/m/memory/transient-runs/tok-1/agents";
    let read = listing(BTreeMap::from([
        (TRANSIENT, Ok(vec![dir("tok-1")])),
        (token_agents, Ok(vec![dir("global")])),
    ]));

    let scan = scan_foreign_roots(
        &paths(&[token_agents, AGENTS, TRANSIENT]),
        &paths(&["/m/memory/transient-runs/tok-1/agents/global"]),
        &read,
    );

    assert_eq!(scan, ForeignRootScan::Clear);
}

#[test]
fn a_listing_error_other_than_missing_fails_closed() {
    for kind in [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::NotADirectory,
        io::ErrorKind::Other,
    ] {
        let read = listing(BTreeMap::from([
            (AGENTS, Ok(vec![dir("global")])),
            (TRANSIENT, Err(io::Error::from(kind))),
        ]));
        assert_eq!(
            scan_foreign_roots(
                &paths(&[AGENTS, TRANSIENT]),
                &paths(&["/m/memory/agents/global"]),
                &read
            ),
            ForeignRootScan::Unlisted {
                area: PathBuf::from(TRANSIENT)
            }
        );
    }
}

#[test]
fn the_real_listing_types_entries_without_following_links() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("global")).unwrap();
    std::fs::write(root.path().join(".DS_Store"), "").unwrap();

    let mut entries = read_area_entries(root.path()).unwrap();
    entries.sort_by(|left, right| left.name.cmp(&right.name));

    assert_eq!(entries, vec![file(".DS_Store"), dir("global")]);
}
