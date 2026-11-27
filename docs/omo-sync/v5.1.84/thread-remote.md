# omo v5.1.83..v5.1.84 sync ledger — `thread-remote` track

Source range: `69a1863a457` (`v5.1.83`) .. `8dbccd12842f` (`v5.1.84`)
Upstream paths: `packages/omo-senpi/src/components/thread/remote`, `packages/omo-senpi/src/components/remote` (31 non-merge commits)
mhc counterparts: `crates/omo/components/maho-omo-remote`, `crates/omo/components/maho-omo-gateway/src/thread/remote/`

## Summary

| verdict | count |
|---|---:|
| port | 5 |
| ported | 0 |
| blocked | 5 |
| gap | 0 |
| n/a | 3 |
| **total** | **13** |

## Scope

The live device-service implementation of `RemoteSessionsPort` (`remote/port/device-service.ts` over `mhc-relayd`) is
still absent in mhc, as the v5.1.83 ledger recorded. Every upstream change that only lives in that adapter (the
#14748 lookup gate, rows 6/7 copying, receipts over `get_delivery_outcome`, reopening a parked session, the
attach-stream follow) is `blocked` on it. The #14358 refusal mapping and its one re-attach are ported where mhc
already sends commands: the attach pool and the gateway's `DeviceServicePort`.

