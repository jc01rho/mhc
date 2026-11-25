# omo v5.1.80..v5.1.83 sync: senpi-task-runners

Range: oh-my-openagent `822bd923038`..`69a1863a457` (v5.1.79 to v5.1.83; v5.1.80 was never released).
Upstream paths: `packages/senpi-task/src/runners`, `packages/senpi-task/src/store`, `packages/senpi-task/src/platform`,
`packages/senpi-task/src/senpi` — 23 non-merge commits.
mhc counterpart: `crates/omo/senpi-task/` (`runners/`, `store/`, `senpi/`) plus the planner's `split_model_decorators`
in `crates/omo/components/maho-omo-task/src/planner_explicit_pin.rs`.

## Summary

| verdict | rows |
|---|---:|
| port | 4 |
| ported | 2 |
| gap | 11 |
| n/a | 4 |
| **total** | **21** |

One row per upstream PR or commit group; every non-merge commit in the range is named in exactly one row
(`3d4af11b30` and its release split across a `port` and an `n/a` row, both named, because its `senpi/` half and its
thread-tools half have different owners).

## Why the socket-client half is a gap, not a port

Fourteen of the commits in this range change `runners/rpc-host/socket-client.ts`: the caller-stall meter, the
one-budget redial rule, the host-drop classification and the opt-in read-RPC diagnostic. mhc has no file-for-file
counterpart for any of them. Its JSONL socket conversation is `crates/maho-rpc/src/rpc_client.rs`, which takes one
wall-clock deadline per request and redials nothing; `crates/omo/senpi-task/src/runners/rpc_host/` reaches a host
through `HostSessionPort` (`session_client_connection.rs`, `reattach_port.rs`, `unresponsive_close.rs`) and never
opens a socket itself. Porting these means giving `maho-rpc`'s client a caller-stall meter and a redial attempt loop —
a different crate and a different track — so the rows below record the gap and name what it needs instead of landing
an emitter with no call site.

