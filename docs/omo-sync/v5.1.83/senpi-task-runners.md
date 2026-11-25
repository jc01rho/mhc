# omo v5.1.80..v5.1.83 sync: senpi-task-runners

Range: oh-my-openagent `822bd923038`..`69a1863a457` (v5.1.79 to v5.1.83; v5.1.80 was never released).
Upstream paths: `packages/senpi-task/src/runners`, `packages/senpi-task/src/store`, `packages/senpi-task/src/platform`,
`packages/senpi-task/src/senpi` — 23 non-merge commits.
mhc counterpart: `crates/omo/senpi-task/` (`runners/`, `store/`, `senpi/`) plus the planner's `split_model_decorators`
in `crates/omo/components/maho-omo-task/src/planner_explicit_pin.rs`.

## Summary

| verdict | rows |
|---|---:|
| port | 15 |
| ported | 2 |
| gap | 0 |
| n/a | 4 |
| **total** | **21** |

One row per upstream PR or commit group; every non-merge commit in the range is named in exactly one row
(`3d4af11b30` and its release split across a `port` and an `n/a` row, both named, because its `senpi/` half and its
thread-tools half have different owners).

## Where the socket-client half landed

Fourteen of the commits in this range change `runners/rpc-host/socket-client.ts`: the caller-stall meter, the
one-budget redial rule, the host-drop classification and the opt-in read-RPC diagnostic. mhc's port of that file is
`crates/maho-rpc/src/socket_client.rs` (`RpcSocket`, from the omo v5.1.47 port), so the v5.1.83 conversation lands
there as `with_rpc_socket`; `read-rpc-diagnostic.ts` is `crates/maho-rpc/src/read_rpc_diagnostic.rs` and
`busy-host.ts`'s `probeSocket` is `crates/maho-rpc/src/host_probe.rs` `probe_socket` (parity fragment
`crates/maho-rpc/parity.d/5183s.md`). Upstream's only caller that passes a read command is the thread live surface's
`requestFrame`; mhc's live surface talks through the `RpcTransport` trait, which has no socket implementation yet, so
the redial and trace rules have no Rust consumer. They are ported as the module plus its tests, run on a manual clock
that the conversation's injected `RpcSocketRuntime` (clock, timers, stall probe) hands to the test: no sleeps.

