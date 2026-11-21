//! Port of senpi `test/conformance/files.ts` and `test/glob-trailing-separator.test.ts`: result shapes and
//! error codes of read, write, edit, glob, grep, stat and list, the gate hooks, and the mutation queue.
//! The read limit is 64 bytes here instead of 10 MiB (`common::READ_LIMIT`), so the oversize cases stay small.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Fixture, INSIDE_TEXT, READ_LIMIT, SECRET_TEXT, code, oversized, text};
use maho_machine_ops::{
    AccessMode, AuditEntry, AuditVerdict, BoxFuture, EditRequest, EntryType, GlobRequest,
    GrepMatch, GrepRequest, ListRequest, OpArgs, OpContext, OpErrorCode, OpHooks, OpName,
    OpOutcome, PathLock, PolicyDecision, ReadRequest, StatRequest, WriteRequest,
};
use tokio::sync::Notify;

const BOUND: Duration = Duration::from_secs(10);

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, future)
        .await
        .expect("timed out waiting for an event")
}

#[tokio::test]
async fn read_returns_the_bytes_and_the_whole_file_size() {
    let f = Fixture::create();
    let result = f
        .ops
        .files
        .read(&ReadRequest::new("inside.txt"), &f.ctx())
        .await
        .unwrap();
    assert_eq!(result.bytes, INSIDE_TEXT.as_bytes());
    assert_eq!(result.size, INSIDE_TEXT.len() as u64);
    assert_eq!(result.offset, 0);
    assert_eq!(result.path, f.root.join("inside.txt"));
}

#[tokio::test]
async fn read_range_returns_a_slice_and_the_whole_size() {
    let f = Fixture::create();
    let request = ReadRequest {
        path: "inside.txt".into(),
        offset: Some(6),
        limit_bytes: Some(5),
    };
    let result = f.ops.files.read(&request, &f.ctx()).await.unwrap();
    assert_eq!(result.bytes, b"world");
    assert_eq!((result.size, result.offset), (INSIDE_TEXT.len() as u64, 6));
    let past_the_end = ReadRequest {
        path: "inside.txt".into(),
        offset: Some(1000),
        limit_bytes: Some(5),
    };
    let result = f.ops.files.read(&past_the_end, &f.ctx()).await.unwrap();
    assert!(result.bytes.is_empty());
}

#[tokio::test]
async fn read_missing_is_not_found_and_oversized_is_limit_exceeded() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    assert_eq!(
        code(files.read(&ReadRequest::new("missing.txt"), &ctx).await),
        OpErrorCode::NotFound
    );
    std::fs::write(f.root.join("big.bin"), oversized()).unwrap();
    assert_eq!(
        code(files.read(&ReadRequest::new("big.bin"), &ctx).await),
        OpErrorCode::LimitExceeded
    );
    let wants_too_much = ReadRequest {
        path: "inside.txt".into(),
        offset: None,
        limit_bytes: Some(READ_LIMIT + 1),
    };
    assert_eq!(
        code(files.read(&wants_too_much, &ctx).await),
        OpErrorCode::LimitExceeded
    );
    let range_of_oversized = ReadRequest {
        path: "big.bin".into(),
        offset: Some(0),
        limit_bytes: None,
    };
    assert_eq!(
        code(files.read(&range_of_oversized, &ctx).await),
        OpErrorCode::LimitExceeded
    );
}

#[tokio::test]
async fn an_unbounded_read_limit_skips_the_size_check() {
    let f = Fixture::create();
    std::fs::write(f.root.join("big.bin"), oversized()).unwrap();
    let mut ctx = f.ctx();
    ctx.limits.read_max_bytes = u64::MAX;
    let result = f
        .ops
        .files
        .read(&ReadRequest::new("big.bin"), &ctx)
        .await
        .unwrap();
    assert_eq!(result.bytes.len(), oversized().len());
}

#[tokio::test]
async fn write_creates_the_file_and_read_max_bytes_does_not_cap_writes() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    let result = files
        .write(&WriteRequest::new("new.txt", "written"), &ctx)
        .await
        .unwrap();
    assert_eq!(result.written_bytes, 7);
    assert_eq!(text(&f.root.join("new.txt")), "written");
    files
        .write(&WriteRequest::new("big.bin", oversized()), &ctx)
        .await
        .unwrap();
    assert_eq!(
        std::fs::metadata(f.root.join("big.bin")).unwrap().len(),
        READ_LIMIT + 1
    );
}

