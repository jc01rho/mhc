//! Port of senpi `core/session-spawn-attestation.ts` (senpi#6972): which session spawned a process,
//! attested by the process that spawned it.
//!
//! Every session of a shared host runs its tool commands as children of ONE host process, so the kernel
//! ancestry of a caller proves the host, not the session, and the caller's `PI_SESSION_ID` is whatever
//! its last `exec` said. The spawner therefore records each child it starts for a session in
//! `<agent dir>/session-spawns/<pid>.json`: the child's pid and kernel start time, the session id, and
//! the spawner's own pid and start time. The records use senpi's version-1 JSON shape, so records written
//! by an omo or senpi engine and by this one resolve through the same reader.
//!
//! [`resolve_pid_session`] answers "which session owns pid P" from the kernel alone: it walks P's ppid
//! chain to the nearest ancestor with a record and believes that record only when the ancestor's start
//! time, its parent and that parent's start time all match. An environment is never read. Anything
//! outside a recorded child's tree (a daemon re-parented to init, a child an in-process runtime spawned
//! directly) is unattested.
//!
//! A record that still holds a live process is never replaced, and a release removes only the record it
//! wrote, so one session's record cannot be overwritten or deleted through another session's spawn or a
//! recycled pid. Same-uid processes can write the agent directory, as they can every other record under
//! it, so this proves against a forged environment and a recycled pid, not against deliberate tampering
//! with the store.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::kernel_process_identity::{KernelProcessIdentity, read_kernel_process_identity};

