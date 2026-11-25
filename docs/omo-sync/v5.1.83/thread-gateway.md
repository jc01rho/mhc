# omo v5.1.80..v5.1.83 sync: thread-gateway

Range: oh-my-openagent `822bd923038..69a1863a457` (v5.1.79 to v5.1.83; v5.1.80 was never released).
Upstream paths: `packages/omo-senpi/src/components/thread/{gateway,contracts,consumer}`, 65 non-merge commits.
mhc counterpart: `crates/omo/components/maho-omo-gateway/src/thread/`.

## Summary

| verdict | rows |
|---|---:|
| port | 16 |
| ported | 4 |
| n/a | 6 |
| gap | 12 |
| total | 38 |

One row per upstream PR, or per commit group where a change carries no PR number; a PR whose slice is partly portable
is split into two rows. Every non-merge commit in the range is named in exactly one row.

## Store schema baseline

mhc's gateway store (`thread/schema.rs`) carries upstream's migration steps v1..v16, statement for statement, since
the gateway-schema track (PR #86): v11 `deliveries.wait_kind` (#13014), v12 the intake ledger, v13 `woken_sessions`,
v14 the session waits and intake marks (#13037), v15 the rebuild of `deliveries` whose `mode_effective` CHECK takes
`start` (#14333) and v16 `deliveries.collapse_key` (#14428). Upstream v5.1.79 was already at v14; this range adds v15
and v16.

`gateway.sqlite` lives under the agent directory, which mhc shares with omo (`~/.omo/agent`), and `user_version`
counts upstream's steps, so mhc's numbering is omo's. Each step commits the reader floor (#14382) of the version it
reaches: a store at v16 carries floor 14, which omo v5.1.79 and older do not read (they refuse it, as they refuse a
store omo v5.1.83 migrated). A drain reads an intake row (v12) under its ledger key with the sender's text as
written; what writes intake rows and reads the v13-v14 tables (the #13037 wake carrier, the intake consumer and their
store ops) is not ported, and the rows that need it stay gap here and in `thread-wake.md`.

The part of #14622 that protects a store mhc does run against is ported. Every op re-reads the schema inside the read
snapshot it works in, and every write lock re-checks it before anything is written. A store that another binary
migrated past mhc's v10 while mhc had it open is refused (`gateway_schema_too_new`) instead of being read and
written with an older schema.

## Per PR

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #14622 review M4, M5, M7 (27731e71d4, 3d29c8e416) | port | Every store op runs inside the read snapshot its schema check opens and every write lock re-checks the schema before anything is written: `store_ops.rs` `in_checked_snapshot`, `end_read_snapshot`, `begin_immediate` (locked check), `SqlAccess::run`; `store.rs` `SqliteGatewayStore::checked` wraps every connection op. A store another binary migrated past this one since it opened answers `gateway_schema_too_new` and is never written. The reader-floor half of these commits is ported under #14382. Tests: `store_ops.rs` `a_write_lock_on_a_store_another_binary_migrated_is_refused_and_released`, `an_op_reads_inside_the_snapshot_its_schema_check_opened`; `tests/gateway_store_tests.rs` `a_store_another_binary_migrated_since_it_opened_refuses_every_op_and_writes_nothing`. |
| #14622 review M9, round 3 (98657b14a4, 30b3a2efd6, 5546223cc6, f7bca224a0) | port | The source-build guard is ported: `maho-omo-agent-home` `is_dev_build_runtime`, `is_shared_agent_dir`, `is_shared_store_file` and the account-home detection (`account_home.rs`, `dev_build_runtime.rs`), wired into `store_ops.rs` `migrate_guarded` (refuses before any pending step when the agent dir, its `gateway/`, its `gateway.sqlite`, or the file `SQLite` actually opened is one a released session shares). mhc's migrations stop at v16, the schema omo v5.1.83 released, so the guard keeps an omob source build from moving a shared store past a released schema; the floor edge cases that follow #14382 (a malformed floor is no floor) are ported with that row. `f7bca224a0` pins a TS worker bundle (n/a). |
| #14842 (713d705656) | n/a | TS test of the pinned v14 worker bundle (`testing/v14`); no Rust counterpart. |
| #14831 (94c544a861) | n/a | TS source-build guard test opening a refused WAL store on macOS; follows the #14622 M9 gap. |
| #14382 (5036654c32, 7e666c8c1e, 50c3da8323, 63133463e9, 9d13dad6e2, 84a952da4a) | port | `crates/omo/components/maho-omo-gateway/src/thread/schema.rs` `GATEWAY_READER_FLOORS`, `gateway_reader_floor`, `GATEWAY_READER_FLOOR_KEY`; `store_ops.rs` `stored_reader_floor` (fails closed on anything but a plain positive integer), `schema_version` (a newer store is usable when its floor is at most this binary's 16), `migrate` (each step writes its floor with its version bump), `record_reader_floor` (a store at this schema without its floor gets one). The source-build guard stays the #14622 M9 gap. Tests: `tests/gateway_store_tests.rs` (4 reader-floor cases), `schema.rs` `every_step_declares_its_reader_floor`. Ported with schema v11-v16 (gateway-schema track, PR #86). |
| #12314 (b92920254f, e923719ad8, 6e00d621cd, e9c1551587) | n/a | mhc never spawns `ps`: a claimant is judged from `/proc/<pid>` (`store_ops.rs` `claimant_is_dead`) and the own start time is read from `/proc/self/stat` (`self_identity_now`), so a refused spawn cannot happen. |
| #14555 (88b8a40602) | n/a | Bun test-harness statement finalization on Windows; mhc's tests seed through rusqlite connections that close when dropped. |
| #14535 (682d026123, 45942f1199) | ported | `crates/omo/components/maho-omo-gateway/src/thread/store.rs` `SqliteGatewayStore`: one connection behind a `Mutex`, shared through `Arc`; there is no worker to close under an in-flight call, and the connection closes only after the last call returned and the last `Arc` dropped. |
| 3d4af11b30 (no PR) | n/a | Only the `thread_set_model` parameter description changes in this track's paths; mhc's schema for it lives in `control_tools.rs` and the thinking-suffix and absent-provider behaviour in `model_match.rs` (thread-core's paths). |
| #14431 empty lane fits (f22daf7d4c) | port | `crates/omo/components/maho-omo-gateway/src/thread/backlog.rs` `backlog_refusal`: a lone delivery the engine accepted always fits an empty lane, whatever its size. Test: `a_lone_delivery_fits_an_empty_lane_whatever_its_size`. |
| #14431 collapsed answers and collapse receipts (f22daf7d4c, c3dcfcb915, 2c9ce28ff7) | port | `crates/omo/components/maho-omo-gateway/src/thread/engine.rs` answers an absorbed repeat `collapsed: true` (`GatewayDeliveryResult::Ok.collapsed`, `deduplicated`, the earlier `delivery_id`) and settles its receipt; `tools.rs` `thread_send` reports `collapsed`; `store_ops.rs` `collapse_earlier` keeps the queued row's marker until the superseding send commits and completes the superseded send's receipt with its refusal. Tests: `engine.rs` `a_repeat_the_target_already_took_answers_collapsed_with_the_earlier_delivery`, `a_send_a_newer_repeat_superseded_replays_its_refusal_on_retry`; `tests/gateway_store_tests.rs` (3). Ported with schema v16 (gateway-schema track, PR #86). |
| #14431 listing backlog_error (f22daf7d4c, 2c9ce28ff7) | gap | `backlog_error` on `thread_list` needs the listing producer (`tools/read-ops.ts` `withBacklog`, thread-core's path); the store side (`backlogs`) is ported under #14428. |
| #14466 (acafacd8b9, 4a60b503bd, 145fe6c74e) | port | `crates/omo/components/maho-omo-gateway/src/thread/store_ops.rs` `refuse_expired_queued` (an enqueue refuses the target's queued rows past their lifetime before counting, and unlinks their markers only after its commit), `expired_dead_claims`; `backlog.rs` (`frees_at`, `target_running`, dead claims leave the count); `result.rs` `engine_failure` (next action for a target nobody runs). Not ported: the retention sweep's global refusal (mhc has no retention sweep). The intake-row exemption (`ledger_id IS NULL`) came with schema v12 (gateway-schema track, PR #86; test `an_expired_row_taken_in_from_another_device_is_left_to_its_target`). Tests: `backlog.rs` (3), `tests/gateway_store_tests.rs` `a_target_nobody_runs_frees_a_backlog_slot_as_its_oldest_queued_row_expires`. |
| #14428 steer reserve and backlog facts (7baf0c1da4, 3b8289574a, f3dbf5e785, 3df4b16a50, 22aec0f4b3) | port | `crates/omo/components/maho-omo-gateway/src/thread/constants.rs` `STEER_RESERVE_MESSAGES`/`STEER_RESERVE_BYTES`; `backlog.rs` (per-lane limits, `session_not_writing`/`session_not_draining` facts, `read_target_backlogs`); the `backlogs` store op (`store_api.rs`, `store.rs`, `store_worker.rs`); `result.rs` queue_full next action. Tests: `backlog.rs` (4). The `thread_list` row `backlog` field needs its producer in thread-core's listing (`ThreadResultSummary` is built literally in `thread_row.rs`). |
| #14428 collapse key (7baf0c1da4) | port | `crates/omo/components/maho-omo-gateway/src/thread/schema.rs` v16 `deliveries.collapse_key`; `store_ops.rs` `collapse_earlier` (the sender's queued row under the key is refused `superseded`, a row the session already took absorbs the repeat: `EnqueueOutcome::Collapsed`); `types.rs` `RefusalReason::Superseded`, `EnqueueRequest.collapse_key`; `engine.rs` `GatewayDeliverRequest.collapse_key` (in the receipt's args hash); `tools.rs` `thread_send.collapse_key`; `result.rs` `superseded` reads `not_resumable`, queue_full next action. Not carried: the SDK `send` (thread-core) and `omo thread send --collapse-key` (omo-native). Ported with schema v16 (gateway-schema track, PR #86). |
| #14278 (67a4f3a199, 039e8efc29) | gap | `engine_version` and the provider retry wait (`waiting_on`) on `thread_list` rows need the listing producer (thread-core) and senpi#7025's published retry wait; `ThreadResultSummary` is built literally in `thread_row.rs`. |
| #14363 create half (b41242db44, 0fcdd7cd47) | port | A create answers a first prompt the thread admitted to its queue as `queued` (with what it waits on) and a prompt stored for a woken session as `queued` waiting on the wake: `crates/omo/components/maho-omo-gateway/src/thread/control/create.rs` `prompt_that_ran`, `result.rs` `admitted_wait` (the hold or approval a queued row waits on, `DeliveryWait`), `types.rs` / `contracts_results.rs` (`waiting_on` on a queued delivery). Tests: `tests/thread_create_tests.rs` `a_prompt_stored_for_a_wake_is_queued_with_a_next_action_not_to_send_it_again`, `a_prompt_not_yet_admitted_waits_for_its_start_and_answers_what_it_became`; `control/prompt_start.rs` (5). |
| #14363 wake memory floor (b41242db44, 0fcdd7cd47) | gap | The wake floor (a quarter of a limit below 4 GiB, at least one wake's measured cost) belongs to the #13037 wake carrier, which is not ported (`thread-wake.md`). |
| #14305 (4960cedb07) | gap | `thread_list` row identity, `unnamed` names: listing producer (thread-core) and thread-address-core `rowName`. |
| #14355, #14339 (640d1bd2ce) | n/a | Formatting of TS merge resolutions. |
| #14335 (929de417e1) | port | `thread_create` now sends its `prompt` to a thread it opens live already: `crates/omo/components/maho-omo-gateway/src/thread/control/create.rs` (`thread-control.md` #14335). |
| #14333 start lane (d82e7a2436) | port | `crates/omo/components/maho-omo-gateway/src/thread/types.rs` `EffectiveLane::Start`; `decision.rs` `effective_lane_of` (an idle session's followUp admission is `start`); `drain.rs` claims with it, so `effective_mode` and the header's `effective=` say `start`; `schema.rs` v15 rebuilds `deliveries` so `mode_effective` takes it. Tests: `decision.rs` `an_admitted_delivery_names_the_lane_it_took_and_an_idle_start_is_start`, `result.rs` `a_turn_begun_on_an_idle_session_reports_the_start_lane`, `provenance.rs` `the_header_names_the_lane_the_delivery_took_and_an_idle_start_is_start`, `tests/gateway_schema_tests.rs` (claim with `start` after the v15 rebuild). Ported with schema v15 (gateway-schema track, PR #86). |
| #14333 relay lane (5825d630a4, 5c80bd4657) | ported | The relay leg reports the lane the receiver answered: `thread/remote/{device_service,outcome}.rs` (`thread-remote.md` #14333). |
| #14333 default delivery auto (484cafd3b2) | ported | `crates/omo/components/maho-omo-gateway/src/thread/engine.rs` `deliver_inner` defaults an omitted mode to `GatewayDeliveryMode::Auto`; the thread_send schema in `tools.rs` offers auto, steer and follow_up. |
| 9b6ee2e310 (no PR) | ported | An unknown field is refused, not dropped: every thread tool schema declares `additionalProperties: false` (`tools.rs`, `control_tools.rs` `object`) and the harness validates arguments before execute. `thread_create` carries the did-you-mean table (`message` -> `prompt`, `contracts_create_params.rs`) and answers `prompt: null` when it was given none; the other tools keep the schema-level refusal without the table. |
| 07bec8317f (no PR) | gap | Stored session waits and holds: the v14 `session_wait` table exists (PR #86); the carrier's `waitBlocksDelivery` and the session-wait store op it reads (#13037 wake carrier) are not ported. |
| pair loop guard (3707d9dcef, 308a8cf429, f70d96145e) | port | `crates/omo/components/maho-omo-gateway/src/thread/store_ops.rs` `take_pair_token` answers `pair_loop_guard` with `guard`, `burst`, `window_ms`, `retry_after_ms`; `errors.rs` code, `pair_loop_guard_message`, `pair_loop_guard_next_action`; `result.rs` `engine_failure`. Not ported: the intake consumer and relay transport legs (no intake consumer in mhc). Tests: `engine.rs` `a_ninth_burst_send_to_one_session_is_refused_pair_loop_guard_with_the_measured_burst`, `result.rs` `a_refusal_that_names_its_recovery_gets_the_next_action_reading_it`. |
| 01fa0b0220 (no PR) | gap | The v13 `woken_sessions` table exists (PR #86); the woken-session store ops and the carrier that replace the count cap with measured memory (#13037) are not ported. |
| no per-turn cap (48dd6841e2, ce613b134e) | port | `crates/omo/components/maho-omo-gateway/src/thread/store_ops.rs` `check_fanout` and `constants.rs` `MAX_FANOUT_PER_TURN` removed. Test: `engine.rs` `one_sender_turn_reaches_forty_sessions`. |
| senpi#7014 held admission (cdb5488d0f, f015ad1077, 6272d2b84b, 661f6142a9, 6912c97fad) | gap | mhc's session runtime never answers `held` (`maho-core/src/external_admission.rs` has no such admission; senpi#7014 is not in its pin), and the answer's `waiting_on` needs the #13014 delivery wait, whose v11 column exists (PR #86) but which mhc's drain does not write yet. |
| #14324 (ae55079bdb) | gap | Graceful close of another device's thread: `thread_close { graceful, wait_ms }` lives in the control and remote paths; `REMOTE_GRACEFUL_MAX_WAIT_MS` has no consumer here. |
| #14313 (336f5fa4b9) | port | `crates/omo/components/maho-omo-gateway/src/thread/store_ops.rs` `refuse_returned` (+ `store_api.rs`, `store.rs`, `store_worker.rs`); `graceful_close_returns.rs` `returned_at_close`, `settle_graceful_close`; `types.rs` `RefusalReason::TargetClosed`. Not wired: the `session_shutdown { closed }` hook (thread component) and `thread_close { graceful }` (control) need senpi#7046's `session-closed` record, which maho-core does not write yet; the closed-intake settle needs an intake ledger. Tests: `graceful_close_returns.rs` (2). |
| #14304 (93d376f3c4) | gap | `thread_list` notes for archived listings and incomplete devices: listing producer (thread-core) and remote devices (thread-remote). |
| #14308 (c6cc5178d8) | gap | `thread_list` `listing: incomplete` when an engine of this device did not answer: listing producer (thread-core). |
| #14295 (1eb9f9b48d) | gap | `thread_list` partial devices and `inactive_dropped`: listing producer (thread-core) and remote devices (thread-remote). |
| #14273 target_closed answer (030e18d0a8) | port | `crates/omo/components/maho-omo-gateway/src/thread/result.rs` `refusal_mapping` answers `target_closed` as `not_resumable` with a next action; the eval-form `thread_create` hint is carried now that mhc has `thread_create`. Test: `a_row_its_closed_session_handed_back_reads_not_delivered`. |
| #14273 closed intake settle (a397780f93) | gap | The intake ledger columns exist (v12, PR #86); settling queued intake on a close needs the intake consumer and the wake carrier (#13037, thread-wake). |
| b4f841607a (no PR) | port | `crates/omo/components/maho-omo-gateway/src/thread/relay.rs` inbound `binding_inactive` and `unsupported` refusals name the connector's next step. |

## Tests

New cases, all under `crates/omo/components/maho-omo-gateway`:

- `src/thread/store_ops.rs`: `a_write_lock_on_a_store_another_binary_migrated_is_refused_and_released`,
  `an_op_reads_inside_the_snapshot_its_schema_check_opened` (#14622).
- `src/thread/backlog.rs`: seven cases for the per-lane cap, the steer reserve, the stall facts, expiry, dead claims
  and `read_target_backlogs` (#14428, #14466, #14431).
- `src/thread/engine.rs`: `one_sender_turn_reaches_forty_sessions`,
  `a_ninth_burst_send_to_one_session_is_refused_pair_loop_guard_with_the_measured_burst`.
- `src/thread/result.rs`: `a_row_its_closed_session_handed_back_reads_not_delivered`,
  `a_refusal_that_names_its_recovery_gets_the_next_action_reading_it`.
- `src/thread/graceful_close_returns.rs`: `the_newest_clean_stop_record_names_the_returned_deliveries`,
  `a_graceful_close_refuses_only_the_deliveries_its_session_handed_back` (#14313).
- `tests/gateway_store_tests.rs` (registered in `tests/main.rs`): the store through its public API for #14622 and
  #14466.

Added by the gateway-schema track (PR #86):

- `tests/gateway_schema_tests.rs` (registered in `tests/main.rs`): a v10 store migrates to v16 keeping every delivery
  row and rowid, the column order, indexes, the provenance trigger, cascade-referencing extension rows and every
  thread-state table, with the v12-v14 registry entries and reader floor 14; a v14 store with every column set
  survives the v15 rebuild; an orphaned extension reference keeps the store at v14 untouched
  (`gateway_migration_failed`).
- `tests/gateway_store_tests.rs`: reader floor (recorded, met, above, missing, malformed, no `gateway_meta`, recorded
  on a store at this schema), collapse key (supersede and replay, absorb, rollback keeps the wake), the intake-row
  exemption, and an intake row settled and continued by its ledger key.
- `src/thread/schema.rs` floors, `decision.rs` lanes, `result.rs` start and superseded, `provenance.rs` header,
  `drain.rs` admission under the ledger key, `engine.rs` collapsed and superseded replays.

## Seen, not changed here

- `store_ops.rs` `claimant_is_dead` judges a claimant only from `/proc/<pid>`. On macOS and Windows every claimant
  therefore reads as dead, so `reconcile` can take back a row a live runtime still holds. This predates this range.
