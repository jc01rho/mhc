# Todo senpi-engine-automount: maho-machine-ops automount trigger detection and isolated realpath (senpi v2026.11.23, senpi#7158 / #7157)

Ports senpi PR `#7158` (`44cb2dc5b`, fixes `#7157`: commits `012c41356`, `46873332d`, `fcb053c78`, `473d1cb85`, `1a848cf20`) into `crates/maho-machine-ops`.

| TS path | Rust path | TS tests | Rust tests | status |
|---|---|---:|---:|---|
| files/mount-table.ts | src/files/mount_table.rs | 0 | 0 | done |
| files/automount-realpath.ts | src/files/automount_realpath.rs | 0 | 0 | done |
| files/automount-trigger.ts | src/files/automount_trigger.rs | 0 | 0 | done |
| files/mutation-queue.ts | src/files/path_lock.rs | 0 | 0 | partial: per-canonical-path slot and automount trigger text key |
| policy/filesystem.ts | src/policy/filesystem.rs | 0 | 0 | done |
| rpc/machine-op/real-path.ts | src/real_path.rs | 0 | 0 | done |
| test/automount-trigger.test.ts | tests/automount_trigger.rs | 13 | 13 | done |