pub const SESSION_SPAWNS_DIR: &str = "session-spawns";
const RECORD_VERSION: u32 = 1;
/// Deeper than any tool's process tree; bounds a walk over a corrupted table.
const MAX_ANCESTRY_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSpawnRecord {
    pub version: u32,
    pub pid: u32,
    pub started_at_ms: u64,
    /// The boot `started_at_ms` is relative to; `None` where it is a wall-clock stamp (macOS).
    pub boot_id: Option<String>,
    pub session_id: String,
    pub host_pid: u32,
    pub host_started_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnattestedReason {
    ProcessNotFound,
    NoAttestedAncestor,
}

impl UnattestedReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProcessNotFound => "process_not_found",
            Self::NoAttestedAncestor => "no_attested_ancestor",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PidSession {
    Attested {
        pid: u32,
        session_id: String,
        /// The recorded child on `pid`'s ancestry (`pid` itself when it is that child).
        attested_pid: u32,
        /// The process that spawned `attested_pid` and wrote its record.
        host_pid: u32,
        /// `pid` up to `attested_pid`, child first.
        chain: Vec<u32>,
    },
    Unattested {
        pid: u32,
        reason: UnattestedReason,
    },
}

pub fn session_spawns_dir(agent_dir: &Path) -> PathBuf {
    agent_dir.join(SESSION_SPAWNS_DIR)
}

static STORE: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// The record directory spawners in this process write to, installed once by session creation from its
/// agent directory (mhc: the tool crates cannot resolve the agent directory themselves). `None` stops
/// recording.
pub fn set_session_spawns_store(dir: Option<PathBuf>) {
    if let Ok(mut store) = STORE.write() {
        *store = dir;
    }
}

pub fn session_spawns_store() -> Option<PathBuf> {
    STORE.read().ok().and_then(|store| store.clone())
}

/// [`attest_session_spawn`] into the installed store; `None` when no store is installed.
pub fn attest_session_spawn_in_store(
    session_id: &str,
    pid: u32,
) -> Option<SessionSpawnAttestation> {
    Some(attest_session_spawn(
        &session_spawns_store()?,
        session_id,
        pid,
    ))
}

/// The record one spawn wrote, if it could be written: release it once the child exited.
pub struct SessionSpawnAttestation {
    path: PathBuf,
    record: Option<SessionSpawnRecord>,
}

impl SessionSpawnAttestation {
    /// `false` when the child could not be identified (already gone) or its record could not be written.
    pub fn recorded(&self) -> bool {
        self.record.is_some()
    }

    /// Removes the record this attestation wrote, and only that one.
    pub fn release(self) {
        let Some(mine) = self.record else {
            return;
        };
        if read_record(&self.path).is_some_and(|current| current == mine) {
            let _removed = fs::remove_file(&self.path);
        }
    }
}

/// Records that `pid`, a child this process just spawned, runs for `session_id`, under `dir` (see
/// [`session_spawns_dir`]). Never fails: a child that cannot be identified or recorded is simply not
/// attested.
pub fn attest_session_spawn(dir: &Path, session_id: &str, pid: u32) -> SessionSpawnAttestation {
    attest_session_spawn_with(
        dir,
        session_id,
        pid,
        std::process::id(),
        &read_kernel_process_identity,
    )
}

/// [`attest_session_spawn`] for a spawner other than this process, with an explicit identity reader.
pub fn attest_session_spawn_with(
    dir: &Path,
    session_id: &str,
    pid: u32,
    host_pid: u32,
    read: &dyn Fn(u32) -> Option<KernelProcessIdentity>,
) -> SessionSpawnAttestation {
    let path = dir.join(format!("{pid}.json"));
    let record = write_record(dir, &path, session_id, pid, host_pid, read)
        .ok()
        .flatten();
    SessionSpawnAttestation { path, record }
}

fn write_record(
    dir: &Path,
    path: &Path,
    session_id: &str,
    pid: u32,
    host_pid: u32,
    read: &dyn Fn(u32) -> Option<KernelProcessIdentity>,
) -> io::Result<Option<SessionSpawnRecord>> {
    let (Some(child), Some(host)) = (read(pid), read(host_pid)) else {
        return Ok(None);
    };
    if child.ppid != host_pid {
        return Ok(None);
    }
    let record = SessionSpawnRecord {
        version: RECORD_VERSION,
        pid,
        started_at_ms: child.started_at_ms,
        boot_id: child.boot_id.clone(),
        session_id: session_id.to_owned(),
        host_pid,
        host_started_at_ms: host.started_at_ms,
    };
    create_private_dir(dir)?;
    let temp = dir.join(format!("{pid}.json.{host_pid}.tmp"));
    write_private_file(&temp, &serde_json::to_string(&record)?)?;
    let published = publish(&temp, path, &record, pid, &child, read);
    let _removed = fs::remove_file(&temp);
    published
}

/// `hard_link` publishes only where no record exists; an existing one is replaced only when it no longer
/// holds.
fn publish(
    temp: &Path,
    path: &Path,
    record: &SessionSpawnRecord,
    pid: u32,
    child: &KernelProcessIdentity,
    read: &dyn Fn(u32) -> Option<KernelProcessIdentity>,
) -> io::Result<Option<SessionSpawnRecord>> {
    match fs::hard_link(temp, path) {
        Ok(()) => Ok(Some(record.clone())),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if let Some(existing) = read_record(path)
                && record_holds(&existing, pid, child, read)
            {
                return Ok((existing == *record).then(|| record.clone()));
            }
            fs::rename(temp, path)?;
            Ok(Some(record.clone()))
        }
        Err(error) => Err(error),
    }
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

fn write_private_file(path: &Path, contents: &str) -> io::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(contents.as_bytes())
}

/// Which session owns `pid`, from the kernel's ancestry and the spawners' records under `dir`.
pub fn resolve_pid_session(pid: u32, dir: &Path) -> PidSession {
    resolve_pid_session_with(pid, dir, &read_kernel_process_identity)
}

