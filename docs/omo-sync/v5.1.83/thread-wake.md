# omo v5.1.80..v5.1.83 sync: thread-wake

Range: oh-my-openagent `822bd923038..69a1863a457` (v5.1.79 to v5.1.83; v5.1.80 was never released).
Upstream paths: `packages/omo-senpi/src/components/thread/{wake,run-alerts}`, 22 non-merge commits.
mhc counterpart: `crates/omo/components/maho-omo-gateway/src/thread/{wake,run_alerts}/`, with the store half of the wake in
`thread/{woken_types,session_wait_types,store_woken_ops,store_woken_facade,intake_admission}.rs` and the consumer's port and
contract in `thread/consumer/`.

## Summary

Counts computed programmatically from the per-PR rows below, not by hand.

| verdict | rows |
|---|---:|
| port | 16 |
| ported | 0 |
| n/a | 1 |
| gap | 0 |
| total | 17 |

One row per upstream PR, or per commit group where a change carries no PR number; a PR or commit group whose slice is
partly portable is split into two rows. Every non-merge commit in the range is named in at least one row.

## Wake subsystem baseline

The #13037 wake runtime (present upstream before v5.1.79) is ported here onto the store tables the gateway-schema track
added (PR #86: v13 `woken_sessions`, v14 `session_wait`, `wake_counts`, `intake_marks`, `intake_commands`):

- store: every `woken_*` op of `gateway/store-worker.ts` (request, start, renew, take over, set state, waiting, retry, end,
  gated, list, wake counts, session work, session waits and `waiting_on`, intake marks, open intake sessions) in
  `store_woken_ops.rs`, behind `SqliteGatewayStore::woken()` / `session_wait()` / `intake_marks()`
  (`gateway/store-woken-facade.ts`), `woken-types.ts`, `session-wait-types.ts`, the wake marker and `WAKE_LEASE_MS`;
- wake: `carrier.ts` (with `engine-state.ts`), `session-waker.ts`, `pending-sweep.ts`, `consumer-start.ts`,
  `runtime.ts`, `wake-runtime.ts`, the open frame and its reply classifier of `legacy-open.ts`, the connection contract of
  `carrier-connection.ts`;
- consumer: `consumer/port.ts` and the consumer's contract (`consumer/consumer.ts` `DeliveryConsumer`), which the runtime
  starts through a factory.

Every timer and event continuation goes through the carrier's `Schedule` seam, so the tests drive the clock (the store's
clock) and the timers by hand; no test sleeps.

Not ported, each for a named reason:

- the carrier's socket connector (`carrier-connection.ts` `connectCarrierHost`) and the legacy open's request
  (`legacy-open.ts` `createLegacyWakeOpen`): they need senpi-task's authenticated `rpc-socket-client`
  (`resolveRpcSocketTransport`, `authenticateRpcHost`) and `live-surface-paths.ts` `resolveThreadSocket`, which have no
  Rust counterpart; the runtime and the waker take them as seams;
- `host-ensure.ts`: the carrier's `host ensure --policy never` needs the engine launch and the thread socket above;
- the delivery consumer itself (`consumer/consumer.ts` `createDeliveryConsumer`) and its local-API port
  (`consumer/device-service-port.ts`): the consumer commits a handed record through the store's intake write
  (`store-worker.ts` `intake`, `store-ops.ts` `IntakeFields`, `inbound_hop`), which this crate does not have, and the
  device service here serves no `deliveries.*` protocol;
- `NativeThreadComponent` in `native.rs` does not start the wake runtime yet (no carrier socket connector); the
  standalone `same_device_wake.rs`, `local_transport.rs`, `wake/runtime.rs::process_same_device_wake`,
  `wake/wake_runtime.rs::create_same_device_wake`, and `drain.rs` `wake_gated` / `admit_intake` seams are ported.

