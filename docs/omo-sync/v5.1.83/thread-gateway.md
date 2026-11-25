# omo v5.1.80..v5.1.83 sync: thread-gateway

Range: oh-my-openagent `822bd923038..69a1863a457` (v5.1.79 to v5.1.83; v5.1.80 was never released).
Upstream paths: `packages/omo-senpi/src/components/thread/{gateway,contracts,consumer}`, 65 non-merge commits.
mhc counterpart: `crates/omo/components/maho-omo-gateway/src/thread/`.

## Summary

| verdict | rows |
|---|---:|
| port | 11 |
| ported | 3 |
| n/a | 6 |
| gap | 15 |
| total | 35 |

One row per upstream PR, or per commit group where a change carries no PR number; a PR whose slice is partly portable
is split into two rows. Every non-merge commit in the range is named in exactly one row.

## Store schema baseline

mhc's gateway store (`thread/schema.rs`) carries upstream's migration steps v1..v16 unchanged: same numbering, same
SQL, same order. Upstream v5.1.79 was already at v14: v11 `deliveries.wait_kind` (#13014), v12 the intake ledger,
v13 `woken_sessions` and v14 the session waits and intake marks (#13037). This range adds v15 (a rebuild of
`deliveries` whose `mode_effective` CHECK takes `start`, #14333) and v16 (`deliveries.collapse_key`, #14428).

`gateway.sqlite` lives under the agent directory, which mhc shares with omo (`~/.omo/agent`), and `user_version`
counts upstream's steps, so the numbering has to match one for one: before this pot mhc stopped at v10, and an omo
binary would have read mhc's own v11 as its own v11. Both rebuild steps (`bindings` at v7, `deliveries` at v15) run
with `foreign_keys` OFF and refuse the bump when `pragma_foreign_key_check` names the rebuilt table.

Reader floor (#14382): every step writes `gateway_meta.reader_floor` inside the transaction that bumps the version,
so no reader ever sees a raised schema with a stale floor, and a binary opening a store at its own schema that lacks
the key records its floor once. `schema_version` admits a store newer than this binary only when the stored floor is
a plain positive integer this binary meets; an empty, padded, exponent, fractional, zero or negative value, or a
store without `gateway_meta`, fails closed with `gateway_schema_too_new`. `GATEWAY_READER_FLOORS` mirrors upstream
(`.., 13, 14, 14, 14`): v15's `start` lane and v16's nullable `collapse_key` are both values a v14 reader passes
through as data.

Still not ported on top of this schema: no mhc code writes the v12..v14 tables yet (the woken-session registry, the
intake ledger and the stored session waits are behaviour, not storage), and `collapse_key` is not offered on
`thread_send`. The rows below that need those behaviours stay gaps.

Storing `start` is now possible (`EffectiveLane::Start`, `store_ops.rs` `row_from_record` and `insert_delivery`), and
the v15 rebuild keeps every row, rowid, index and the v8 provenance trigger, which the migration tests pin.

The part of #14622 that protects a store mhc does run against is ported. Every op re-reads the schema inside the read
snapshot it works in, and every write lock re-checks it before anything is written. A store that another binary
migrated past mhc while mhc had it open is refused (`gateway_schema_too_new`) instead of being read and
written with an older schema.

## Per PR

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #14622 review M4, M5, M7 (27731e71d4, 3d29c8e416) | port | Every store op runs inside the read snapshot its schema check opens and every write lock re-checks the schema before anything is written: `store_ops.rs` `in_checked_snapshot`, `end_read_snapshot`, `begin_immediate` (locked check), `SqlAccess::run`; `store.rs` `SqliteGatewayStore::checked` wraps every connection op. A store another binary migrated past this one since it opened answers `gateway_schema_too_new` and is never written. The reader-floor half of these commits is the #14382 gap. Tests: `store_ops.rs` `a_write_lock_on_a_store_another_binary_migrated_is_refused_and_released`, `an_op_reads_inside_the_snapshot_its_schema_check_opened`; `tests/gateway_store_tests.rs` `a_store_another_binary_migrated_since_it_opened_refuses_every_op_and_writes_nothing`. |
| #14622 review M9, round 3 (98657b14a4, 30b3a2efd6, 5546223cc6, f7bca224a0) | gap | The source-build guard keys on the agent-home dev-build runtime (`isDevBuildRuntime`, `isSharedAgentDir`, `isSharedStoreFile`, account-home detection in `components/agent-home`, misc-core's path). The store now migrates to v16, so a source build can move a shared store past a released schema, which makes this guard more urgent — it is still not here. The floor edge cases follow #14382 (ported); f7bca224a0 pins a TS worker bundle. |
| #14842 (713d705656) | n/a | TS test of the pinned v14 worker bundle (`testing/v14`); no Rust counterpart. |
| #14831 (94c544a861) | n/a | TS source-build guard test opening a refused WAL store on macOS; follows the #14622 M9 gap. |
| #14382 (5036654c32, 7e666c8c1e, 50c3da8323, 63133463e9, 9d13dad6e2, 84a952da4a) | port | `crates/omo/components/maho-omo-gateway/src/thread/schema.rs` `GATEWAY_READER_FLOORS` / `gateway_reader_floor` / `GATEWAY_READER_FLOOR_KEY`; `store_ops.rs` `stored_reader_floor`, `write_reader_floor`, `record_reader_floor`, and `schema_version` admitting a newer store only when its floor is a plain positive integer this binary meets. Each step's floor commits with its version bump, and a binary opening a store at its own schema that lacks the key records it once. Tests: `store_ops.rs` `every_step_has_a_reader_floor_and_the_v15_and_v16_rebuilds_keep_fourteen`, `reader_floor_admits_a_compatible_future_store_and_rejects_malformed_or_higher_floors`; `tests/gateway_store_tests.rs` `a_newer_store_whose_reader_floor_this_binary_meets_keeps_reading_and_writing`, `a_store_another_binary_migrated_since_it_opened_refuses_every_op_and_writes_nothing`. Not ported: the mac/Windows `f7bca224a0` bundle pin (n/a) and the commit-observing harness, replaced by reading the floor back after each migration. |
| #12314 (b92920254f, e923719ad8, 6e00d621cd, e9c1551587) | n/a | mhc never spawns `ps`: a claimant is judged from `/proc/<pid>` (`store_ops.rs` `claimant_is_dead`) and the own start time is read from `/proc/self/stat` (`self_identity_now`), so a refused spawn cannot happen. |
| #14555 (88b8a40602) | n/a | Bun test-harness statement finalization on Windows; mhc's tests seed through rusqlite connections that close when dropped. |
| #14535 (682d026123, 45942f1199) | ported | `crates/omo/components/maho-omo-gateway/src/thread/store.rs` `SqliteGatewayStore`: one connection behind a `Mutex`, shared through `Arc`; there is no worker to close under an in-flight call, and the connection closes only after the last call returned and the last `Arc` dropped. |
| 3d4af11b30 (no PR) | n/a | Only the `thread_set_model` parameter description changes in this track's paths; mhc's schema for it lives in `control_tools.rs` and the thinking-suffix and absent-provider behaviour in `model_match.rs` (thread-core's paths). |
| #14431 empty lane fits (f22daf7d4c) | port | `crates/omo/components/maho-omo-gateway/src/thread/backlog.rs` `backlog_refusal`: a lone delivery the engine accepted always fits an empty lane, whatever its size. Test: `a_lone_delivery_fits_an_empty_lane_whatever_its_size`. |
| #14431 collapsed answers, listing backlog_error (f22daf7d4c, 2c9ce28ff7, c3dcfcb915) | gap | `collapsed` answers and the collapse key's receipts need schema v16 (`deliveries.collapse_key`, after v11-v15). `backlog_error` on `thread_list` needs the listing producer (`tools/read-ops.ts` `withBacklog`, thread-core's path); the store side (`backlogs`) is ported under #14428. |
| #14466 (acafacd8b9, 4a60b503bd, 145fe6c74e) | port | `crates/omo/components/maho-omo-gateway/src/thread/store_ops.rs` `refuse_expired_queued` (an enqueue refuses the target's queued rows past their lifetime before counting, and unlinks their markers only after its commit), `expired_dead_claims`; `backlog.rs` (`frees_at`, `target_running`, dead claims leave the count); `result.rs` `engine_failure` (next action for a target nobody runs). Not ported: the retention sweep's global refusal (mhc has no retention sweep) and the intake-row exemption (no intake ledger, so the `ledger_id IS NULL` filter has no column to read). Tests: `backlog.rs` (3), `tests/gateway_store_tests.rs` `a_target_nobody_runs_frees_a_backlog_slot_as_its_oldest_queued_row_expires`. |
| #14428 steer reserve and backlog facts (7baf0c1da4, 3b8289574a, f3dbf5e785, 3df4b16a50, 22aec0f4b3) | port | `crates/omo/components/maho-omo-gateway/src/thread/constants.rs` `STEER_RESERVE_MESSAGES`/`STEER_RESERVE_BYTES`; `backlog.rs` (per-lane limits, `session_not_writing`/`session_not_draining` facts, `read_target_backlogs`); the `backlogs` store op (`store_api.rs`, `store.rs`, `store_worker.rs`); `result.rs` queue_full next action. Tests: `backlog.rs` (4). The `thread_list` row `backlog` field needs its producer in thread-core's listing (`ThreadResultSummary` is built literally in `thread_row.rs`). |
| #14428 collapse key (7baf0c1da4) | gap | The column and index are here (`thread/schema.rs` v16 `deliveries.collapse_key`, `deliveries_collapse`), but `collapse_key` is not offered on `thread_send` and a repeat does not yet replace its earlier undelivered delivery: the parameter and its behaviour belong to the thread tools' argument schema. |
| #14278 (67a4f3a199, 039e8efc29) | gap | `engine_version` and the provider retry wait (`waiting_on`) on `thread_list` rows need the listing producer (thread-core) and senpi#7025's published retry wait; `ThreadResultSummary` is built literally in `thread_row.rs`. |
| #14363 (b41242db44, 0fcdd7cd47) | gap | `thread_create` (not ported in mhc) and the wake path (#13037). |
| #14305 (4960cedb07) | gap | `thread_list` row identity, `unnamed` names: listing producer (thread-core) and thread-address-core `rowName`. |
| #14355, #14339 (640d1bd2ce) | n/a | Formatting of TS merge resolutions. |
| #14335 (929de417e1) | gap | `thread_create` is not ported in mhc. |
| #14333 start lane, relay lane (d82e7a2436, 5825d630a4, 5c80bd4657) | port | The storage half: `thread/schema.rs` v15 rebuilds `deliveries` so `mode_effective` takes `start`, `types.rs` `EffectiveLane::Start` (serialised `start`), and `store_ops.rs` `row_from_record` reads it back and `insert_delivery` writes it. Tests: `store_ops.rs` `each_step_from_v10_to_v16_migrates_forward_keeping_rows_written_at_that_version`, `v15_rebuild_keeps_on_delete_cascade_children_and_rolls_back_on_an_orphan`. The relay-leg lane (`effective_mode = start` on the remote leg) belongs to the relay transport (thread-remote). |
| #14333 default delivery auto (484cafd3b2) | ported | `crates/omo/components/maho-omo-gateway/src/thread/engine.rs` `deliver_inner` defaults an omitted mode to `GatewayDeliveryMode::Auto`; the thread_send schema in `tools.rs` offers auto, steer and follow_up. |
| 9b6ee2e310 (no PR) | ported | An unknown field is refused, not dropped: every thread tool schema declares `additionalProperties: false` (`tools.rs`, `control_tools.rs` `object`) and the harness validates arguments before execute. Not carried: the did-you-mean suggestion table; `prompt: null` on a create follows `thread_create` (not ported). |
| 07bec8317f (no PR) | gap | Stored session waits and holds: the v14 tables (`session_wait`, `wake_counts`) and the `wait_kind` column are now in the store, but no mhc code writes or reads them (the carrier/consumer behaviour is the thread-wake track's). |
| pair loop guard (3707d9dcef, 308a8cf429, f70d96145e) | port | `crates/omo/components/maho-omo-gateway/src/thread/store_ops.rs` `take_pair_token` answers `pair_loop_guard` with `guard`, `burst`, `window_ms`, `retry_after_ms`; `errors.rs` code, `pair_loop_guard_message`, `pair_loop_guard_next_action`; `result.rs` `engine_failure`. Not ported: the intake consumer and relay transport legs (no intake consumer in mhc). Tests: `engine.rs` `a_ninth_burst_send_to_one_session_is_refused_pair_loop_guard_with_the_measured_burst`, `result.rs` `a_refusal_that_names_its_recovery_gets_the_next_action_reading_it`. |
| 01fa0b0220 (no PR) | gap | Woken sessions: the v13 `woken_sessions` table and its index are now in the store, but no mhc code requests, leases or opens a woken session (that registry is the thread-wake track's). |
| no per-turn cap (48dd6841e2, ce613b134e) | port | `crates/omo/components/maho-omo-gateway/src/thread/store_ops.rs` `check_fanout` and `constants.rs` `MAX_FANOUT_PER_TURN` removed. Test: `engine.rs` `one_sender_turn_reaches_forty_sessions`. |
| senpi#7014 held admission (cdb5488d0f, f015ad1077, 6272d2b84b, 661f6142a9, 6912c97fad) | gap | mhc's session runtime never answers `held` (`maho-core/src/external_admission.rs` has no such admission; senpi#7014 is not in its pin), and the answer's `waiting_on` needs the #13014 delivery wait. |
| #14324 (ae55079bdb) | gap | Graceful close of another device's thread: `thread_close { graceful, wait_ms }` lives in the control and remote paths; `REMOTE_GRACEFUL_MAX_WAIT_MS` has no consumer here. |
| #14313 (336f5fa4b9) | port | `crates/omo/components/maho-omo-gateway/src/thread/store_ops.rs` `refuse_returned` (+ `store_api.rs`, `store.rs`, `store_worker.rs`); `graceful_close_returns.rs` `returned_at_close`, `settle_graceful_close`; `types.rs` `RefusalReason::TargetClosed`. Not wired: the `session_shutdown { closed }` hook (thread component) and `thread_close { graceful }` (control) need senpi#7046's `session-closed` record, which maho-core does not write yet; the closed-intake settle needs an intake ledger. Tests: `graceful_close_returns.rs` (2). |
| #14304 (93d376f3c4) | gap | `thread_list` notes for archived listings and incomplete devices: listing producer (thread-core) and remote devices (thread-remote). |
| #14308 (c6cc5178d8) | gap | `thread_list` `listing: incomplete` when an engine of this device did not answer: listing producer (thread-core). |
| #14295 (1eb9f9b48d) | gap | `thread_list` partial devices and `inactive_dropped`: listing producer (thread-core) and remote devices (thread-remote). |
| #14273 target_closed answer (030e18d0a8) | port | `crates/omo/components/maho-omo-gateway/src/thread/result.rs` `refusal_mapping` answers `target_closed` as `not_resumable` with a next action; the eval-form `thread_create` hint is not carried because mhc has no `thread_create`. Test: `a_row_its_closed_session_handed_back_reads_not_delivered`. |
| #14273 closed intake settle (a397780f93) | gap | Settling queued intake on a close needs the intake ledger (#13037). |
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

## Seen, not changed here

- `store_ops.rs` `claimant_is_dead` judges a claimant only from `/proc/<pid>`. On macOS and Windows every claimant
  therefore reads as dead, so `reconcile` can take back a row a live runtime still holds. This predates this range.