## Per-PR verdicts

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #14358 `engine_capability_missing` naming a transient cause; a command refused `attach_required` re-attaches and is sent once more; the retry goes the attach way only (`8b96ca71dd`, `4281730b5a`, `ecd6f27afd`, `da5da7ef10`, `349d0134e5`) | port | `maho-omo-gateway/src/thread/remote/outcome.rs` (`engine_not_ready`, `attach_required`, `command_not_ready`, `not_ready_failure`: `relay_outcome_failure` and `outcome_failure` answer `host_unavailable` with `details.cause` for `attach_required`/`daemon_unavailable`, a bare `engine_capability_missing` stays `unsupported`), `attach_pool.rs` (`session_command`: an `attach_required` answer drops the pooled attach and is sent once more on a fresh one; a direct send so refused goes the attach way and is never sent directly twice), `device_service.rs` (`command_reattaching` for the `get_state` probe, the send and the steer-race retry). `relay-transport.ts`'s `ADMIT_UNSUPPORTED` guard has no separate Rust branch: mhc's remote send has no `admit_external_message` leg, so `relay_outcome_failure` carries the mapping. Tests: `outcome.rs` (`an_engine_capability_missing_refusal_naming_a_transient_cause_is_host_unavailable`, `a_command_refused_engine_capability_missing_names_its_cause_or_stays_unsupported`), `attach_pool.rs` (4 cases from `device-service-attach-required.test.ts`). |
| `/remote` display escaping (`989f0a8a06`, tests `944c86d6a4`, `24642a0c89`) | port | `maho-omo-remote/src/command.rs` (`shown_lines`, `device_option`, `session_options`): every verb output line, picker option and dialog title is escaped with `maho_omo_gateway::thread::remote::ports::escape_for_display`, while the argv a verb runs with keeps the raw target. `maho-omo-remote` gains a path dependency on `maho-omo-gateway` (one `Cargo.lock` line, no new external crate). Tests: `command.rs` (`the_picker_shows_escaped_options_and_keeps_the_raw_target`, `a_verbs_output_is_shown_line_by_line_escaped`); `24642a0c89` only shortens the TS fixture loops. |
| typed reference refuses C1 controls, format characters stay addressable (`a29c2980b8`) | n/a | In this track's paths the commit only adds a `remote-text-escape.test.ts` case; the rule itself lives in `thread/addressing.ts` (thread-core track) and `utils/escape-for-display.ts` (misc-skills track). |
| #14748 lookup gate: solo probe lookups exclude every other locate of the port, the fence and admission rules, and watch recovery retrying a fence refusal (`4f4215f9f4`, `bd8df618cb`, `4051b7d173`, `2cb91cd067`, `de9066508b`, `049533bb09`, `e2abf877ad`, `e6799eda7f`, `5c132ac1d8`) | blocked | mhc has no device-service implementation of `RemoteSessionsPort` (`lookup`/`locate` over `mhc-relayd`; the trait's `lookup` defaults to `Unavailable`) and no remote session watch (`remote-watch.ts`), so the gate would have no caller. `e6799eda7f`/`5c132ac1d8` only move a TS barrel export. |
| #14799 `thread_read`/`thread_send` reopen a session another device parked (`283011ad26`) | blocked | `reopen-parked.ts` needs `find_session` (`holderView`, `sessionFileOf`) and `open_session` through `RelaySessionHost`; `thread/control/plan_create.rs` still refuses those remote opens because `mhc-relayd` carries neither RPC. |
| #14623 remote delivery receipts and #14278-era reconcile of queued delivery outcomes (`351df3416c`, `30411b35be`, `1e5ecd91a9`, `abf5e307ac`) | port | `thread/remote/receipt.rs` `read_receipt` reads and reconciles through the gateway store members now ported (`store_tool_receipts.rs` `remote_receipt`, `store_ops.rs` `receipt_outcome`, `store_tool_receipts.rs` `reconcile_remote_receipt`) and answers `ThreadReceiptOk`. Tests: `tests/gateway_receipt_tests.rs` `a_receipt_read_reports_the_callers_own_delivery_to_exactly_that_thread`, `a_receipt_read_of_a_remote_send_reports_the_outcome_its_retained_receipt_holds`. The `RemoteSessionsPort.receipt` leg (`get_delivery_outcome` to the peer) is answered by the consumer once a live device-service port exists, which stays this track's blocked item. |
| #14278 another device's `thread_list` rows carry the holder's retry wait and engine version (`ad364fb654`) | blocked | The fields are copied by the live device-service listing adapter (`remote/port/device-service.ts` `listing`), which mhc does not have; `device-service-core` already carries `waiting`/`engine_version` on its listing rows (v5.1.83 device-service track). |
| #13031 / senpi#7085 `fork_from` forks on an engine that advertises `open_session_fork` (`aef88019f8`) | port | The relay leg's decision and capability read: `thread/control/plan_create.rs` resolves the source across the account and, for a source on device b, asks b's engine endpoint for an observing `get_protocol_info` (`relay_forks` over the new defaulted `RemoteSessionsPort::endpoint_command`, which `remote/closed_rows.rs` forwards); an engine that does not list `open_session_fork`, or does not answer, is refused `fork_unsupported` naming b, as upstream. Only the open on b itself stays refused (`relay_session_host_unsupported`): `mhc-relayd` carries no remote `open_session` (there is no `RelaySessionHost` open), so the fork on b and `openRefusal`'s fork arm have no reply to read. This device's engine forks (`maho-rpc` `open_session { forkFrom }`, see thread-gateway's row). Test: `tests/remote_list_and_close_tests.rs` `thread_create_fork_on_b_reads_only_bs_capabilities_and_opens_nothing_there`. |
| retain buffered output from the identified active send (`25dc4d8220`) | blocked | `followRemote` streams a remote attach's records; mhc's `thread/remote/remote_ops.rs` has no attach-stream follow because `mhc-relayd` has no attach event stream. |
| #14091 closing a remote client releases its per-connection sessions ports (`182c5aa2c4`) | blocked | mhc's `RemoteClient` owns no per-connection sessions port to release; `AttachPool::close` already ends held attaches. The `onClose` hook arrives with the live device-service port. |
| #14305 U8 `sessions.resolve` on the shared thread-part ladder (`a5f001273f`) | port | `maho-omo-remote` `verbs.rs` `match_session_rows` resolves through `thread-address-core` `match_thread` (`LadderView`): exact id, then the listing or row identity key, alias, name, and an 8+ hex prefix. Tests: `verbs_resolve_tests.rs` (7 cases). |
| remove thread bindings, the relay tools and their store (`4f6a56bbc0`) | n/a | In the remote paths the commit only drops `binding_id`/`binding_revision` and moves `hashArgs`; mhc's remote send (`device_service.rs`) never wrote binding fields, and the binding store belongs to the thread-gateway track. |
| TS formatting and bundle regeneration (`80e682a504`, `499aec3431`) | n/a | Formatting and generated bundle only; no Rust counterpart. |
