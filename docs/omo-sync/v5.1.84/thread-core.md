# omo v5.1.83..v5.1.84 sync: thread-core

Range: oh-my-openagent `69a1863a457..8dbccd12842f` (v5.1.83 to v5.1.84).
Upstream paths: `packages/omo-senpi/src/components/thread/*` (root files), `thread/{tools,address,wake,run-alerts}`, `packages/thread-address-core`.
mhc counterpart: `crates/omo/components/maho-omo-gateway/src/thread/` (root modules, `address/`, `wake/`, `run_alerts/`) and `crates/omo/thread-address-core`.

## Summary

Counts computed programmatically from the per-PR rows below.

| verdict | rows |
|---|---:|
| port | 17 |
| gap | 3 |
| n/a | 8 |
| blocked | 0 |
| ported | 0 |
| total | 28 |

Every one of the 48 non-merge commits in the range that touch these paths is named in exactly one row.

## Per PR

| upstream PR | verdict | mhc location or reason |
|---|---|---|
| #14305 U8 `a5f001273f`, `4fda0171b4` | port | `crates/omo/thread-address-core/src/ladder.rs` (`match_thread`, `LadderView`, `LadderStep`, `LadderMatch`, `IdPrefixStep`, exported from `lib.rs`; tests in `tests/address_suite.rs`); `maho-omo-gateway/src/thread/addressing.rs` `resolve_target` runs that ladder and keeps only the workspace scope and the wording. The core never exported an `ID_PREFIX_SHAPE` constant (`is_id_prefix` is the predicate), so the drop needs no change. The remote client's `sessions.resolve` on the same ladder is `maho-omo-remote` (thread-remote track). |
| #14305 U8 `ce9cc56916`, `249131aae2` | n/a | Typing fixes to the TS cross-surface table test (`address/surfaces.table.test.ts`: optional fixture ports, `onClose` on the client core); it drives the TS remote client, monitor and SDK surfaces over the TS two-device fixture, which have no one-to-one Rust harness. |
| #12867 `9df82b93ee` | port | `thread-address-core/src/parse.rs` `parse_address`: any backslash and a drive prefix (`C:dir/x`) read as a bare path (`has_drive_prefix`); the four new parse cases are in `tests/address_suite.rs`. |
| #13304 `944c86d6a4`, `ca01f22933`, `a29c2980b8`, `da611a9473` | port | `addressing.rs` `has_control_character`: a typed reference or a new name refuses every control character (C1 included) after the whitespace controls fold into a space; format characters stay addressable. `tools.rs` `ExtensionToolRegistration::session_start` logs a skipped tool's name through `escape_for_display`. Tests: `addressing.rs` (`a_typed_reference_holding_a_control_character_other_than_whitespace_is_refused_unechoed`, `a_whitespace_control_folds_into_a_space_and_a_format_character_stays_addressable`, the tab case of the exact ladder), `tools.rs` (`a_skipped_extension_tool_is_logged_with_its_name_escaped`). mhc has no fuzzy query (`fuzzyMatch`), so that third refusal has no Rust counterpart. |
| `1a232eb8c9`, `24642a0c89` | n/a | `tools/render.ts` display escaping of the thread tool result view, and TS test timeouts: a `ThreadTool` carries no call/result renderer in mhc (the native registration prints `ToolResult::text`), so there is no shown line to escape. |
| omo#14972 `223d85cfed` | port | `sdk.rs` `ThreadSdkOptions.same_device_wake`: an SDK send that queued for a parked interactive session runs `SameDeviceWake::after_send`, as `LocalTransport` does for an agent's `thread_send`. Test: `tests/same_device_wake_tests.rs` `an_sdk_send_to_a_parked_interactive_session_requests_its_wake`. |
| `25d8ea6771` | port | `wake/carrier.rs` `CarrierOptions.on_error` (`CarrierErrorListener`): every wake, takeover and evaluation the carrier tends by itself (sweep, start sweep, timer tick, lifecycle re-evaluation) reports its failure instead of dropping it; `wake/runtime.rs` warns `the carrier's sweep failed and retries at its next tick` and, for a send's wake, `<id> was not woken and is retried by the sweep` before returning the error. The TS cases wrap the store to make `woken.start` throw; the Rust carrier takes a concrete `SqliteGatewayStore` with no wrap seam, so they are not carried. |
| #14986 `6b8c7a23de`, `8f6459c749`, `50189c7524`, #15118 `71010471b2` | n/a | TS test hygiene: one short socket temp root on POSIX/win32, wake tests removing their temp dirs, disposing the stores before removing their directory. The Rust wake tests use `tempfile::TempDir` (removed on drop) and open no Unix sockets under `/tmp`. |
| `64e1b806a4` | n/a | A TS `thread-remote.test.ts` case (two concurrent bare-id reads of closed threads on two devices); the read leg over another device is thread-remote's, and the case changes no thread-core module. |
| #14807 `df5a3b82f2` | n/a | `tools/local-permission.ts`: a `ThreadTool` has no permission-parser seam in mhc (same call as `docs/omo-sync/v5.1.83/thread-control.md`), so there is no approval ask to extend to `thread_read`. |
| #14893 `6185e86ebf` | port | `address_book.rs` `find_disk_sessions`: a scoped name lookup keeps only sessions whose header cwd is a workspace root or below it (path components), so `<root>-tmp` no longer matches `<root>`'s directory prefix. Test: `a_scoped_name_lookup_skips_a_sibling_directory_that_extends_the_workspace_path`. |
| #14305 `fe8d250e97` | port | New `thread/delivery_ledger.rs` `ledger_device` (URI-decoded device part, a key with no `/id` still names its device, an empty part names none); `store_woken_ops.rs` drops its own copy and `intake_admission.rs`, `wake/carrier.rs` and `store_woken_ops.rs` read through it. Tests: `delivery_ledger.rs` (the upstream table) and `tests/wake_store_tests.rs` (`device:dev-b` now names `dev-b`). |
| #14799 `283011ad26` | gap | `thread_read` reopening a session another device parked needs `remote/reopen-parked.ts` (`reopenParkedRow`) and the relay control over the reopened row, which mhc's `thread::remote` does not have yet (thread-remote track); `read_ops.rs` still answers such a row from its session file or `not_resumable`. |
| #14748 `915aeab0e1`, `bd8df618cb`, `4f4215f9f4`, `4051b7d173`, `5c132ac1d8`, `e6799eda7f` | port | New `address/solo_retry.rs` `ask_silent_alone` (one device at a time, only while the call budget holds a lookup plus `READ_RESERVE_MS`, an unmade or `Unavailable` retry keeps the silent answer); `address/probe.rs` `probe_across` asks the silent devices again when none found the thread; `remote/ports.rs` `RemoteSessionsPort::lookup_alone` (defaulted to `None`) and `remote/closed_rows.rs` forwards it. Tests: `solo_retry.rs` (E12, E15, E19/E6, E23, E26, E36). The port's lookup gate (`remote/port/lookup-gate.ts`) belongs to the device-service port implementation (thread-remote); E14 is timing-driven (a 2.5 s sleep) and not carried. |
| #14623 `351df3416c`, `a77765fca0` | port | `thread/tool_receipts.rs` `ToolReceipts` (`execute`, `admit`, `settle`, `unrecorded`): every gateway tool call runs under its idempotency key, scope `session:<caller>`, key `args.idempotency_key` or `call:<call_id>`, and the recovery bound answers `overloaded` with `details.budget=receipt_recovery`; `tools.rs` `receipted()` wraps every tool but `thread_send`/`thread_handoff`. Tests: `tests/tool_receipts_tests.rs` (`a_retried_call_replays_its_answer_and_the_side_effect_runs_once`, `the_same_key_with_different_arguments_conflicts_and_runs_nothing`, `a_refused_call_releases_its_key_for_a_fixed_retry`, `a_receipt_the_store_could_not_settle_answers_uncertain_on_the_retry_and_runs_the_side_effect_once`, `an_admission_the_store_refused_before_writing_answers_the_lock_and_the_next_call_runs_once`, `a_receipt_committed_before_its_reply_was_lost_is_recovered_once_and_replayed_after`, `a_full_recovery_bound_refuses_a_new_key_until_one_recovered_key_frees_its_slot`). |
| #14569 `cdc0aa9720` | port | `create_tool.rs` `create_body(..., retrying)` with `CreateSurface::retrying`: a create whose open may already have made its thread is retried as the recovery it is, keeping the first cause. Test: `tests/tool_receipts_tests.rs` `a_create_left_uncertain_keeps_its_first_cause_and_the_recovery_settles_then_replays`. |
| #14560 `877fe926ec` | port | The local half: the same `create_body(..., retrying)` and `tool_receipts.rs` `settle`, where a thrown call or an undecided create leaves `idempotency_uncertain` for the next attempt. The relay create leg (`RelaySessionHost`) stays thread-gateway's blocked item. |
| #14541 `8409382654`, #14542 `ca85fc8b51` | gap | The reply grant widens a scoped refusal for a consumed delivery's sender; mhc's `thread_send` has no reply allowance (`tools/reply-permission.ts`, `tools/reply-target.ts`, consumed senders), so there is nothing to widen. |
| `abf5e307ac`, `1e5ecd91a9` | port | `thread/remote/receipt.rs` `read_receipt` answers `thread_read { thread, delivery_id }` from the receipt-outcome store reads (`store_ops.rs` `receipt_outcome`, `store_tool_receipts.rs` `remote_receipt`), and `control_tools.rs` routes the call to it. Tests: `tests/thread_control_tests.rs` `thread_read_with_a_delivery_id_answers_that_deliverys_outcome_without_a_host`, `tests/gateway_receipt_tests.rs` `a_receipt_read_reports_the_callers_own_delivery_to_exactly_that_thread`, `a_receipt_read_of_a_remote_send_reports_the_outcome_its_retained_receipt_holds`. |
| #14278 `ad364fb654` | port | `thread_state.rs` `holder_version` (nonempty, at most 128 UTF-16 units) and `live_surface.rs`: the engine version is `engineVersion` when the key is present, the legacy `serverVersion` only when absent, through that validator. Test: `a_holder_version_is_a_nonempty_string_of_at_most_128_utf16_units`. The relay row half (`control/relay-session-host.ts` copying a listing row's `waiting`/`engine_version`) is thread-gateway's. |
| `6de23b79b2` | port | `component.rs` test `the_tool_set_covers_send_and_control_and_no_binding_tool_is_reachable` asserts none of the six binding tools is listed or executable. |
| `a130d78c73` | port | `contracts_state.rs` `ThreadState::Opening`; `thread_state.rs` `thread_state(archived, held, opening, activity)`; `thread_row.rs` `live_row`: a session whose engine lists it `opening` is `opening`, with no controls, no activity and no retry wait; `control_tools.rs` `thread_list` takes `opening`. Tests in `thread_state.rs` and `thread_row.rs`. The `thread_close` refusal `session_opening` is `control/locate.ts` (thread-gateway). |
| `572e5a6a39` | port | `reader.rs` `ReadSource::Jsonl { messages_view }`: a tool read pages the visible messages (revision suffixed `.m`, so a raw-entry cursor is stale there and the reverse); `read_ops.rs` `read_degraded` reads that view. Test: `a_messages_view_pages_only_visible_messages_and_refuses_a_raw_entry_cursor`. |
| #13031 `aef88019f8`, `90dc3fef61`, `07060a225b` | gap | `fork_from` on an engine that advertises `open_session_fork` is decided in the control legs (`control/local-open.ts`, `session-host.ts`, `relay-session-host.ts`, thread-gateway); the root half is only `ThreadHost.capabilities`, which has no reader in mhc until those legs land. |
| `4caaa6d488` | n/a | The SDK half refuses a non-object request: `ThreadSdk` takes typed Rust arguments and has no `list`, so there is no untyped request to refuse. The v15 binding-table drop is gateway schema (thread-gateway). |
| `bf4633eb06` | n/a | Rewinds a TS migration fixture of `extension-tools.test.ts` past v15; no Rust module changes. |
| `da19e1d32d`, `0635bcfac0` | n/a | Formatting and unused imports of a TS merge; no semantics. |
| `4f6a56bbc0` | port | This track's half of the binding removal: `tools.rs` drops `thread_report`, `thread_bind`, `thread_bindings`, `thread_outbox`, `thread_outbox_ack`, `thread_answer` and the `relay` option; `component.rs` builds no `GatewayRelay` and loses the `locate` option only the relay read; `sdk.rs` drops `report`, `bind`, `unbind`, `rebind`, `settle` and `report_failure`; `native.rs` registers only `thread_send` (with a device service). The relay engine, `store_relay_ops.rs`, the binding tables and their v15/v16 drop are gateway modules (thread-gateway track) and are left in place, now without a tool caller. |