#[tokio::test]
async fn write_needs_createparentdirs_for_a_missing_parent() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    assert_eq!(
        code(
            files
                .write(&WriteRequest::new("nope/x.txt", "x"), &ctx)
                .await
        ),
        OpErrorCode::NotFound
    );
    let mut request = WriteRequest::new("nope/x.txt", "x");
    request.create_parent_dirs = true;
    files.write(&request, &ctx).await.unwrap();
    assert_eq!(text(&f.root.join("nope").join("x.txt")), "x");
}

#[tokio::test]
async fn ranges_written_in_any_order_assemble_the_file() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    let tail = WriteRequest {
        offset: Some(6),
        total: Some(11),
        ..WriteRequest::new("ranged.txt", "world")
    };
    let head = WriteRequest {
        offset: Some(0),
        total: Some(11),
        ..WriteRequest::new("ranged.txt", "hello ")
    };
    files.write(&tail, &ctx).await.unwrap();
    files.write(&head, &ctx).await.unwrap();
    assert_eq!(text(&f.root.join("ranged.txt")), "hello world");
}

#[tokio::test]
async fn a_range_whose_total_does_not_cover_it_is_invalid_argument() {
    let f = Fixture::create();
    let bad = WriteRequest {
        offset: Some(4),
        total: Some(6),
        ..WriteRequest::new("ranged.txt", "world")
    };
    assert_eq!(
        code(f.ops.files.write(&bad, &f.ctx()).await),
        OpErrorCode::InvalidArgument
    );
    assert!(!f.root.join("ranged.txt").exists());
}

#[tokio::test]
async fn edit_replaces_every_occurrence() {
    let f = Fixture::create();
    let result = f
        .ops
        .files
        .edit(&EditRequest::new("inside.txt", "l", "L"), &f.ctx())
        .await
        .unwrap();
    let expected = INSIDE_TEXT.replace('l', "L");
    assert_eq!(result.replacements, INSIDE_TEXT.matches('l').count());
    assert_eq!(result.content, expected);
    assert_eq!(result.write_result.written_bytes, expected.len() as u64);
    assert_eq!(text(&f.root.join("inside.txt")), expected);
}

#[tokio::test]
async fn edit_error_codes_and_no_effect() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    assert_eq!(
        code(
            files
                .edit(&EditRequest::new("missing.txt", "a", "b"), &ctx)
                .await
        ),
        OpErrorCode::NotFound
    );
    std::fs::write(f.root.join("big.bin"), oversized()).unwrap();
    assert_eq!(
        code(
            files
                .edit(&EditRequest::new("big.bin", "a", "b"), &ctx)
                .await
        ),
        OpErrorCode::LimitExceeded
    );
    assert_eq!(
        code(
            files
                .edit(&EditRequest::new("inside.txt", "", "x"), &ctx)
                .await
        ),
        OpErrorCode::InvalidPattern
    );
    assert_eq!(
        code(
            files
                .edit(&EditRequest::new("inside.txt", "absent", "x"), &ctx)
                .await
        ),
        OpErrorCode::NotFound
    );
    assert_eq!(text(&f.root.join("inside.txt")), INSIDE_TEXT);
}

#[tokio::test]
async fn glob_finds_files_by_pattern_and_search_directory() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    let result = files
        .glob(&GlobRequest::in_dir("*.ts", "dir"), &ctx)
        .await
        .unwrap();
    let paths: Vec<_> = result
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    assert_eq!(paths, vec![f.root.join("dir").join("a.ts")]);
    assert!(!result.truncated);
    assert_eq!(
        code(
            files
                .glob(&GlobRequest::in_dir("*", "missing-dir"), &ctx)
                .await
        ),
        OpErrorCode::NotFound
    );
}

#[tokio::test]
async fn read_max_bytes_does_not_apply_to_glob_stat_and_list() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    std::fs::write(f.root.join("dir").join("big.bin"), oversized()).unwrap();
    let globbed = files
        .glob(&GlobRequest::in_dir("*.bin", "dir"), &ctx)
        .await
        .unwrap();
    assert_eq!(globbed.entries.len(), 1);
    let stat = files
        .stat(&StatRequest::new("dir/big.bin"), &ctx)
        .await
        .unwrap();
    assert_eq!(stat.size, READ_LIMIT + 1);
    let mut names: Vec<_> = files
        .list(&ListRequest::new("dir"), &ctx)
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    names.sort();
    assert_eq!(names, ["a.ts", "big.bin"]);
}

