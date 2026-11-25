# omo v5.1.80..v5.1.83 sync ledger — `memory` track

Source range: `822bd923038` (`v5.1.79`) .. `69a1863a457` (`v5.1.83`)
Upstream paths: `packages/omo-senpi/src/components/memory`, `packages/memory-core` (24 non-merge commits across 12 PRs/entries)

## Summary

| verdict | count |
|---|---:|
| port | 4 |
| ported | 1 |
| gap | 0 |
| n/a | 7 |
| **total** | **12** |

## Per-PR verdicts

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #14838 (`7b50cccd88`, ref #14837) | port | `crates/omo/components/maho-omo-memory/src/status_live.rs` (`MemoryFooterLive::start_recompute`, `is_current_generation`, `finish_recompute`, `stop`/`stop_session` generation bumps, and regression test `stale_recompute_never_records_fingerprint_so_restore_replaces_spinner`) |
| #14825 (`0bbb5afc00`, `1ac1959633`, `04ce925e58`, `670ee75836`, `b7e86df39f`, `5e91eb31cc`, `41060f084a`; refs #13046, #13050) | port | `crates/omo/components/maho-omo-memory/src/wiring_context.rs` (`StatusHandle`, `read_status_ui`, `guarded_status`, `sequenced_status`, per-host `HostStatusState` deduplicating unchanged text, recording state only after host acceptance, dropping stale refreshes, and giving clears their own `cleared_at` boundary), `status.rs` (`MemoryStatusUi::begin_refresh`, `read_dirty` under `memory_writer_lock_path` with `DIRTY_SAMPLE_LOCK_WAIT_MS = 2_000`, `refresh_memory_status` sequencing), `status_live.rs` (`MemoryFooterUi::begin_refresh`), `wiring.rs` (`NativeFooter`, `clear_status`), `wiring_reflection_live.rs` (`GuardedFooterUi`), and `crates/maho-cli/src/cli/memory_runtime.rs` (`StatusUi` sharing `StatusHandle`); `settled` footer restore runs synchronously in `MemoryFooterStatusLive::settle` |
| #14546 (`22a635bae1`, `238874cd18`; ref #14543) | port | `crates/omo/memory-core/src/tools/memory.rs` (`MemoryWriteStage`, `run_memory_tool_with_stage`, `run_memory_tool_as_with_stage`), `memory_apply_patch.rs` (`run_memory_apply_patch_with_stage`), `memory_tests.rs`, `crates/omo/components/maho-omo-memory/src/tool_call_bound.rs` (`MEMORY_CALL_TIMEOUT_MS`, `MemoryCallStage`, `MemoryCallState`, `BoundedCall`, `bound_memory_call`), `lib.rs`, and `tools.rs` (`MemoryToolsOptions::call_timeout_ms`, `spawn_blocking` isolation, abort/timeout stage reporting, late-commit silence, and FIFO `pre-commit` tests) |
| #14351 (`2090ccd3e4`, `d5d5bcf7cd`) | port | `crates/omo/memory-core/src/journal/store.rs` (`TranscriptTailCache` incremental tail reader keyed on `(byte_offset, modified)` with full reload on shrink/mtime change, `JournalCache.persisted_state_json` skipping no-op `state.json` writes while still persisting cursor/stale repairs, and temp-file cleanup on rename error) and `store_tests.rs`. Sibling `packages/omo-senpi/src/extension/settle-observers.ts` supersession belongs to `omo-sdk-relay` (`crates/omo/maho-omo/src/settle_observers.rs`) |
| #13077 (`a0b8b79abe`, `5f45c8f713`; ref #13074) | ported | `crates/omo/components/maho-omo-memory/src/kibitzer_hooks.rs` (`EventKind::ToolResult`), `recall_wiring.rs` (`on_tool_result`), `kibitzer_delivery.rs` (`KibitzerDelivery::on_tool_result`, `mount_recall_ports`), and `crates/maho-core/src/agent_session.rs` (`send_message` / `send_message_with_receipt`) are synchronous non-awaiting calls that queue steers immediately without blocking `ToolResult` on an awaited delivery promise |
| feat/14364-omo-sdk-entry (`32fe5b945b`) | n/a | Track slice (`device-session/account-runtime.ts`, `device-session/cli-session.ts`) only splits `engineDeviceService`/`cliCredentialStore`/`openCliSession` for the TS `omo-sdk` bundle (`packages/omo-senpi/src/components/omo-sdk/`, owned by `omo-sdk-relay`); native Rust `mhc` has no JS `omo-sdk` entry (`device_session/{account_session,account_source,credential_store,resolver}.rs` link directly; `maho-omo-gateway` and `maho-omo-remote` already consume `device_service_core` directly) |
| #14633 (`ef59fa2a93`, `11313553b7`; ref #14546) | n/a | TS Bun test-harness teardown fixes (`ManualClock.releaseWaits()`, `afterEach` cleanup in `engine-converge.test.ts` and `engine-harness-dispose.test.ts`); Rust `crates/omo/memory-core/src/sync/engine/*_tests.rs` uses self-contained per-test `tempfile::TempDir` RAII fixtures without a shared global clock or device registry |
| #14631 (`a67e4a587b`) | n/a | TypeScript strict-lint fix (`Promise.withResolvers<undefined>()`) in `sync-rpc-coordinator.test.ts`; no JS `Promise<void>` counterpart in Rust |
| #14601 (`102f0808ed`) | n/a | TS Bun `locks/subprocess-contention.test.ts` decoupling shared `teardownProbe` across Bun tests; already marked N/A in `crates/omo/memory-core/parity.d/2.md` (`locks/acquire_tests.rs` uses per-test temp dirs) |
| #14599 (`eddfcb2823`) | n/a | Shards `sync/item/idempotence.r3.test.ts` across 5 files to fit Bun's per-file 120s CI timeout; Rust `crates/omo/memory-core/src/sync/item/property_tests.rs` executes natively in `nextest` without sharding |
| #14597 (`9aa22d5216`) | n/a | Splits `sync/space/adopt-preservation.test.ts` across 2 files to fit Bun's 60s CI timeout; Rust `crates/omo/memory-core/src/sync/space/preservation_tests.rs` executes natively without sharding |
| #14533 (`8f8fa82e4d`, `6822512718`, `81455eaf47`; refs #12698, #14434) | n/a | TS child-process `SENPI_PRIVATE_FDS` fd 5/6 win32 pipe-pair test harness in `desktop-sync/memory-sync-channel.test-support.ts`, `index-engine.test.ts`, and `index-selection.test.ts`; Rust tests `desktop_sync/channel.rs` over an in-process duplex stream and intentionally returns `DesktopSyncUnavailableReason::PlatformNotSupported` on Windows (`cfg!(windows)`) |
