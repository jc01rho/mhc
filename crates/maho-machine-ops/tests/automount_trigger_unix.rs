#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use maho_machine_ops::{MountEntry, MountView};

fn view_of(mounts: Vec<MountEntry>, case_insensitive: bool) -> MountView {
    MountView::new(mounts, case_insensitive)
}

mod unix_resolvers {
    use std::path::Path;
    use std::sync::Mutex as StdMutex;
    use std::sync::OnceLock;
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
        let (killed_tx, killed_rx) = tokio::sync::oneshot::channel();
        let opts = AutomountRealpathOptions {
            deadline: Some(Duration::from_millis(200)),
            resolver: Some(Arc::new(move |_path, abort| {
                let was_aborted = abort.wait_aborted(Duration::from_secs(5));
                let _ = killed_tx.send(was_aborted);
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
        let killed = tokio::time::timeout(Duration::from_secs(5), killed_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(killed);
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