#[tokio::test]
async fn grep_returns_matching_lines_with_their_shape() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    let result = files
        .grep(&GrepRequest::new(".", "second"), &ctx)
        .await
        .unwrap();
    assert_eq!(
        result.matches,
        vec![GrepMatch {
            path: f.root.join("inside.txt"),
            line: 2,
            text: "second line".to_owned()
        }]
    );
    assert!(!result.truncated);
    assert_eq!(
        code(
            files
                .grep(&GrepRequest::new("missing.txt", "x"), &ctx)
                .await
        ),
        OpErrorCode::NotFound
    );
    std::fs::write(f.root.join("big.bin"), oversized()).unwrap();
    assert_eq!(
        code(files.grep(&GrepRequest::new("big.bin", "a"), &ctx).await),
        OpErrorCode::LimitExceeded
    );
    assert_eq!(
        code(files.grep(&GrepRequest::new("inside.txt", "("), &ctx).await),
        OpErrorCode::InvalidPattern
    );
}

#[tokio::test]
async fn stat_describes_files_and_directories() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    let file = files
        .stat(&StatRequest::new("inside.txt"), &ctx)
        .await
        .unwrap();
    assert_eq!(file.entry_type, EntryType::File);
    assert_eq!(file.size, INSIDE_TEXT.len() as u64);
    assert!(file.mtime_ms > 0.0);
    let dir = files.stat(&StatRequest::new("dir"), &ctx).await.unwrap();
    assert_eq!(dir.entry_type, EntryType::Directory);
    assert_eq!(
        code(files.stat(&StatRequest::new("missing.txt"), &ctx).await),
        OpErrorCode::NotFound
    );
}

#[tokio::test]
async fn stat_access_requires_the_permission() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    for access in [AccessMode::Read, AccessMode::Write, AccessMode::ReadWrite] {
        let request = StatRequest {
            access: Some(access),
            ..StatRequest::new("inside.txt")
        };
        assert!(files.stat(&request, &ctx).await.is_ok(), "{access:?}");
    }
    let readable_dir = StatRequest {
        access: Some(AccessMode::Read),
        ..StatRequest::new("dir")
    };
    assert!(files.stat(&readable_dir, &ctx).await.is_ok());
    let missing = StatRequest {
        access: Some(AccessMode::Read),
        ..StatRequest::new("missing.txt")
    };
    assert_eq!(
        code(files.stat(&missing, &ctx).await),
        OpErrorCode::NotFound
    );
}

#[tokio::test]
async fn list_returns_entry_names_and_types() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    let result = files.list(&ListRequest::new("."), &ctx).await.unwrap();
    let entries: BTreeMap<_, _> = result
        .entries
        .into_iter()
        .filter(|entry| entry.name != "escape")
        .map(|entry| (entry.name, entry.entry_type))
        .collect();
    assert_eq!(
        entries,
        BTreeMap::from([
            ("dir".to_owned(), EntryType::Directory),
            ("inside.txt".to_owned(), EntryType::File),
        ])
    );
    assert_eq!(
        code(files.list(&ListRequest::new("missing-dir"), &ctx).await),
        OpErrorCode::NotFound
    );
}

#[cfg(unix)]
#[tokio::test]
async fn list_reports_a_symlink_as_a_symlink() {
    let f = Fixture::create();
    let result = f
        .ops
        .files
        .list(&ListRequest::new("."), &f.ctx())
        .await
        .unwrap();
    let escape = result
        .entries
        .iter()
        .find(|entry| entry.name == "escape")
        .unwrap();
    assert_eq!(escape.entry_type, EntryType::Symlink);
}

struct Recorder {
    seen: Mutex<Vec<String>>,
    deny: Option<&'static str>,
}

