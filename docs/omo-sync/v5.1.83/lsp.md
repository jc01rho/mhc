# omo v5.1.80..v5.1.83 sync: lsp

Range: oh-my-openagent `822bd923038..69a1863a457` (v5.1.79 to v5.1.83; v5.1.80 was never released).
Upstream paths: `packages/lsp-core` (8 non-merge commits, all under `#14642`).
mhc counterpart: `crates/omo/lsp-core`.

## Summary

| verdict | rows |
|---|---:|
| port | 7 |
| ported | 0 |
| n/a | 1 |
| gap | 0 |
| total | 8 |

One row per upstream commit in `packages/lsp-core` over the range (all 8 commits reference `#14642`, merged in `#14656`).

## Per PR

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #14642 / #14656 (`f05081f053`) | port | `crates/omo/lsp-core/src/lsp/native_typescript.rs` (`native_typescript_command`), `server_resolution.rs` (`find_server_for_extension_with_options`, `get_all_servers_with_options`), and `tests/server_resolution_typescript_native.rs`. Note: TS `withHostJsRuntime` (`host-js-runtime.ts`) is not ported because mhc runs natively rather than under Bun. |
| #14642 / #14656 (`8aca434b9f`) | port | `crates/omo/lsp-core/src/lsp/native_typescript.rs` (`native_typescript_search_directory`), `config_loader.rs` (`ServerWithSource::inherits_builtin_command`, `configured_initialization`), `server_resolution.rs` (`find_server_for_extension_for_target`), `client_wrapper.rs` (`with_lsp_client`), and `tests/server_resolution_typescript_native.rs`. |
| #14642 / #14656 (`03a73ed9de`) | port | `crates/omo/lsp-core/src/lsp/client_key.rs` (`lsp_client_key`), `manager.rs` (`LspManager` keyed by `(root, server_id, command)`, `get_snapshot` reading stored `root`/`server_id`), `client_wrapper.rs`, `manager_tests.rs` (`given_one_root_when_files_resolve_to_different_typescript_commands_then_each_command_gets_its_own_client_and_the_same_command_shares_one`), and `tests/lifecycle_latest.rs`. |
| #14642 / #14656 (`78f69f4bad`) | port | `crates/omo/lsp-core/src/lsp/native_typescript.rs` (`native_typescript_search_directory` real-path session containment, `is_real_directory`), `directory_server_groups.rs` (`group_files_by_client`), `directory_diagnostics.rs`, `tests/server_resolution_typescript_native.rs`, and `tests/directory_diagnostics_mixed_typescript.rs`. |
| #14642 / #14656 (`ece0df4f2f`) | port | `crates/omo/lsp-core/src/lsp/native_typescript.rs` (`find_project_tsc` restricted to `NODE_PROJECT_MARKERS` + `node_modules/.bin`), `server_installation.rs` (`NODE_PROJECT_MARKERS`), `directory_group_scan.rs` (`scan_served_groups`), `directory_diagnostics.rs`, and `tests/directory_diagnostics_group_scan.rs`. |
| #14642 / #14656 (`e1ebcc449c`) | port | `crates/omo/lsp-core/src/lsp/client_startup.rs` (`ManagedClient`, `await_with_signal`, `await_init`), `manager.rs` (`detach_waiter`, `discard_if_same`, tracked `pending_stops`), `native_typescript.rs` (skipping stale `tsc` shims lacking `node_modules/typescript/package.json`), `directory_group_scan.rs` (`acquire_within_init_timeout`, abort rethrow after group settlement), `directory_diagnostics_tests.rs` (`abort_after_first_file_schedules_no_later_file`), and `tests/directory_diagnostics_group_scan.rs`. |
| #14642 / #14656 (`9b377baa8c`) | port | `crates/omo/lsp-core/src/lsp/timer_provider.rs` (`TimerProvider`, `RealTimerProvider`, `repeat_every`), `client_startup.rs`, `manager.rs` (atomic waiter uncount + reference acquisition, `release_client` instance check, injected `timers`), `client_wrapper.rs`, `directory_group_scan.rs`, `tests/support/controlled_clock.rs`, `tests/manager_acquisition.rs`, and `tests/manager_reaping.rs`. |
| #14642 / #14656 (`002393d99c`) | n/a | Drops two TS `export` keywords (`gate`, `GatedClient`) in `packages/lsp-core/src/lsp/manager-test-support.ts` for TS dead-code checks; Rust helper structs in `tests/support/manager_test_support.rs` are module-scoped. |