## Per PR

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #14809 (`ff12bf99f6`, `8b014b1eac`, `ff5393c2f1`) | port | `crates/maho-rpc/src/socket_client.rs` `with_rpc_socket` arms every timer and its stall probe through the injected `RpcSocketRuntime`, so `tests/socket_client_redial.rs` `a_redialed_read_gets_only_what_is_left_of_the_calls_budget` reads the armed list `[400, 400, 100, 100]` off a manual clock, takes over only the conversation's own timers and probe, and fails loudly on the captured probe and first timer instead of hanging. No Rust caller yet: module + tests. |
| #14808 review M1 (`66763e56b7`) | port | Darwin `proc_taskinfo` CPU times are Mach absolute-time ticks, not nanoseconds: 1 ns per tick on Intel, 125/3 ns on Apple Silicon. `crates/omo/senpi-task/src/platform/native_process.rs` `scale_mach_ticks(ticks, numer, denom)` scales by `mach_timebase_info` and `read_process_cpu_seconds` applies it; the arithmetic and the 41.7x error it removes are pinned by `scale_mach_ticks_agrees_with_the_runtime_clock` on every platform. |
| `958842b2c8` (no PR; #13165 tests) | port | The startup CPU cap is driven by an injected advancing CPU reading instead of host load. `RpcProcessRunnerOptions::read_cpu_seconds` is the injection seam and `a_runaway_engine_is_stopped_by_the_startup_cpu_cap` advances the reading 0.1 s per sample, so the cap trips whatever share of a real CPU the test child got. |
| #13165 (`147e4d47d6`) | port | `crates/omo/senpi-task/src/runners/rpc_process.rs`: `StartupBounds` + `with_startup_progress` bound the wait for the engine's first answer (`get_protocol_info`) by its CPU progress (10 s stall window, 120 s CPU cap, 2 % progress epsilon, 250 ms sampling) with a 10 min outer bound that holds when the CPU cannot be read; `fallback_chain_deadline_ms` now covers only `set_retry_fallback`. Not ported from the same commit: moving the kernel reads out of `store/lock-owner.ts` — mhc's `store/record_lock.rs` never probes a pid (file age is the staleness authority, a documented divergence), so there is nothing to move. Tests: `runners/rpc/rpc_tests/rpc_process.rs` (4). |
| #14624 review M5, M4 (`d83b94a9f3`) | port | `socket_client.rs` `MeterState::look`: a probe proves recovery only when it is on time against the previous probe, so an overdue probe right after a request's clock read is not one (`an_overdue_probe_after_the_requests_clock_read_is_not_recovery_so_the_drop_is_redialed`, `an_overdue_probe_then_an_on_time_probe_before_the_reset_is_not_redialed`). The translated-address half is the thread component's caller-stall fixture, which has no Rust counterpart. |
| #14624 review M5 (`c460dd28af`) | port | `socket_client.rs` `Conversation::fail`: a host drop before any response byte is the stall's only if a stall began after the dial and the caller had written nothing or had not run on time again (`!(wrote && meter.recovered())`); the responsive-caller, first-on-time-probe and silent-connect cases in `tests/socket_client_edge_cases.rs`. No Rust caller yet. |
| #14624 review M5, L2 (`ce10661ecc`) | port | `socket_client.rs`: only a drop the meter saw as a stall is redialed, and a subscription's stall probe pauses on each reply and resumes with the next request (`Conversation::dispatch`, `Conversation::arm`); `tests/socket_client_edge_cases.rs` ports all 24 controlled-clock cases. No Rust caller yet. |
| #14624 review M1 (`01eaaf633b`) | port | `with_rpc_socket` redials only while `now < deadline + meter.total()`: `a_host_drop_handled_after_the_whole_budget_ran_is_not_redialed`. No Rust caller yet. |
| #14624 review M1, M2 (`445d0df079`) | port | `with_rpc_socket` keeps one deadline across attempts (each step is due at `min(deadline + stall, armed + step + stall - stall at arm)`) and `is_host_drop` counts only `ConnectionReset`/`BrokenPipe` (or a close) as the host's drop: the EACCES and ECONNRESET cases of `tests/socket_client_redial.rs`. No Rust caller yet. |
| `1f4085ab06` (no PR; #14444) | port | `socket_client.rs` `CallerStallMeter` (50 ms probe, 50 ms tolerance) moves every deadline by the caller's stall and a read dropped during one is dialed once more (`a_caller_stall_past_the_deadline_is_not_charged_and_the_late_answer_settles_the_read`, the redial cases); `RpcSocket` keeps its single wall-clock deadline for `authenticated_host_socket`. |
| #14498 (`c1b1ae31cd`) | port | `crates/maho-rpc/src/read_rpc_diagnostic.rs` + `socket_client.rs`: a connect without a secret reports `connected`, and only transport failures are traced, never `RpcSocketError::Operation` (`tests/read_rpc_diagnostic.rs`). No Rust caller yet. |
| `1c280530f6` (no PR; #14498 feature) | port | `read_rpc_diagnostic.rs`: with `OMO_THREAD_READ_TRACE=1` (read by `RpcSocketRuntime::system`) a failed read command prints one `[thread-read-rpc] command phase elapsed_ms code caller_stall_ms attempts` line and nothing private; 5 cases in `tests/read_rpc_diagnostic.rs`. No Rust caller yet. |
| `a74db808d7` (no PR) | n/a | TypeScript test fixture only (`store/__fixtures__/owned-task-state.ts` `ownedTaskStateConfig`), wiring a project-scoped `task.state_dir` into the paused-run DAG cases. Rust's `dag/e2e_tests.rs` already builds one `create_dag_file_store(&DagStoreConfig::new(&project))` per test over its own `tempfile::tempdir().keep()`, so fixture, engine and DAG runtime share one store without a config helper. |
| `1a0471480b` (no PR, #14607 review M1) | ported | Every test's task state stays out of the shared agent dir. mhc's `store/record_store.rs` has no default agent-dir fallback: `resolve_state_dir` (`store/types.rs`) is `task_state_dir` or `project_dir/.omo/senpi-task`, and every store in `store/store_tests.rs` and the manager fixtures is built over a `tempfile::tempdir()`, so the child-process scripts the upstream fix had to redirect write inside their own project too. |
| #11632 (`4f00b2a657`) | n/a | An in-memory `TaskRecordStore` double plus a contract suite run against both stores. mhc's store is a concrete struct (`store/record_store.rs` `TaskRecordStore`), not the TS `TaskRecordStore` interface, so there is no second implementation to hold to one contract; the double exists to remove the file store's 19 s lock/stat/rename cost from a Bun e2e case, and Rust's `dag/e2e_tests.rs` runs one manager per test over a tempdir instead. Crash durability is owned by `store/store_tests.rs`. |
| `3d4af11b30` (no PR; thread tools half) | n/a | `components/thread/tools/model-match.ts` (thinking suffixes, absent-provider naming) and `control/operations.ts` belong to the thread tracks (`thread-core`/`thread-control`); this track owns only the `senpi/` half, ported in the row below. |
| `3d4af11b30` (no PR; `senpi/` half) | port | `crates/omo/senpi-task/src/senpi/model_decorators.rs` (new) carries `SENPI_THINKING_LEVELS`, `SENPI_SERVICE_TIERS` and `split_model_decorators(reference, thinking_only)` with upstream's right-to-left, at-most-two grammar, so a genuine colon id (`provider/model:exacto`) stays glued instead of being read as a thinking level. `crates/omo/components/maho-omo-task/src/planner_explicit_pin.rs` `split_model_decorators` now delegates to it and keeps its `PinDecorators` type and its service-tier refusal. Tests: 6 in `senpi/model_decorators.rs`, 1 regression in `planner_explicit_pin.rs`. |
| `ab77e30cfe` (no PR) | n/a | Clears the leftover-socket readiness timer in `busy-host-probe.test.ts` once the listener is ready; a Bun test-harness detail with no Rust counterpart (there is no `busy-host-probe.test.ts` port). |
| `c18d8f169c` (no PR) | port | `crates/maho-rpc/src/host_probe.rs` `probe_socket` answers `SocketProbe::{Accepts, Refused, Unknown { reason }}` and `probe_socket_reachable` (`socketAcceptsConnection`) is `== Accepts`; three real-listener cases (live, a dead host's leftover entry and a missing path, mode 000). |
| `1f7a72d214` (no PR; #14437) | port | `crates/omo/components/maho-omo-remote/src/sync_status.rs` `read_engine_sync_status` (now async) reads the operator socket through `maho_rpc::host_probe::probe_socket` (2 s), mapping accepts/refused/unknown to running/no engine/unreadable with the probe's reason. |
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
- `crates/maho-rpc/tests/socket_client_redial.rs` (4), `tests/socket_client_edge_cases.rs` (the 24 controlled-clock
  cases plus one real Unix socket case) and `tests/read_rpc_diagnostic.rs` (5): `with_rpc_socket`, the caller-stall
  meter and the read trace on a manual clock (#14809, #14624, `1f4085ab06`, #14498, `1c280530f6`).
- `crates/maho-rpc/src/host_probe.rs`: `a_live_listener_accepts`,
  `a_socket_whose_host_died_and_a_missing_path_are_refused`,
  `a_socket_this_user_may_not_open_is_unknown_with_the_reason` (`c18d8f169c`).
- `crates/omo/components/maho-omo-remote/src/sync_status.rs`:
  `the_operator_socket_decides_no_engine_running_or_unreadable` now runs through the shared probe (`1f7a72d214`).

## Seen, not changed here

- `store/record_lock.rs` judges a lock's staleness by file age and never probes the holder's pid, so `lock-owner.ts`'s
  `readProcessStartIdentity` and the `platform/native-process.ts` extraction it drove have no Rust counterpart to
  move. That divergence predates this range (parity fragments `2.md`/`41.md`).
- `RpcProcessRunnerOptions` gained four `Option<...>` startup fields and one injection seam
  (`read_cpu_seconds`); every literal that does not name them keeps compiling through `Default`.
- `RpcSocket` (`host_probe::authenticated_host_socket` and its callers) keeps one wall-clock deadline per
  connection, while upstream charges every `withRpcSocket` call only the host's time. Moving those callers onto
  `with_rpc_socket`, and giving the thread live surface a socket `RpcTransport` built on it, is follow-up work outside
  these rows.
