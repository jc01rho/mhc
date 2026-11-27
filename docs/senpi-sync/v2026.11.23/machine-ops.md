# senpi v2026.11.23 sync ledger — track `senpi-engine` slice `automount` (`machine-ops`)

Range: `f01af134919f..47333c26cdba` (`v2026.11.21-2` to `v2026.11.23`).
Input specification: `slices/automount.md` (1 upstream PR / 5 commits: `012c41356`, `46873332d`, `fcb053c78`, `473d1cb85`, `1a848cf20`, merged as `#7158` `44cb2dc5b`).

## Summary

- `port`: 1 (`#7158`)
- `ported`: 0
- `golden`: 0
- `gap`: 0
- `n/a`: 0

## Verdicts

| senpi PR | verdict | mhc location or reason |
| --- | --- | --- |
| `#7158` (`44cb2dc5b`, fixes `#7157`; commits `012c41356`, `46873332d`, `fcb053c78`, `473d1cb85`, `1a848cf20`) | `port` | `crates/maho-machine-ops/src/files/mount_table.rs` (`MountEntry`, `parse_linux_mount_info`, `parse_darwin_mount_table`, `read_mount_table`), `crates/maho-machine-ops/src/files/automount_realpath.rs` (`AutomountRealpath`, `AutomountAbort`, `AutomountRealpathOptions`, `canonicalize_by_opening_sync`, `realpath_out_of_process` with private 5000ms deadline on a dedicated `std::thread::spawn` outside Tokio's shared blocking pool), `crates/maho-machine-ops/src/files/automount_trigger.rs` (`MountView`, `unmounted_automount_path`, `create_automount_trigger_lookup`, `answer_automount_triggers_from`, `realpath_under_automount`, `automount_trigger_path`), `crates/maho-machine-ops/src/files/path_lock.rs` (`mutation_queue_key` keys unmounted automount trigger paths from text without filesystem I/O), `crates/maho-machine-ops/src/policy/filesystem.rs` (`canonicalize_filesystem_path_with` -> `canonicalize_under_automount`, refusing `Unanswered` as `OpErrorCode::PolicyDenied`), `crates/maho-machine-ops/src/real_path.rs` (`real_path_under` delegates unmounted automount trigger paths to `realpath_under_automount`), `crates/maho-machine-ops/tests/automount_trigger.rs` (13/13 cases from `packages/machine-ops/test/automount-trigger.test.ts`), `crates/maho-machine-ops/parity.d/senpi-engine-automount.md` |

## Design notes

- Upstream `automount-realpath.ts` spawns `process.execPath -e <RESOLVER> <path>` and kills it with `SIGKILL` after 5000ms so a wedged automount cannot hold a thread on the runtime's shared filesystem pool. Native `mhc` has no `-e` JS evaluator, so `realpath_out_of_process` spawns a dedicated `std::thread::spawn` (never `tokio::task::spawn_blocking`) running `canonicalize_by_opening_sync` and awaits a `tokio::sync::oneshot` channel with the module-private 5000ms deadline (`AUTOMOUNT_RESOLUTION_DEADLINE_MS`), signalling `AutomountAbort` and returning `AutomountRealpath::Unanswered` (`OpErrorCode::PolicyDenied`) when the deadline expires.
