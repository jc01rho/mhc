#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use maho_machine_ops::{
    MountEntry, MountView, create_automount_trigger_lookup, parse_darwin_mount_table,
    parse_linux_mount_info, unmounted_automount_path,
};

fn view_of(mounts: Vec<MountEntry>, case_insensitive: bool) -> MountView {
    MountView::new(mounts, case_insensitive)
}

fn sample_view(case_insensitive: bool) -> MountView {
    MountView {
        mounts: vec![
            MountEntry::new("/", "apfs"),
            MountEntry::new("/System/Volumes/Data/home", "autofs"),
            MountEntry::new("/System/Volumes/Data/home/alice", "nfs"),
        ],
        aliases: BTreeMap::from([("/home".into(), "/System/Volumes/Data/home".into())]),
        case_insensitive,
    }
}

#[test]
fn reads_mount_points_and_types_from_proc_self_mountinfo_unescaping_octal() {
    let text = "22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
        36 25 0:31 / /proc/sys/fs/binfmt_misc rw,relatime shared:13 - autofs systemd-1 rw,fd=32,direct\n\
        90 22 0:50 / /mnt/my\\040share rw,relatime - nfs4 server:/export rw";
    let expected = vec![
        MountEntry::new("/", "ext4"),
        MountEntry::new("/proc/sys/fs/binfmt_misc", "autofs"),
        MountEntry::new("/mnt/my share", "nfs4"),
    ];
    assert_eq!(parse_linux_mount_info(text), expected);
}

#[test]
fn reads_mount_points_and_types_from_macos_mount_output() {
    let text = "/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)\n\
        map auto_home on /System/Volumes/Data/home (autofs, automounted, nobrowse)\n\
        //user@nas/My Share on /Users/me/NAS/My Share (smbfs, nodev, nosuid, mounted by me)";
    let expected = vec![
        MountEntry::new("/", "apfs"),
        MountEntry::new("/System/Volumes/Data/home", "autofs"),
        MountEntry::new("/Users/me/NAS/My Share", "smbfs"),
    ];
    assert_eq!(parse_darwin_mount_table(text), expected);
}

#[test]
fn maps_a_path_through_the_root_alias_onto_the_trigger_it_reaches() {
    let view = sample_view(false);
    let sub = unmounted_automount_path("/home/someone/work/x", &view);
    assert_eq!(
        sub.as_deref(),
        Some("/System/Volumes/Data/home/someone/work/x")
    );
    let root = unmounted_automount_path("/home", &view);
    assert_eq!(root.as_deref(), Some("/System/Volumes/Data/home"));
}

#[test]
fn leaves_a_filesystem_already_mounted_below_the_trigger_and_every_other_path_to_normal_resolution()
{
    let view = sample_view(false);
    assert_eq!(
        unmounted_automount_path("/home/alice/notes.md", &view),
        None
    );
    assert_eq!(unmounted_automount_path("/homework/x", &view), None);
    assert_eq!(unmounted_automount_path("/Users/me/x", &view), None);
}

#[test]
fn treats_a_filesystem_stacked_on_the_triggers_own_mount_point_as_mounted_whichever_entry_the_table_lists_first()
 {
    let autofs = "22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
        40 22 0:40 / /mnt/nas rw,relatime shared:20 - autofs systemd-1 rw,fd=45,direct\n\
        41 22 0:41 / /boot rw,relatime shared:21 - autofs systemd-1 rw,fd=46,direct";
    let mounted = "90 40 0:50 / /mnt/nas rw,relatime shared:30 - nfs4 server:/export rw\n\
        91 41 259:1 / /boot rw,relatime shared:31 - vfat /dev/nvme0n1p1 rw";
    let linux = |text: &str| view_of(parse_linux_mount_info(text), false);
    let both = format!("{autofs}\n{mounted}");
    let rev = format!("{mounted}\n{autofs}");

    assert_eq!(
        unmounted_automount_path("/mnt/nas/link/secret", &linux(autofs)).as_deref(),
        Some("/mnt/nas/link/secret")
    );
    assert_eq!(
        unmounted_automount_path("/mnt/nas/link/secret", &linux(&both)),
        None
    );
    assert_eq!(unmounted_automount_path("/boot/efi/x", &linux(&both)), None);
    assert_eq!(
        unmounted_automount_path("/mnt/nas/link/secret", &linux(&rev)),
        None
    );
}