/// [`resolve_pid_session`] over an explicit identity reader.
pub fn resolve_pid_session_with(
    pid: u32,
    dir: &Path,
    read: &dyn Fn(u32) -> Option<KernelProcessIdentity>,
) -> PidSession {
    let mut identity = match read(pid) {
        Some(identity) if !identity.zombie => identity,
        _ => {
            return PidSession::Unattested {
                pid,
                reason: UnattestedReason::ProcessNotFound,
            };
        }
    };
    let mut chain = Vec::new();
    let mut current = pid;
    while chain.len() < MAX_ANCESTRY_DEPTH {
        chain.push(current);
        if let Some(record) = read_record(&dir.join(format!("{current}.json")))
            && record_holds(&record, current, &identity, read)
        {
            return PidSession::Attested {
                pid,
                session_id: record.session_id,
                attested_pid: current,
                host_pid: record.host_pid,
                chain,
            };
        }
        if identity.ppid <= 1 {
            break;
        }
        current = identity.ppid;
        let Some(parent) = read(current) else {
            break;
        };
        identity = parent;
    }
    PidSession::Unattested {
        pid,
        reason: UnattestedReason::NoAttestedAncestor,
    }
}

fn record_holds(
    record: &SessionSpawnRecord,
    pid: u32,
    identity: &KernelProcessIdentity,
    read: &dyn Fn(u32) -> Option<KernelProcessIdentity>,
) -> bool {
    if record.pid != pid
        || record.started_at_ms != identity.started_at_ms
        || record.boot_id != identity.boot_id
        || record.host_pid != identity.ppid
    {
        return false;
    }
    read(record.host_pid)
        .is_some_and(|host| !host.zombie && host.started_at_ms == record.host_started_at_ms)
}