## Per PR

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #14809 (`ff12bf99f6`, `8b014b1eac`, `ff5393c2f1`) | gap | Manual-clock tests of `socket-client.ts`'s redial budget (probe takeover, armed-timer list `[400, 400, 100, 100]`, fail-loudly assertions). mhc's `crates/omo/senpi-task/src/runners/rpc_host/` has no socket conversation at all: the JSONL socket client lives in `maho-rpc` (`crates/maho-rpc/src/rpc_client.rs`) and answers redial differently, so there is no `withRpcSocket` to clock. Needs the whole socket-transport layer (see the #14624 rows). |
| #14808 review M1 (`66763e56b7`) | port | Darwin `proc_taskinfo` CPU times are Mach absolute-time ticks, not nanoseconds: 1 ns per tick on Intel, 125/3 ns on Apple Silicon. `crates/omo/senpi-task/src/platform/native_process.rs` `scale_mach_ticks(ticks, numer, denom)` scales by `mach_timebase_info` and `read_process_cpu_seconds` applies it; the arithmetic and the 41.7x error it removes are pinned by `scale_mach_ticks_agrees_with_the_runtime_clock` on every platform. |
| `958842b2c8` (no PR; #13165 tests) | port | The startup CPU cap is driven by an injected advancing CPU reading instead of host load. `RpcProcessRunnerOptions::read_cpu_seconds` is the injection seam and `a_runaway_engine_is_stopped_by_the_startup_cpu_cap` advances the reading 0.1 s per sample, so the cap trips whatever share of a real CPU the test child got. |
| #13165 (`147e4d47d6`) | port | `crates/omo/senpi-task/src/runners/rpc_process.rs`: `StartupBounds` + `with_startup_progress` bound the wait for the engine's first answer (`get_protocol_info`) by its CPU progress (10 s stall window, 120 s CPU cap, 2 % progress epsilon, 250 ms sampling) with a 10 min outer bound that holds when the CPU cannot be read; `fallback_chain_deadline_ms` now covers only `set_retry_fallback`. Not ported from the same commit: moving the kernel reads out of `store/lock-owner.ts` — mhc's `store/record_lock.rs` never probes a pid (file age is the staleness authority, a documented divergence), so there is nothing to move. Tests: `runners/rpc/rpc_tests/rpc_process.rs` (4). |
| #14624 review M5, M4 (`d83b94a9f3`) | gap | A probe tick needs its own clock for the caller-stall meter's recovery verdict, and the caller-stall test needs a translated-address endpoint. Both are `socket-client.ts` internals (see #14444 below); mhc has no stall meter. |
| #14624 review M5 (`c460dd28af`) | gap | A host drop counts as the stall's only before the caller wrote or before it ran on time again: `lostConnections`/`CallerStallMeter::recovered` in `socket-client.ts`. No counterpart; `maho-rpc`'s client redials nothing today. |
| #14624 review M5, L2 (`ce10661ecc`) | gap | Redial only a drop seen as a stall ends, and pause the stall probe while a subscription is passive: `socket-client.ts` + `socket-client-edge-cases.test.ts` (22 controlled-clock cases). No counterpart. |
| #14624 review M1 (`01eaaf633b`) | gap | Redial only while the call has budget left (`performance.now() < deadline + meter.total()`), a `socket-client.ts` rule with no Rust consumer. |
| #14624 review M1, M2 (`445d0df079`) | gap | A redialed read keeps the call's one budget and only a host drop (`ECONNRESET`/`EPIPE`) is redialed: `withRpcSocket`'s attempt loop in `socket-client.ts`. No counterpart. |
| `1f4085ab06` (no PR; #14444) | gap | A local socket conversation's deadline stops while the caller's own event loop is blocked, and a read dropped during such a stall is dialed once more: the `callerStallMeter` half of `socket-client.ts`. mhc's `maho-rpc` client takes one wall-clock deadline per request (`rpc_client.rs`) and has no caller-stall accounting; porting it is `maho-rpc` work, not this crate's. |
| #14498 (`c1b1ae31cd`) | gap | Correct `readRpcDiagnostic` phases and log only transport failures. `crates/omo/senpi-task/` has no `read-rpc-diagnostic` counterpart and no consumer: the read RPC path is `maho-rpc`'s, and an emitter with no call site would be dead code. |
| `1c280530f6` (no PR; #14498 feature) | gap | Opt-in `OMO_THREAD_READ_TRACE=1` diagnostics for failed read RPCs (`read-rpc-diagnostic.ts`, new file). Same missing socket layer and consumer as #14498. |
| `a74db808d7` (no PR) | n/a | TypeScript test fixture only (`store/__fixtures__/owned-task-state.ts` `ownedTaskStateConfig`), wiring a project-scoped `task.state_dir` into the paused-run DAG cases. Rust's `dag/e2e_tests.rs` already builds one `create_dag_file_store(&DagStoreConfig::new(&project))` per test over its own `tempfile::tempdir().keep()`, so fixture, engine and DAG runtime share one store without a config helper. |
| `1a0471480b` (no PR, #14607 review M1) | ported | Every test's task state stays out of the shared agent dir. mhc's `store/record_store.rs` has no default agent-dir fallback: `resolve_state_dir` (`store/types.rs`) is `task_state_dir` or `project_dir/.omo/senpi-task`, and every store in `store/store_tests.rs` and the manager fixtures is built over a `tempfile::tempdir()`, so the child-process scripts the upstream fix had to redirect write inside their own project too. |
| #11632 (`4f00b2a657`) | n/a | An in-memory `TaskRecordStore` double plus a contract suite run against both stores. mhc's store is a concrete struct (`store/record_store.rs` `TaskRecordStore`), not the TS `TaskRecordStore` interface, so there is no second implementation to hold to one contract; the double exists to remove the file store's 19 s lock/stat/rename cost from a Bun e2e case, and Rust's `dag/e2e_tests.rs` runs one manager per test over a tempdir instead. Crash durability is owned by `store/store_tests.rs`. |
| `3d4af11b30` (no PR; thread tools half) | n/a | `components/thread/tools/model-match.ts` (thinking suffixes, absent-provider naming) and `control/operations.ts` belong to the thread tracks (`thread-core`/`thread-control`); this track owns only the `senpi/` half, ported in the row below. |
| `3d4af11b30` (no PR; `senpi/` half) | port | `crates/omo/senpi-task/src/senpi/model_decorators.rs` (new) carries `SENPI_THINKING_LEVELS`, `SENPI_SERVICE_TIERS` and `split_model_decorators(reference, thinking_only)` with upstream's right-to-left, at-most-two grammar, so a genuine colon id (`provider/model:exacto`) stays glued instead of being read as a thinking level. `crates/omo/components/maho-omo-task/src/planner_explicit_pin.rs` `split_model_decorators` now delegates to it and keeps its `PinDecorators` type and its service-tier refusal. Tests: 6 in `senpi/model_decorators.rs`, 1 regression in `planner_explicit_pin.rs`. |
| `ab77e30cfe` (no PR) | n/a | Clears the leftover-socket readiness timer in `busy-host-probe.test.ts` once the listener is ready; a Bun test-harness detail with no Rust counterpart (there is no `busy-host-probe.test.ts` port). |
| `c18d8f169c` (no PR) | gap | `probeSocket` answers accept / refused / unknown (with the reason) instead of a boolean, and its three cases start real listeners. mhc's only socket probe is `maho_rpc::host_probe::probe_socket_reachable`, which answers a bare bool and is `maho-rpc`'s surface; `crates/omo/senpi-task/` has no `busy-host.ts` counterpart to host the tri-state. |
| `1f7a72d214` (no PR; #14437) | gap | Points memory-sync's status at senpi-task's shared socket probe. mhc's sync-status path is `maho-omo-memory` + `maho-native`, which never calls into `senpi-task` for a probe; the consumer side of `probeSocket` is outside this track. |
| #14300 (`658f06380d`, `9eed8276f2`) | ported | `crates/omo/senpi-task/src/runners/rpc/terminate.rs` `platform` (non-unix): `taskkill.exe` is raced against the child's exit event and one 2 s observation window, a still-running tree kill is left to finish detached (`/T /F` must not be cut partway), and the forced kill's exit is observed for a bounded window (`terminate.rs` module header, `PROCESS_EXIT_OBSERVATION_TIMEOUT`). The observation-window timer the review asked to clear has no separate handle here: the loop owns it and drops it when another branch wins. |

## Tests

New cases, all under `crates/omo/senpi-task` unless named:

- `src/platform/native_process.rs`: `read_process_cpu_seconds_rejects_a_pid_that_is_not_a_positive_integer`,
  `scale_mach_ticks_agrees_with_the_runtime_clock`, `proc_stat_cpu_ticks_survive_a_command_name_with_spaces_and_parens`,
  `this_process_reads_its_own_cpu_seconds` (#14808, #13165).
- `src/runners/rpc/rpc_tests/rpc_process.rs`: `an_idle_engine_that_never_answers_is_stopped_by_the_stall_window`,
  `a_runaway_engine_is_stopped_by_the_startup_cpu_cap`,
  `an_unreadable_cpu_reading_still_hits_the_startup_outer_bound`,
  `a_cold_start_that_outlasts_the_chain_deadline_still_takes_its_chain` (#13165, `958842b2c8`).
- `src/senpi/model_decorators.rs`: `thinking_level_and_service_tier_suffixes_split_right_to_left`,
  `a_genuine_colon_id_stays_glued`, `thinking_only_leaves_a_service_tier_on_the_id`,
  `at_most_two_decorators_are_stripped`, `a_bare_or_trailing_colon_is_not_a_decorator`,
  `vocabularies_are_upstreams` (`3d4af11b30`).
- `crates/omo/components/maho-omo-task/src/planner_explicit_pin.rs`:
  `a_colon_model_id_is_never_mistaken_for_a_thinking_level` (`3d4af11b30`).
- `src/runners/rpc/rpc_tests/protocol_client.rs`: the designated-chain and manager-fallback cases now inject their
  startup bounds, so the "silent engine" case fails as a stall inside one sample window instead of waiting out a
  wall-clock deadline (#13165).

## Seen, not changed here

- `store/record_lock.rs` judges a lock's staleness by file age and never probes the holder's pid, so `lock-owner.ts`'s
  `readProcessStartIdentity` and the `platform/native-process.ts` extraction it drove have no Rust counterpart to
  move. That divergence predates this range (parity fragments `2.md`/`41.md`).
- `RpcProcessRunnerOptions` gained four `Option<...>` startup fields and one injection seam
  (`read_cpu_seconds`); every literal that does not name them keeps compiling through `Default`.