## Tests

New or changed cases (no sleeps; each asserts machine-read values):

- `crates/omo/thread-address-core/tests/address_suite.rs`: the four new path cases of `parse_address`; `match_thread` step order, ties, name scope and the prefix switch (the upstream `ladder.test.ts` table, every row).
- `maho-omo-gateway/src/thread/addressing.rs`: C0/DEL/C1 refusals with the value unechoed, whitespace controls folding, format characters addressable, the tab case of the exact ladder.
- `maho-omo-gateway/src/thread/delivery_ledger.rs`: the upstream `delivery-ledger.test.ts` table; `tests/wake_store_tests.rs` updated for a key with no id part.
- `maho-omo-gateway/src/thread/address/solo_retry.rs`: E12, E15, E19/E6, E23, E26, E36 of `solo-retry.test.ts`.
- `maho-omo-gateway/src/thread/reader.rs`: the messages view and its cursor separation.
- `maho-omo-gateway/src/thread/address_book.rs`: the `<root>-tmp` sibling case of `offline-address.test.ts`.
- `maho-omo-gateway/src/thread/thread_state.rs`, `thread_row.rs`: holder version bounds, the opening state.
- `maho-omo-gateway/src/thread/tools.rs`: the escaped skip log; `component.rs`: no binding tool reachable.
- `maho-omo-gateway/tests/same_device_wake_tests.rs`: an SDK send wakes a parked interactive session.

## Left for sibling tracks

- thread-gateway: tool receipts (`store-tool-receipts.ts`) for #14623/#14569/#14560, receipt outcomes for `thread_read { delivery_id }`, the fork legs of #13031, `session_opening` on `thread_close`, the relay row's `waiting`/`engine_version` (#14278), and the gateway half of the binding removal (relay engine, `store_relay_ops.rs`, schema v15/v16 drop).
- thread-remote: `reopen-parked.ts` (#14799), the device-service lookup gate behind `lookup_alone` (#14748), `sessions.resolve` on `match_thread` (#14305 U8).
- A reply allowance (#14541/#14542) has no subsystem in mhc yet.
