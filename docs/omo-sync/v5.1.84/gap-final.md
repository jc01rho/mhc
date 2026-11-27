# omo v5.1.83..v5.1.84 sync ledger — `gap-final` track

Source range: `69a1863a457` (`v5.1.83`) .. `8dbccd12842f` (`v5.1.84`), repo code-yeongyu/oh-my-openagent.
This track closes the gaps the per-track ledgers left open, one slice per missing subsystem. Each slice records its
own verdicts in the ledger of the track that owns the upstream paths; this file is the index, and it records the two
items this track had to decide with evidence (E and F).

## Summary

Counts computed from the slice rows below (E and F are the two rows this track had to decide).

| verdict | slices |
|---|---:|
| port | 3 |
| ported | 1 |
| see `thread-core.md` (reply grant) | 1 |
| gap | 1 |
| blocked | 1 |
| **total** | **7** |

## Slices

| slice | upstream | verdict | where |
|---|---|---|---|
| tool receipts | #14623 `351df3416c`, `a77765fca0`, `abf5e307ac`, `1e5ecd91a9`; #14569 `cdc0aa9720`; #14560 `877fe926ec` local half | port | `crates/omo/components/maho-omo-gateway` `src/thread/tool_receipts.rs`, `create_tool.rs`, `remote/receipt.rs`, `store_tool_receipts.rs`; `tests/tool_receipts_tests.rs`, `tests/gateway_receipt_tests.rs` |
| consumer and intake | #14623 `dde26d7b4c`, `1e5ecd91a9`; #13037 design 5.4–5.5 | port | `src/thread/consumer/delivery_consumer.rs` (`create_delivery_consumer`), `src/thread/store_ops.rs` (`intake`), `store_api.rs`, `store.rs`; `tests/intake_tests.rs` |
| reply grant | #14541 `8409382654`, #14542 `ca85fc8b51` | see `thread-core.md` | `crates/omo/components/maho-omo-gateway` `src/thread/` |
| resolve | #14305 `a5f001273f` | port | `crates/omo/components/maho-omo-remote` `src/verbs.rs`, `verbs_resolve_tests.rs` |
| sdk | #14948 `bb8b61313a`, `5d2e861f2f` | ported | `crates/omo/omo-sdk-contract` `src/calls.rs`, `src/shape.rs`, `src/results/root.rs`, `tests/root_calls.rs`; see `parity.d/5184s.md` and `docs/omo-sync/v5.1.84/omo-sdk.md` |

## Tool receipts

`thread/tool_receipts.rs` `ToolReceipts` runs every gateway tool call under its idempotency key: scope
`session:<caller>`, key `args.idempotency_key` or `call:<call_id>`, a recovery bound that answers `overloaded` with
`details.budget=receipt_recovery`, `idempotency_in_progress` resumed by its owner, and Replay / Conflict / Uncertain /
Retry as the store's receipt status decides. `settle` maps an `invalid_arguments` refusal to a released key, and a
thrown call or an undecided create to `idempotency_uncertain` — which is what a lost reply answers on the retry, and
what makes #14560's local half real. `thread_send` and `thread_handoff` stay outside the wrapper: a send carries its
own delivery receipt, and a handoff's own contract decides it.

`thread_row.rs` `current_result` / `stored_row` / `stored_state` / `stored_candidates` / `renamed` / `current_row`
replay a receipt written before #14044 as today's row, so an old receipt is never a shape this build cannot read.

## Consumer and intake

`store_ops.rs` `intake` is upstream `store-worker.ts` `intake` ported: the device, delivery id and target must be
durable ids, only an agent sender reaches a session that is not running, a steer to one is a `turn_conflict`, a child
or worker target is `not_resumable`, a chain over this machine's limit is `unsupported` with `chain_count` and `limit`,
and every one of those refusals happens before the commit. The accepted record is one enqueue transaction's row with
`ledger_id = device:<device>/<deliveryId>` and its receipt keyed by the attested device alone, completed with the answer
in the same commit; a repeat answers from that receipt, and a repeat whose receipt expired but whose row is still
stored answers from the row (`answer_from_ledger_row`), which is what the argument hash would have answered.

`consumer/delivery_consumer.rs` `create_delivery_consumer` is the consumer the wake runtime starts: a handed record is
resolved first (`resolve_wake_target`: gone, ambiguous and a non-interactive session are refused before anything is
committed), committed through the intake, answered from the row, and only then is the sender's admission checked —
a yes wakes the session, a no settles the row `sender_not_admitted`. `delivery_outcome_notice` reconciles the caller's
retained remote receipt, `get_delivery_outcome` answers a receipt outcome (and checks the sender's admission again
before and after, since the service re-checks its grant), the pending list is reported after every declaration, and the
drained report runs after it with the refusal notices beside it, never ahead of it.

