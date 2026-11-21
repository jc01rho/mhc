// Shared by tests/path_escape.rs and tests/files.rs; each test crate uses only part of it.
#![allow(dead_code)]

use std::fmt::Debug;
use std::path::{Path, PathBuf};

use maho_machine_ops::{
    Limits, LocalMachineOpsOptions, MachineOpError, MachineOps, OpContext, OpErrorCode, Principal,
    PrincipalKind, Scope, create_local_machine_ops,
};

pub const INSIDE_TEXT: &str = "hello world\nsecond line\n";
pub const SECRET_TEXT: &str = "secret\n";
pub const READ_LIMIT: u64 = 64;

/// `base/root` is the scope root (holding `inside.txt`, `dir/a.ts` and, on unix, `escape` -> `outside`);
/// `base/outside` holds `secret.txt`; `base/root-evil` shares a string prefix with the root.
pub struct Fixture {
    pub base: tempfile::TempDir,
    pub root: PathBuf,
    pub outside: PathBuf,
    pub sibling: PathBuf,
    pub ops: MachineOps,
}

impl Fixture {
    pub fn create() -> Self {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("root");
        let outside = base.path().join("outside");
        let sibling = base.path().join("root-evil");
        std::fs::create_dir_all(root.join("dir")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(root.join("inside.txt"), INSIDE_TEXT).unwrap();
        std::fs::write(root.join("dir").join("a.ts"), "export const a = 1;\n").unwrap();
        std::fs::write(outside.join("secret.txt"), SECRET_TEXT).unwrap();
        std::fs::write(sibling.join("secret.txt"), SECRET_TEXT).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        Self {
            base,
            root,
            outside,
            sibling,
            ops: create_local_machine_ops(LocalMachineOpsOptions::default()),
        }
    }

    pub fn ctx(&self) -> OpContext {
        self.ctx_rooted(&self.root)
    }

    pub fn ctx_rooted(&self, root: &Path) -> OpContext {
        let mut ctx = OpContext::new(
            Scope::rooted("conformance", root),
            Principal {
                kind: PrincipalKind::RemoteDevice,
                id: "conformance-device".to_owned(),
            },
        );
        ctx.limits = Limits {
            read_max_bytes: READ_LIMIT,
        };
        ctx
    }
}

pub fn code<T: Debug>(result: Result<T, MachineOpError>) -> OpErrorCode {
    result.unwrap_err().code
}

pub fn oversized() -> Vec<u8> {
    vec![b'a'; READ_LIMIT as usize + 1]
}

pub fn text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}