impl OpHooks for Recorder {
    fn before_op<'a>(
        &'a self,
        op: OpName,
        _args: OpArgs<'a>,
        _ctx: &'a OpContext,
    ) -> BoxFuture<'a, PolicyDecision> {
        Box::pin(async move {
            self.seen.lock().unwrap().push(format!("before {op}"));
            match self.deny {
                Some(reason) => PolicyDecision::deny(reason),
                None => PolicyDecision::Allow,
            }
        })
    }

    fn after_op<'a>(
        &'a self,
        op: OpName,
        _args: OpArgs<'a>,
        outcome: OpOutcome<'a>,
        _ctx: &'a OpContext,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let label = match outcome {
                OpOutcome::Ok => "ok".to_owned(),
                OpOutcome::Err(error) => error.code.to_string(),
            };
            self.seen
                .lock()
                .unwrap()
                .push(format!("after {op} {label}"));
        })
    }
}

#[tokio::test]
async fn before_op_denial_is_policy_denied_with_no_effect_and_is_audited() {
    let f = Fixture::create();
    let recorder = Arc::new(Recorder {
        seen: Mutex::new(Vec::new()),
        deny: Some("not today"),
    });
    let audit_log: Arc<Mutex<Vec<AuditEntry>>> = Arc::default();
    let mut ctx = f.ctx();
    ctx.policy.hooks = Some(recorder.clone());
    let sink = Arc::clone(&audit_log);
    ctx.audit = Some(Arc::new(move |entry| sink.lock().unwrap().push(entry)));
    let error = f
        .ops
        .files
        .write(&WriteRequest::new("denied.txt", "x"), &ctx)
        .await
        .unwrap_err();
    assert_eq!(error.code, OpErrorCode::PolicyDenied);
    assert_eq!(error.message, "not today");
    assert!(!f.root.join("denied.txt").exists());
    assert_eq!(*recorder.seen.lock().unwrap(), ["before files.write"]);
    let audit = audit_log.lock().unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].verdict, AuditVerdict::Denied);
    assert_eq!(audit[0].op, OpName::FilesWrite);
    assert_eq!(audit[0].target, "denied.txt");
}

#[tokio::test]
async fn an_allowed_operation_runs_between_before_and_after_and_after_sees_the_error() {
    let f = Fixture::create();
    let recorder = Arc::new(Recorder {
        seen: Mutex::new(Vec::new()),
        deny: None,
    });
    let audit_log: Arc<Mutex<Vec<AuditEntry>>> = Arc::default();
    let mut ctx = f.ctx();
    ctx.policy.hooks = Some(recorder.clone());
    let sink = Arc::clone(&audit_log);
    ctx.audit = Some(Arc::new(move |entry| sink.lock().unwrap().push(entry)));
    f.ops
        .files
        .read(&ReadRequest::new("inside.txt"), &ctx)
        .await
        .unwrap();
    f.ops
        .files
        .read(&ReadRequest::new("missing.txt"), &ctx)
        .await
        .unwrap_err();
    assert_eq!(
        *recorder.seen.lock().unwrap(),
        [
            "before files.read",
            "after files.read ok",
            "before files.read",
            "after files.read not_found"
        ]
    );
    let verdicts: Vec<_> = audit_log
        .lock()
        .unwrap()
        .iter()
        .map(|e| e.verdict)
        .collect();
    assert_eq!(verdicts, [AuditVerdict::Allowed, AuditVerdict::Allowed]);
}

#[tokio::test]
async fn a_cancelled_signal_aborts_before_the_effect() {
    let f = Fixture::create();
    let ctx = f.ctx();
    ctx.signal.cancel();
    assert_eq!(
        code(
            f.ops
                .files
                .write(&WriteRequest::new("x.txt", "x"), &ctx)
                .await
        ),
        OpErrorCode::Aborted
    );
    assert!(!f.root.join("x.txt").exists());
}

struct HoldFirstWrite {
    first_entered: Notify,
    release_first: Notify,
    order: Mutex<Vec<String>>,
}

impl OpHooks for HoldFirstWrite {
    fn before_op<'a>(
        &'a self,
        op: OpName,
        args: OpArgs<'a>,
        _ctx: &'a OpContext,
    ) -> BoxFuture<'a, PolicyDecision> {
        Box::pin(async move {
            let label = match args {
                OpArgs::Edit(request) => format!("{op}:{}", request.exact),
                _ => format!("{op}:write"),
            };
            self.order.lock().unwrap().push(label.clone());
            if label == "files.write:write" {
                self.first_entered.notify_one();
                self.release_first.notified().await;
            }
            PolicyDecision::Allow
        })
    }
}