The local-API client behind the port (`consumer/device-service-port.ts`) is not ported: the device service in this
repository serves no `deliveries.*` protocol, so the runtime keeps taking a factory and a host that brings one binds it.

## E — live device-service `RemoteSessionsPort` over mhc-relayd: `blocked`

Upstream's `remote/port/device-service.ts` speaks a `deliveries.*`/`sessions.*` protocol to the device service. mhc's
relay daemon answers the requests it serves and refuses this list by name (`crates/omo/mhc-relayd/src/local_server.rs`
`UNSUPPORTED_REQUESTS`):

`refresher.register`, `device.adopt`, `enforcement.report`, `owner.shutdown_for_upgrade`, `session.endpoints`,
`session.locate`, `session.read`, `session.command_part`, `sessions.events.subscribe`, `deliveries.serve`.

What each missing RPC carries: `session.endpoints` and `session.locate` are #14748's lookup gate and #14799's reopen
path; `session.read` is #14278's listing row (`waiting`, `engine_version`) and the read leg; `session.command_part`
and `sessions.events.subscribe` are `25dc4d8220`'s follow stream; `deliveries.serve` is the delivery consumer's own
declaration. Porting this port is not a small addition on both sides — it is the whole device-service delivery
protocol — so the row stays `blocked` with those six names as the gap. `#14091`'s `onClose` arrives with the same port
and has nothing to release before it: mhc's `RemoteClient` owns no per-connection sessions port, and
`AttachPool::close` already ends held attaches.

## F — `fork_from` on an engine advertising `open_session_fork`: `gap`

`open_session_fork` appears nowhere in this tree (`grep -rn open_session_fork crates/` is empty), so no mhc engine or
session host advertises it, and `thread/control/plan_create.rs` refuses `fork_from` as `fork_unsupported` before any
open. The upstream work (#13031 `aef88019f8`, `90dc3fef61`, `07060a225b`) is a reader of `ThreadHost.capabilities`
plus the fork branch in the control legs; with no capability to read and no `SessionHost.forks`, the refusal this build
gives is the true answer, and it is the row's `gap`.

## The sdk slice

The omo SDK root's type is computed from one table, `ROOT_CALLS` (134 rows over the 14 namespaces of design 4.2). That
table, its per-row input schemas, its answer shapes and its wait flag are data, so this slice carries them as data in
`crates/omo/omo-sdk-contract`:

- `src/shape.rs` is the TypeBox shape vocabulary as data (`Shape`, `FieldSpec`, `accepts`, `sample`,
  `all_fields_optional`, `names_field`).
- `src/calls.rs` is `ROOT_CALLS`: each row's `key`, input `Shape`, `AnswerShape` (`value`/`thread`/`stream`) and
  `waits`; `input_accepts` is `validate.ts`'s `inputAccepts` over every row, and `takes_deadline_option` is design 4.4's
  wait-options rule ("`deadline_ms` only on waitable calls whose input does not already name it").
- `src/results/root.rs` restates the answer records the contract directory declares itself; a credential-bearing call's
  answer stays with `src/results/custody.rs`, a thread call's with the thread contract.
- `tests/root_calls.rs` ports the table cases of `contract.test.ts` and `contract-controls.test.ts`.

Not carried, with the reason each is not: `root.ts` (`Tree`/`Member`/`Answer`/`OptionsFor`, `Partial<T> extends T`) is
TypeScript type-level machinery, and the root object is slice P1b's; `root-options.ts`, `cancellation.ts`'s signal
plumbing and `failure.ts`'s stream classes are runtime JS host surfaces; `gated.ts`, `ports.ts` and
`descriptor-ahead.ts` are TS parity tooling; `results/threads.ts` re-exports the thread contract's records, which
`maho-omo-gateway` carries.

The table agrees with the pinned TypeBox schemas on 4,398 generated probes over all 134 rows (each row's minimal sample,
a sample with every optional field filled, and every single-field removal, retype and unknown-key addition, nested),
except the 19 that name a JS handle (`api_key` on the five launch calls, `signal` on `engine.schedule.run` and
`threads.runToCompletion`): TypeBox types those `{}`, which admits any JSON value, while this crate's earlier slice ruled
a handle has no JSON form, and that ruling stands. The answer records agree on their own samples and field mutations.