#[test]
fn treats_a_macos_direct_map_share_mounted_over_its_autofs_entry_as_mounted() {
    let trigger = "map auto_nas on /Volumes/nas (autofs, automounted, nobrowse)";
    let share = "server:/export on /Volumes/nas (nfs, nodev, nosuid, automounted, nobrowse)";
    let darwin = |text: &str| view_of(parse_darwin_mount_table(text), true);

    assert_eq!(
        unmounted_automount_path("/Volumes/nas/link/secret", &darwin(trigger)).as_deref(),
        Some("/Volumes/nas/link/secret")
    );
    let stacked = format!("{trigger}\n{share}");
    assert_eq!(
        unmounted_automount_path("/Volumes/nas/link/secret", &darwin(&stacked)),
        None
    );
}

#[test]
fn matches_mount_points_and_root_aliases_regardless_of_case_on_a_case_insensitive_volume() {
    let view = sample_view(false);
    let folded = sample_view(true);

    assert_eq!(
        unmounted_automount_path("/HOME/someone/x", &folded).as_deref(),
        Some("/System/Volumes/Data/home/someone/x")
    );
    assert_eq!(
        unmounted_automount_path("/System/Volumes/Data/HOME/someone/x", &folded).as_deref(),
        Some("/System/Volumes/Data/home/someone/x")
    );
    assert_eq!(
        unmounted_automount_path("/home/ALICE/notes.md", &folded),
        None
    );
    assert_eq!(unmounted_automount_path("/HOME/someone/x", &view), None);
}

#[tokio::test]
async fn re_reads_the_mount_view_before_a_trigger_answer_so_a_home_that_mounted_since_is_resolved_normally()
 {
    let unmounted = view_of(
        vec![
            MountEntry::new("/", "ext4"),
            MountEntry::new("/home", "autofs"),
        ],
        false,
    );
    let mounted = view_of(
        vec![
            MountEntry::new("/", "ext4"),
            MountEntry::new("/home", "autofs"),
            MountEntry::new("/home/alice", "nfs"),
        ],
        false,
    );
    let views = Arc::new(vec![unmounted, mounted]);
    let reads = Arc::new(AtomicUsize::new(0));
    let (rv, rr) = (Arc::clone(&views), Arc::clone(&reads));
    let lookup = create_automount_trigger_lookup(move || {
        let idx = rr.fetch_add(1, Ordering::SeqCst);
        let view = rv[idx.min(rv.len() - 1)].clone();
        async move { view }
    });

    assert_eq!(
        lookup.lookup("/home/alice/link/x").await.as_deref(),
        Some("/home/alice/link/x")
    );
    assert_eq!(lookup.lookup("/home/alice/link/x").await, None);
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn classifies_other_paths_from_the_cached_view_and_shares_one_re_read_among_concurrent_trigger_answers()
 {
    let unmounted = view_of(
        vec![
            MountEntry::new("/", "ext4"),
            MountEntry::new("/home", "autofs"),
        ],
        false,
    );
    let reads = Arc::new(AtomicUsize::new(0));
    let rr = Arc::clone(&reads);
    let lookup = create_automount_trigger_lookup(move || {
        rr.fetch_add(1, Ordering::SeqCst);
        let view = unmounted.clone();
        async move { view }
    });
    assert_eq!(lookup.lookup("/srv/x").await, None);
    assert_eq!(lookup.lookup("/srv/y").await, None);
    assert_eq!(reads.load(Ordering::SeqCst), 1);

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let l = lookup.clone();
            tokio::spawn(async move { l.lookup(format!("/home/user-{i}/x")).await })
        })
        .collect();
    for (i, h) in handles.into_iter().enumerate() {
        assert_eq!(h.await.unwrap(), Some(format!("/home/user-{i}/x")));
    }
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}