// The edit starts only once the write holds the slot (it sits in its gate), through another spelling of the
// same file: it must queue behind the write and see its result. The queue itself is pinned deterministically
// by the unit tests in src/files/path_lock.rs.
#[tokio::test]
async fn two_mutations_of_one_canonical_path_serialize() {
    let f = Fixture::create();
    std::fs::write(f.root.join("shared.txt"), "zero").unwrap();
    let hold = Arc::new(HoldFirstWrite {
        first_entered: Notify::new(),
        release_first: Notify::new(),
        order: Mutex::new(Vec::new()),
    });
    let mut ctx = f.ctx();
    ctx.policy.hooks = Some(hold.clone());
    let files = f.ops.files;
    let first_request = WriteRequest::new("shared.txt", "first");
    let second_request = EditRequest::new("./dir/../shared.txt", "first", "second");
    let first = files.write(&first_request, &ctx);
    let second_after_first_entered = async {
        hold.first_entered.notified().await;
        let second = files.edit(&second_request, &ctx);
        let release = async { hold.release_first.notify_one() };
        tokio::join!(second, release).0
    };
    let (first, second) = bounded(async { tokio::join!(first, second_after_first_entered) }).await;
    first.unwrap();
    assert_eq!(second.unwrap().replacements, 1);
    assert_eq!(
        *hold.order.lock().unwrap(),
        ["files.write:write", "files.edit:first"]
    );
    assert_eq!(text(&f.root.join("shared.txt")), "second");
}

struct HoldA {
    a_entered: Notify,
    release_a: Notify,
}

impl OpHooks for HoldA {
    fn before_op<'a>(
        &'a self,
        _op: OpName,
        args: OpArgs<'a>,
        _ctx: &'a OpContext,
    ) -> BoxFuture<'a, PolicyDecision> {
        Box::pin(async move {
            if let OpArgs::Write(request) = args
                && request.path.ends_with("a.txt")
            {
                self.a_entered.notify_one();
                self.release_a.notified().await;
            }
            PolicyDecision::Allow
        })
    }
}

// Mutations of different paths overlap: b must finish while a still holds its own slot; a serialized queue
// would never let b through and the bounded wait fails the test.
#[tokio::test]
async fn mutations_of_different_paths_overlap() {
    let f = Fixture::create();
    let hold = Arc::new(HoldA {
        a_entered: Notify::new(),
        release_a: Notify::new(),
    });
    let mut ctx = f.ctx();
    ctx.policy.hooks = Some(hold.clone());
    let files = f.ops.files;
    let a_request = WriteRequest::new("a.txt", "a");
    let b_request = WriteRequest::new("b.txt", "b");
    let a = files.write(&a_request, &ctx);
    let b_then_release = async {
        hold.a_entered.notified().await;
        let result = files.write(&b_request, &ctx).await;
        hold.release_a.notify_one();
        result
    };
    let (a, b) = bounded(async { tokio::join!(a, b_then_release) }).await;
    a.unwrap();
    b.unwrap();
    assert_eq!(text(&f.root.join("a.txt")), "a");
    assert_eq!(text(&f.root.join("b.txt")), "b");
}

// A caller that already holds the slot (ctx.path_lock = Caller) must not deadlock on it: the lock taken
// here is the slot the write would otherwise wait for.
#[tokio::test]
async fn a_caller_holding_the_slot_runs_without_taking_it() {
    let f = Fixture::create();
    let held = maho_machine_ops::acquire_file_mutation_lock(
        &f.root.join("held.txt"),
        &maho_machine_ops::CancellationToken::new(),
    )
    .await
    .unwrap();
    let mut ctx = f.ctx();
    ctx.path_lock = Some(PathLock::Caller);
    bounded(f.ops.files.write(&WriteRequest::new("held.txt", "x"), &ctx))
        .await
        .unwrap();
    drop(held);
    assert_eq!(text(&f.root.join("held.txt")), "x");
}

#[tokio::test]
async fn the_secret_outside_the_root_is_never_touched_by_in_root_operations() {
    let f = Fixture::create();
    let (files, ctx) = (f.ops.files, f.ctx());
    files
        .write(&WriteRequest::new("inside.txt", "changed"), &ctx)
        .await
        .unwrap();
    assert_eq!(text(&f.outside.join("secret.txt")), SECRET_TEXT);
}