fn read_record(path: &Path) -> Option<SessionSpawnRecord> {
    let record: SessionSpawnRecord = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    (record.version == RECORD_VERSION && !record.session_id.is_empty()).then_some(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn process(ppid: u32, started_at_ms: u64) -> KernelProcessIdentity {
        KernelProcessIdentity {
            ppid,
            started_at_ms,
            boot_id: Some("boot".to_owned()),
            zombie: false,
        }
    }

    /// A host (100), its recorded child (200) and that child's own child (300).
    fn fixture() -> HashMap<u32, KernelProcessIdentity> {
        HashMap::from([
            (100, process(1, 1_000)),
            (200, process(100, 2_000)),
            (300, process(200, 3_000)),
        ])
    }

    fn attested(pid: u32, session: &str, attested_pid: u32, chain: Vec<u32>) -> PidSession {
        PidSession::Attested {
            pid,
            session_id: session.to_owned(),
            attested_pid,
            host_pid: 100,
            chain,
        }
    }

    fn unattested(pid: u32, reason: UnattestedReason) -> PidSession {
        PidSession::Unattested { pid, reason }
    }

    fn attest(
        dir: &Path,
        table: &HashMap<u32, KernelProcessIdentity>,
        session: &str,
        pid: u32,
    ) -> SessionSpawnAttestation {
        attest_session_spawn_with(dir, session, pid, 100, &|pid| table.get(&pid).cloned())
    }

    fn resolve(dir: &Path, table: &HashMap<u32, KernelProcessIdentity>, pid: u32) -> PidSession {
        resolve_pid_session_with(pid, dir, &|pid| table.get(&pid).cloned())
    }

    #[test]
    fn a_recorded_child_and_its_descendants_resolve_to_the_spawning_session() {
        let dir = tempfile::tempdir().expect("dir");
        let table = fixture();
        assert!(attest(dir.path(), &table, "session-1", 200).recorded());

        assert_eq!(
            resolve(dir.path(), &table, 300),
            attested(300, "session-1", 200, vec![300, 200])
        );
        assert_eq!(
            resolve(dir.path(), &table, 200),
            attested(200, "session-1", 200, vec![200])
        );
        assert_eq!(
            resolve(dir.path(), &table, 100),
            unattested(100, UnattestedReason::NoAttestedAncestor)
        );
        assert_eq!(
            resolve(dir.path(), &table, 999),
            unattested(999, UnattestedReason::ProcessNotFound)
        );
    }

    #[test]
    fn a_recycled_pid_is_not_attested() {
        let dir = tempfile::tempdir().expect("dir");
        let mut table = fixture();
        assert!(attest(dir.path(), &table, "session-1", 200).recorded());
        table.insert(200, process(100, 5_000));

        assert_eq!(
            resolve(dir.path(), &table, 200),
            unattested(200, UnattestedReason::NoAttestedAncestor)
        );
    }

    #[test]
    fn a_replaced_dead_or_zombie_host_invalidates_the_record() {
        let dir = tempfile::tempdir().expect("dir");
        let mut table = fixture();
        assert!(attest(dir.path(), &table, "session-1", 200).recorded());
        let expected = unattested(200, UnattestedReason::NoAttestedAncestor);

        table.insert(100, process(1, 9_999));
        assert_eq!(resolve(dir.path(), &table, 200), expected, "replaced host");

        table.remove(&100);
        assert_eq!(resolve(dir.path(), &table, 200), expected, "dead host");

        let mut zombie_host = process(1, 1_000);
        zombie_host.zombie = true;
        table.insert(100, zombie_host);
        assert_eq!(resolve(dir.path(), &table, 200), expected, "zombie host");
    }

    #[test]
    fn a_zombie_pid_is_not_found() {
        let dir = tempfile::tempdir().expect("dir");
        let mut table = fixture();
        table.get_mut(&300).expect("pid 300").zombie = true;

        assert_eq!(
            resolve(dir.path(), &table, 300),
            unattested(300, UnattestedReason::ProcessNotFound)
        );
    }

    #[test]
    fn only_a_direct_child_of_the_spawner_is_recorded() {
        let dir = tempfile::tempdir().expect("dir");
        let table = fixture();

        assert!(!attest(dir.path(), &table, "session-1", 300).recorded());
        assert!(!attest(dir.path(), &table, "session-1", 999).recorded());
        assert!(!dir.path().join("300.json").exists());
    }

    #[test]
    fn a_record_that_still_holds_a_live_process_is_never_replaced() {
        let dir = tempfile::tempdir().expect("dir");
        let table = fixture();
        assert!(attest(dir.path(), &table, "session-1", 200).recorded());

        assert!(!attest(dir.path(), &table, "session-2", 200).recorded());
        assert_eq!(
            resolve(dir.path(), &table, 200),
            attested(200, "session-1", 200, vec![200])
        );
        assert!(attest(dir.path(), &table, "session-1", 200).recorded());
    }

    #[test]
    fn a_record_that_no_longer_holds_is_replaced() {
        let dir = tempfile::tempdir().expect("dir");
        let mut table = fixture();
        assert!(attest(dir.path(), &table, "session-1", 200).recorded());
        table.insert(200, process(100, 7_000));

        assert!(attest(dir.path(), &table, "session-3", 200).recorded());
        assert_eq!(
            resolve(dir.path(), &table, 200),
            attested(200, "session-3", 200, vec![200])
        );
    }

    #[test]
    fn a_release_removes_only_the_record_it_wrote() {
        let dir = tempfile::tempdir().expect("dir");
        let table = fixture();
        let path = dir.path().join("200.json");

        attest(dir.path(), &table, "session-1", 200).release();
        assert!(!path.exists());

        let second = attest(dir.path(), &table, "session-1", 200);
        fs::write(
            &path,
            r#"{"version":1,"pid":200,"startedAtMs":2000,"bootId":"boot","sessionId":"session-2","hostPid":100,"hostStartedAtMs":1000}"#,
        )
        .expect("another session's record");
        second.release();

        assert_eq!(
            resolve(dir.path(), &table, 200),
            attested(200, "session-2", 200, vec![200])
        );
    }

    #[test]
    fn a_record_written_by_an_omo_or_senpi_engine_resolves_the_same_way() {
        let dir = tempfile::tempdir().expect("dir");
        let wall_clock = |ppid, started_at_ms| KernelProcessIdentity {
            ppid,
            started_at_ms,
            boot_id: None,
            zombie: false,
        };
        let table = HashMap::from([(100, wall_clock(1, 1_000)), (200, wall_clock(100, 2_000))]);
        fs::write(
            dir.path().join("200.json"),
            r#"{"version":1,"pid":200,"startedAtMs":2000,"bootId":null,"sessionId":"session-9","hostPid":100,"hostStartedAtMs":1000}"#,
        )
        .expect("record");

        assert_eq!(
            resolve(dir.path(), &table, 200),
            attested(200, "session-9", 200, vec![200])
        );
    }

    #[test]
    fn a_written_record_is_senpis_version_one_shape() {
        let dir = tempfile::tempdir().expect("dir");
        let table = fixture();
        assert!(attest(dir.path(), &table, "session-1", 200).recorded());

        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join("200.json")).expect("record"))
                .expect("json");
        assert_eq!(
            written,
            serde_json::json!({
                "version": 1,
                "pid": 200,
                "startedAtMs": 2000,
                "bootId": "boot",
                "sessionId": "session-1",
                "hostPid": 100,
                "hostStartedAtMs": 1000,
            })
        );
        assert!(fs::read_dir(dir.path()).expect("dir").all(|entry| {
            !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn a_malformed_record_attests_nothing() {
        let dir = tempfile::tempdir().expect("dir");
        let table = fixture();
        let path = dir.path().join("200.json");
        for body in [
            "not json",
            r#"{"version":2,"pid":200,"startedAtMs":2000,"bootId":"boot","sessionId":"session-1","hostPid":100,"hostStartedAtMs":1000}"#,
            r#"{"version":1,"pid":200,"startedAtMs":2000,"bootId":"boot","sessionId":"","hostPid":100,"hostStartedAtMs":1000}"#,
            r#"{"version":1,"pid":200,"startedAtMs":"2000","bootId":"boot","sessionId":"session-1","hostPid":100,"hostStartedAtMs":1000}"#,
        ] {
            fs::write(&path, body).expect("record");
            assert_eq!(
                resolve(dir.path(), &table, 200),
                unattested(200, UnattestedReason::NoAttestedAncestor),
                "{body}"
            );
        }
    }

    #[test]
    fn the_walk_stops_at_the_depth_bound() {
        let dir = tempfile::tempdir().expect("dir");
        let mut table = HashMap::from([(99, process(1, 1))]);
        for index in 0..70u32 {
            let parent = if index == 0 { 99 } else { 999 + index };
            table.insert(1000 + index, process(parent, 10 + u64::from(index)));
        }
        let first = attest_session_spawn_with(dir.path(), "deep", 1000, 99, &|pid| {
            table.get(&pid).cloned()
        });
        assert!(first.recorded());

        assert_eq!(
            resolve(dir.path(), &table, 1063),
            PidSession::Attested {
                pid: 1063,
                session_id: "deep".to_owned(),
                attested_pid: 1000,
                host_pid: 99,
                chain: (1000..=1063).rev().collect(),
            }
        );
        assert_eq!(
            resolve(dir.path(), &table, 1064),
            unattested(1064, UnattestedReason::NoAttestedAncestor)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_real_child_is_attested_to_its_session_and_released_when_done() {
        let dir = tempfile::tempdir().expect("dir");
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep starts");
        let pid = child.id();
        let attestation = attest_session_spawn(dir.path(), "real-session", pid);
        assert!(attestation.recorded());
        let resolved = resolve_pid_session(pid, dir.path());
        let _killed = child.kill();
        let _waited = child.wait();
        attestation.release();

        assert_eq!(
            resolved,
            PidSession::Attested {
                pid,
                session_id: "real-session".to_owned(),
                attested_pid: pid,
                host_pid: std::process::id(),
                chain: vec![pid],
            }
        );
        assert!(!dir.path().join(format!("{pid}.json")).exists());
    }
}