#[cfg(unix)]
mod unix_resolvers {
    use std::path::Path;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Mutex as StdMutex, OnceLock};
    use std::time::Duration;

    use maho_machine_ops::{
        AutomountRealpath, AutomountRealpathOptions, BoxFuture, CancellationToken, DEFAULT_LIMITS,
        FilesystemOperation, FilesystemPolicy, FilesystemPolicyDecision, FilesystemPolicyRequest,
        OpContext, OpErrorCode, OpName, Principal, PrincipalKind, Scope,
        acquire_file_mutation_lock, answer_automount_triggers_from, canonicalize_filesystem_path,
        check_path, spawn_path_in,
    };
    use tokio::sync::{Mutex as AsyncMutex, Notify};

    use super::*;

    fn test_serial() -> &'static AsyncMutex<()> {
        static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| AsyncMutex::new(()))
    }

    struct DenyDirPolicy(PathBuf);

    impl FilesystemPolicy for DenyDirPolicy {
        fn check<'a>(
            &'a self,
            req: &'a FilesystemPolicyRequest,
        ) -> BoxFuture<'a, FilesystemPolicyDecision> {
            Box::pin(async move {
                if req.canonical_path == self.0 || req.canonical_path.starts_with(&self.0) {
                    FilesystemPolicyDecision::deny("denied directory")
                } else {
                    FilesystemPolicyDecision::Allow
                }
            })
        }
    }

    struct TriggerFixture {
        _temp: tempfile::TempDir,
        trigger: PathBuf,
        denied: PathBuf,
        mounted_later: Arc<StdMutex<Vec<MountEntry>>>,
    }

    impl TriggerFixture {
        fn create() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(temp.path()).unwrap();
            let (trigger, denied) = (root.join("trigger"), root.join("denied"));
            std::fs::create_dir(&trigger).unwrap();
            std::fs::create_dir(&denied).unwrap();
            Self {
                _temp: temp,
                trigger,
                denied,
                mounted_later: Arc::new(StdMutex::new(Vec::new())),
            }
        }

        fn home_with_link(&self, name: &str) -> PathBuf {
            let home = self.trigger.join(name);
            std::fs::create_dir(&home).unwrap();
            std::os::unix::fs::symlink(&self.denied, home.join("link")).unwrap();
            home
        }

        fn read_view_fn(
            &self,
        ) -> impl Fn() -> BoxFuture<'static, MountView> + Send + Sync + 'static {
            let tm = self.trigger.to_string_lossy().into_owned();
            let ml = Arc::clone(&self.mounted_later);
            move || {
                let mut mounts = vec![
                    MountEntry::new("/", "ext4"),
                    MountEntry::new(tm.clone(), "autofs"),
                ];
                mounts.extend(ml.lock().unwrap().iter().cloned());
                Box::pin(async move { view_of(mounts, false) })
            }
        }

        fn ctx(&self) -> OpContext {
            let principal = Principal {
                kind: PrincipalKind::LocalSession,
                id: "automount-test".into(),
            };
            let mut ctx = OpContext::new(Scope::unrooted("automount-test"), principal);
            ctx.limits = DEFAULT_LIMITS;
            ctx.policy.filesystem = Some(Arc::new(DenyDirPolicy(self.denied.clone())));
            ctx
        }
    }

    #[tokio::test]
    async fn lets_the_policy_see_where_a_symlink_on_a_share_that_has_not_mounted_yet_leads_resolved_outside_this_process()
     {
        let _serial = test_serial().lock().await;
        let f = TriggerFixture::create();
        let guard =
            answer_automount_triggers_from(f.read_view_fn(), AutomountRealpathOptions::default());
        let target = f.home_with_link("alice").join("link/secret.txt");

        let err = check_path(
            &target,
            FilesystemOperation::Read,
            OpName::FilesRead,
            &f.ctx(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (OpErrorCode::PolicyDenied, "denied directory")
        );
        assert_eq!(
            canonicalize_filesystem_path(&target).await.unwrap(),
            f.denied.join("secret.txt")
        );
        let spawn_err = spawn_path_in(&f.trigger, Path::new("alice/link/secret.txt"), true)
            .await
            .unwrap_err();
        assert_eq!(spawn_err.code, OpErrorCode::PolicyDenied);
        assert!(guard.in_process_calls_under(&f.trigger).is_empty());
    }

    #[tokio::test]
    async fn refuses_a_path_whose_automounter_never_answers_killing_the_resolver_at_its_deadline() {
        let _serial = test_serial().lock().await;
        let f = TriggerFixture::create();
        let target = f.home_with_link("dave").join("link/secret.txt");
        let killed = Arc::new(AtomicBool::new(false));
        let kf = Arc::clone(&killed);
        let opts = AutomountRealpathOptions {
            deadline: Some(Duration::from_millis(200)),
            resolver: Some(Arc::new(move |_path, abort| {
                kf.store(abort.wait_aborted(Duration::from_secs(5)), Ordering::SeqCst);
                AutomountRealpath::Unanswered
            })),
        };
        let guard = answer_automount_triggers_from(f.read_view_fn(), opts);

        let err = canonicalize_filesystem_path(&target).await.unwrap_err();
        assert_eq!(err.code, OpErrorCode::PolicyDenied);
        assert!(
            err.message.contains("automount that did not answer"),
            "{}",
            err.message
        );
        assert!(killed.load(Ordering::SeqCst));
        assert!(guard.in_process_calls_under(&f.trigger).is_empty());
    }

    #[tokio::test]
    async fn keys_a_mutation_of_such_a_path_from_its_text_apart_from_the_file_its_symlink_names() {
        let _serial = test_serial().lock().await;
        let f = TriggerFixture::create();
        let guard =
            answer_automount_triggers_from(f.read_view_fn(), AutomountRealpathOptions::default());
        let direct_path = f.denied.join("draft.md");
        std::fs::write(&direct_path, "draft").unwrap();
        let trigger_draft = f.home_with_link("carol").join("link/draft.md");
        let (started, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        let (sw, rw) = (Arc::clone(&started), Arc::clone(&release));
        let started_waiter = started.notified();
        let through_trigger = tokio::spawn(async move {
            let _lock = acquire_file_mutation_lock(&trigger_draft, &CancellationToken::new())
                .await
                .unwrap();
            sw.notify_waiters();
            rw.notified().await;
            "through trigger"
        });
        started_waiter.await;

        let direct_lock = acquire_file_mutation_lock(&direct_path, &CancellationToken::new())
            .await
            .unwrap();
        drop(direct_lock);
        release.notify_waiters();

        assert_eq!(through_trigger.await.unwrap(), "through trigger");
        assert!(guard.in_process_calls_under(&f.trigger).is_empty());
    }

    #[tokio::test]
    async fn resolves_a_home_in_this_process_as_soon_as_it_has_mounted_without_waiting_for_the_view_to_age()
     {
        let _serial = test_serial().lock().await;
        let f = TriggerFixture::create();
        let guard =
            answer_automount_triggers_from(f.read_view_fn(), AutomountRealpathOptions::default());
        let home = f.home_with_link("bob");
        let target = home.join("link/secret.txt");
        assert_eq!(
            canonicalize_filesystem_path(&target).await.unwrap(),
            f.denied.join("secret.txt")
        );
        guard.clear_in_process_calls();

        f.mounted_later
            .lock()
            .unwrap()
            .push(MountEntry::new(home.to_string_lossy().into_owned(), "nfs"));
        assert_eq!(
            canonicalize_filesystem_path(&target).await.unwrap(),
            f.denied.join("secret.txt")
        );
        assert!(guard.in_process_calls_under(&home).contains(&target));
    }
}