## Per PR

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #14335 (`d005504cdf`, `6bc00f474f`) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/runtime.rs` `RuntimeInner::admit_intake`: a consumer the service acknowledged answers a woken drain's admission on its live port at once, before its startup pending sweep ends (waiting for the start would wait on itself). Test `tests/wake_runtime_tests.rs` `given_a_parked_session_the_startup_sweep_wakes_when_its_drain_asks_the_admission_then_the_serving_consumer_answers_at_once` (the engine host's `wake` runs the drain's ask inside the startup sweep). `component-wiring.test.ts` is not ported: the native thread component does not start the wake (no carrier connector, see baseline). |
| #14430 (`c02d7200fa`, `293610e2a2`, `0098c7729e`, `4c123c6ec1`) | port | `crates/omo/components/maho-omo-gateway/src/thread/run_alerts/` (`error_streak.rs`, `run_alerts.rs`), `tools.rs` (`create_thread_send_as`), `component.rs` / `native.rs` lifecycle hook wiring (`AgentEnd`, `AgentSettled`, `SessionShutdown`), and `tests/run_alerts_tests.rs` (13 unit tests + 5 component integration tests). Schema registration in `omo-config-core` (`operator_alerts.rs`) landed in `misc-core` (#75). |
| #14383 closed-intake race drop (`aff3383d15`) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/closed_intake.rs` `settle_closed_intake` drops rows whose `refuse_queued` lost the race to another claimant instead of returning them as remaining queued rows. Test: `given_a_row_already_claimed_before_refusal_when_settled_then_it_is_dropped_from_remaining`. |
| #14383 runtime nothing_queued drained marks (`aff3383d15`) | port | `wake/runtime.rs` `RuntimeInner::wake` reports the drained marks when a wake answers `nothing_queued` (the rows a close settled during that wake), and `wake/consumer_start.rs` reports them after the startup sweep. Tests: `tests/wake_runtime_tests.rs` `given_a_delivery_from_another_device_queued_before_a_close_on_request_when_the_session_is_woken_then_it_answers_nothing_queued_and_its_drained_mark_is_reported`, `tests/wake_carrier_tests.rs` `given_rows_queued_for_a_session_that_was_then_closed_on_request_...`. |
| #14363 memory floor (`f1ec5e79ae`, `0fcdd7cd47`, `abd077fd6a`, `b41242db44`) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/memory_headroom.rs` `WAKE_SESSION_COST_BYTES` (409 MiB) and `read_wake_memory_floor` (1/4 of cgroup v2 limit / `MemTotal`, capped at 1 GiB, floored at 409 MiB) with 6 unit tests. |
| #14363 carrier floor (`f1ec5e79ae`, `0fcdd7cd47`, `abd077fd6a`, `b41242db44`) | port | `wake/carrier.rs` reads `memory_floor` (production: `read_default_wake_memory_floor`, `WAKE_MEMORY_FLOOR_BYTES` when absent) before a start and defers with `host_memory_pressure { available_bytes, floor_bytes }`. Tests: `given_a_768_mib_cgroup_whose_floor_is_one_wakes_cost_and_500_mib_of_room_...`, `given_room_below_that_floor_...`. |
| #14363 create answer (`0fcdd7cd47`, `b41242db44`) | port | `control/create.ts` answering a first prompt stored for a woken session as `queued` waiting on the wake: ported by the thread-create track. `thread/control/create.rs` `prompt_that_ran` answers `queued` with `waiting_on`, which `result.rs` `admitted_wait` reads off the stored row (`wait_kind`, or `hold` for a held admission). Test: `tests/thread_create_tests.rs` `a_prompt_stored_for_a_wake_is_queued_with_a_next_action_not_to_send_it_again` (a faux sender answers the stored row; the wake carrier itself is exercised by `wake_*_tests.rs`). `thread-control.md` #14363. |
| #14355, #14339 (`640d1bd2ce`) | n/a | Formatting of TS merge resolutions in `wake/parked-resume.test.ts`. |
| `07bec8317f`, `b7a993b53e` (no PR) | port | `wake/carrier.rs` blocks a wake on a stored hold only when `session_wait_types.rs` `wait_blocks_delivery` says so (a held state made only of `unattended` holds blocks nothing), and `store_woken_ops.rs` `waiting_on` reads the same predicate. Tests: `tests/wake_carrier_tests.rs` `given_a_session_the_carrier_handed_back_held_unattended_when_a_later_wake_comes_then_the_stored_hold_does_not_block_it`, `tests/wake_store_tests.rs` `given_stored_waits_...`. |
| `01fa0b0220`, `a7ad9d1639` memory headroom probe (no PR) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/memory_headroom.rs` `WAKE_MEMORY_FLOOR_BYTES` (1 GiB) and `read_host_memory_headroom` (min of Linux cgroup v2 room and `/proc/meminfo` `MemAvailable`) with 5 unit tests. |
| `01fa0b0220`, `a7ad9d1639` carrier & session-waker (no PR) | port | No woken-session count cap: `wake/carrier.rs` defers a start only on measured headroom (`host_memory_pressure`, the row stays requested and the sweep retries it) and `store_woken_ops.rs` `start_wake` takes a row out of `requested` with no count; `wake/session_waker.rs` opens past any number of woken rows. Tests: `given_nine_sessions_a_host_restart_parked_...`, `given_host_memory_headroom_below_the_floor_...`, `given_four_held_sessions_woken_in_turn_...`, `tests/wake_session_waker_tests.rs` `given_four_woken_sessions_past_requested_...`. |
| `8dfb297ba1`, `9d4469b155` CalVer prefix engine gate (no PR) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/engine_minimum.rs` `MIN_WAKE_ENGINE_VERSION` (`2026.11.15-2`) and `engine_drops_retained_record` reading dev-engine CalVer prefix (`YYYY.MM.DD-N[-omob...]`) with 3 unit tests (22 version cases). |
| `d54aa61b60` parked resume, wake side (no PR) | port | Under the dev engine stamp `2026.11.19-2-1-omob...` the runtime starts at the shipped minimum, the carrier opens the parked session with `retain_on_disconnect` and sends the engine's `wake`, hands it back, and wakes it again: `tests/wake_runtime_tests.rs` `given_the_dev_engine_stamp_and_a_parked_session_no_endpoint_lists_...`. |
| `d54aa61b60` parked resume, send and drain side (no PR) | port | `same_device_wake.rs` (`SameDeviceWake`), `local_transport.rs` (`LocalTransport`), `types.rs` (`GatewayDeliveryDelivery::QueuedOffline.reason`), `intake_admission.rs` (`IntakeAdmissionPass`, `IntakeVerdict`), `drain.rs` (`InboxDrainOptions::admit_intake`, `wake_gated`, `session_control_result`), `wake/runtime.rs` `process_same_device_wake`, and `wake/wake_runtime.rs` `create_same_device_wake`. Tests: `tests/same_device_wake_tests.rs` (switch cases, `drain_intake_admission_*`, `given_a_parked_session_when_local_send_wakes_it_then_it_answers_queued_waiting_on_wake_drains_as_started_and_resumes_again`, `given_a_session_handed_back_held_unattended_when_a_later_send_arrives_then_it_wakes_and_starts_again`). |
| #14313, #14324 session-target close instant (`336f5fa4b9`, `ae55079bdb`) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/session_target.rs` `close_instant_of`, `closed_at_of`, and `resolve_wake_target` reading `SESSION_CLOSED_ENTRY` (`session-closed`) and `rpc_effect` (`unload`/`done`) timestamps from the session transcript tail, with unit tests. |
| #14273 closed-intake & session-target (`a397780f93`) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/closed_intake.rs` `settle_closed_intake` (refusing rows queued at or before `closed_at` as `target_closed`) and `crates/omo/components/maho-omo-gateway/src/thread/wake/session_target.rs` `resolve_wake_target` (`closed_at` from `rpc_effect` unload `done`) with unit tests. |
| #14273 carrier, consumer-start & pending-sweep (`a397780f93`) | port | `wake/carrier.rs` `open_allowance` settles rows queued before a close on request first and opens nothing for them (`nothing_queued`); `wake/pending_sweep.rs` settles them before it wakes; `wake/consumer_start.rs` passes each session's `closed_at` to the sweep and reports drained marks after it; `testing/fake-wake-host.ts` is `tests/wake_support.rs`. Tests: `tests/wake_pending_sweep_tests.rs` (3 close cases), `tests/wake_carrier_tests.rs`, `tests/wake_runtime_tests.rs`. |

## Tests

New cases, all under `crates/omo/components/maho-omo-gateway`:

- `tests/wake_carrier_tests.rs` (42, `carrier.test.ts`): lift, wake and quiet release; closed-intake settlement; streaming,
  background and stalled; the queued-delivery grace and its restart; person attached; unknown release; lifecycle event
  through the schedule; the 30-minute person wait and its reset; the held rule (stop, grown file, unattended hold, interrupt
  release, host stamp); lease takeover, successor sweep and start sweep, gone, ambiguous, unknown, child and worker at the
  takeover; another carrier's live record; sender admission at the retry and the takeover; the A7.2.3 switches; the wake
  limit and the input revision; measured memory and the floor; the renewal timer.
- `tests/wake_session_waker_tests.rs` (10, `session-waker.test.ts`), `tests/wake_pending_sweep_tests.rs` (6,
  `pending-sweep.test.ts`), `tests/wake_store_tests.rs` (7, the woken ops and `store-session-wait.test.ts`).
- `tests/wake_runtime_tests.rs` (14, `consumer-start.test.ts` and `runtime-start.test.ts`): refused and unsupported
  declarations, a connection ending during the sweep, serving at the wake, a failing sweep, per-project same_device; the
  start gates and one wake per agent dir, no timer on an empty table, one warning per change of failure, a stop against an
  in-flight retry, #14383, #14335, the dev engine stamp, and the drained report at a carry's end.
- `tests/same_device_wake_tests.rs` (8, `same-device-wake.test.ts`, `drain-intake-admission.test.ts`,
  `parked-resume.test.ts`): same-device switch, local transport validation, `waiting_on` order, per-device intake
  admission, `wake_gated` lease expiry, and end-to-end parked resume + unattended-hold resume with manual schedule.
- unit tests: `wake/engine_state.rs` (4), `wake/legacy_open.rs` (3), `consumer/port.rs` (2).

Not ported from the upstream tests: the store-wrapper race and store-read-failure cases of `carrier.test.ts` (no wrapper
seam over the concrete store; the race is covered by `closed_intake.rs`), the concurrent single flight of
`session-waker.test.ts`, the async-scope, idle-exit, chain-limit and production-port cases of `runtime-start.test.ts`,
`component-wiring.test.ts` and `host-ensure.test.ts` (see the baseline).
