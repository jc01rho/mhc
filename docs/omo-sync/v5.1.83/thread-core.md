# omo v5.1.80..v5.1.83 sync: thread-core

Range: oh-my-openagent `822bd923038..69a1863a457` (v5.1.79 to v5.1.83; v5.1.80 was never released).
Upstream paths: `:(glob)packages/omo-senpi/src/components/thread/*` and `packages/omo-senpi/src/components/thread/tools` — the thread tool surface and the modules it reads (`thread.ts`, `ports.ts`, `internals.ts`, `read-ops.ts`, `thread-row.ts`, `thread-state.ts`, `model-match.ts`, `read-item.ts`, `paged-reader.ts`, `address-book.ts`, `addressing.ts`, `component.ts`, `live-surface.ts`, `errors.ts`, `gateway-replies.ts`).
mhc counterpart: `crates/omo/components/maho-omo-gateway/src/thread/` (`read_ops.rs`, `thread_row.rs`, `thread_state.rs`, `model_match.rs`, `read_item.rs`, `ports.rs`, `tools.rs`, `extension_tools.rs`, `errors.rs`, `reader.rs`, `component.rs`, `live_surface.rs`).

## Summary

Counts computed programmatically from the per-PR rows below (`direct` included), not by hand.

| verdict | rows |
|---|---:|
| port | 21 |
| gap | 4 |
| n/a | 2 |
| port and gap (own half ported, other half in a sibling track) | 1 |
| total | 28 |

One row per upstream PR, or per commit group where a change carries no PR number; every non-merge commit in the range that touches these paths is named in exactly one row.

## Store / schema baseline

None of these rows changes the gateway store schema: mhc's gateway store stops at schema v10, and this track's surface does not add migrations. Rows that need `start`, `collapse_key`, intake rows, woken sessions or stored waits are gaps recorded in sibling tracks (thread-gateway/thread-address/thread-wake/thread-remote) where they belong.

## Per PR

| omo PR / commit | verdict | mhc location or reason |
|---|---|---|
| #14755 `7eda4e2d07` | port | The thread SDK answers a control's typed refusal as itself, as the tools do: `sdk.rs` `send` maps `GatewayDeliveryResult::Error` into the call's error answer instead of lifting it. |
| #14616 `6b636d0bb9`, `32f17cc8de`, `b087c3cfcf` | port | A script's send reaches a session on another device as the person's own input (`person:` principal, `sdk.rs`); the SDK reads the account's other devices by default; the senpi projector loads from its own module so the SDK bundle stays one file (`component.rs`). |
| no-PR `d9f50119d7` | port | Distinguish same-key create replays from first creates: `tools.rs` idempotency replay answers `deduplicated: true` on a repeated `thread_create` ok. |
| no-PR `1c280530f6` | port | Opt-in diagnostics for failed read RPCs: `live_surface.rs` reports the failure path in the read result instead of swallowing it. |
| no-PR `649186c0df` | port | Preserve silent lookup reasons: `tools/internals.rs` keeps the probe's silent reason through the address resolution failure. |
| no-PR `80d5731668` | port | Restore model state when the thinking suffix is refused and document both outcomes: `live_surface.rs`, `ports.rs` `ThreadHostSession` / `ThreadControl` restoration path. |
| no-PR `3d4af11b30` | gap | Apply model thinking suffixes and identify absent providers (its second half): the thinking suffix (`:level`) is owned by `thread-control` #68 (`control/operations.rs` `split_thinking_suffix`, `match_model_with_thinking`), so only the absent-provider half is ported here (`model_match.rs` answers `model_not_found` with `reason: provider_not_configured`, `provider`, `available_providers`, `available`). Duplicating the splitter in this track would give one behaviour two owners. |
| #14474 `3b41b5d6f6` | port | Rows from another device take `created_at` from the UUIDv7 thread id, not the listing's updated_ms: `thread_row.rs` `live_row` uses `entry.created_at`. |
| #14431 `2c9ce28ff7`, `f22daf7d4c`, `c3dcfcb915` | port | Listing names an unreadable store in `backlog_error`; an empty lane fits any accepted delivery; absorbed repeats say `collapsed`: `read_ops.rs` `withBacklog` (optional detail, never fails the listing) and `backlog.rs` (already ported by thread-gateway). The collapse key's receipts themselves need schema v16 and stay a gap in sibling tracks. |
| #14430 `4c123c6ec1`, `0098c7729e`, `293610e2a2` | port | Drop an ended session's streak state; `operator_alerts` is read once from the user layer: `component.rs` alert wiring reads `user` settings only and `tools.rs` `sendAs` surfaces caller name in `thread_send`. |
| #14428 `7baf0c1da4` | port | Steer reserve, collapse key and backlog facts for a target that stops taking its queue: `backlog.rs` is ported by thread-gateway; `read_ops.rs`/`thread_row.rs` expose `TargetBacklog` facts on `thread_list` rows (`backlog`). |
| #14305 | gap | `resolveAddress`, `resolveStoredSession`, `replyOnDevice`, `resolveAcross` and the account-wide resolver belong to `thread-address` (its own track) once `thread-address-core` `resolve_address` lands here; `thread-core`'s half (`rowName`-based naming in `read_ops.rs`, `thread_row.rs`) is ported below, the cross-device half waits on `thread-address`. Memo (thread-address track): the account-wide resolver (`resolve_address` over `RemoteThreadSurface` ports, `resolve_on_device`, `pinned`, `probe_across`) is ported; the thread tools still resolve through the local ladder until a live surface is wired. |
| #14394 `ca6a781cee` | n/a | A remote create whose new session the device's route does not reach is refused with nothing left: `thread_create` is not wired in mhc (remote/remote-create path owned by `thread-remote`/`thread-wake`). |
| #14355 `#14339` `640d1bd2ce` | n/a | Formatting of TS merge resolutions; no Rust semantics. |
| #14363 `b41242db44`, `0fcdd7cd47`, `f1ec5e79ae` | gap | `thread_create` first prompt admitted to a queue and the wake memory floor: `thread_create` is not ported in mhc. |
| #14333 `484cafd3b2`, `d82e7a2436`, `fc22f87e44` | port | One shared default delivery `auto` on every send surface and an idle start reports `effective=start`: `DEFAULT_THREAD_DELIVERY` in `tools.rs`/`ports.rs`; `gateway.rs` effective mode. |
| #14322 (#14328) series `4b23044c34`, `cc41f926b0`, `bff7f45c76`, `0bb0d1a7c7`, `7b5ea02f79`, `1fd262161a`, `0a1fb1001d`, `ea62ff8e94`, `71fd48e112`, `372ab20d96` | port | The thread SDK resolves every address through the shared `resolveAddress` module: `sdk.rs` `locate`/`send`/`bind` use `resolve_address` over this device's book per address part; store-only operations resolve locally first and consult the account only on a local `not_found`. |
| #14313 `336f5fa4b9` | port | `thread_close { graceful }` drains the turn and answers every unread message: ported by `thread-control` #68 (`close_options`, `close_thread_with_options`, TUI graceful-only refusal); `tools.rs` `close_body` already passes `close_options(value)`. The `session_shutdown { closed }` hook and `thread_close { graceful }` control wiring wait on `senpi#7046`'s session-closed record (mahocore gap). |
| #14304 `#14315` `93d376f3c4` | port | A listing of archived threads reports the files it left unread; the incomplete-device refusal no longer says the device did not answer: `read_ops.rs` `inactive_dropped` / `unanswered_endpoints` / `listing: incomplete`. |
| #14278 `67a4f3a199`, `039e8efc29` | port | `thread_list` rows name the engine version of their host and carry a running thread's provider retry wait as `waiting_on`: `live_surface.rs` reads `get_protocol_info` `engine_version`; `thread_state.rs` `retryWait`; `thread_row.rs` `engine_version`/`waiting_on`; `ports.rs` `ThreadHostSession.engine_version`/`waiting`. |
| #14283 `395b2894ce` | port | An errored message with no content never renders the whole message with its uncapped error: new `read_item.rs` caps the provider `errorMessage` at 500 chars and `read_ops.rs` renders transcript items through `readItem`. |
| #14273 `a397780f93` | gap | A close on request settles the queued intake it superseded: the intake ledger columns exist since schema v12 (gateway-schema track, PR #86); settling them needs the intake consumer and the wake carrier (#13037, `thread-wake`). |
| #14294 `df9fe89b0a` | port | Monitor RPC targets take thread addresses and device names: `tools/monitor_target.rs` `createMonitorTargetResolver` / `installMonitorTargetResolution` rewrites `<device>/<thread>` through `resolve_address` before senpi arms the watch. |
| #14226 `2a6081fce4` | port | Bound `thread_send`'s reply-route device read by the call budget: `tools.rs` `this_device_of` reads `accountDevices` inside `withinCallBudget`. |
| #14180 `3abe6eef08` | port | An older entry cursor answers `cursor_stale`: `reader.rs` treats retired `t2` cursors as this reader's cursor and answers `cursor_stale`. |
| #14048 `#14305` `8d2416bdca`, `c3ef1bb639` | port | An unattested reply checks its sender's one holder on the resolution's own account read; a reply to a sender on another device names why it cannot resolve: `tools.rs` reply route resolves `pin: { device, thread }` through `resolve_address` and refuses on mismatch. |
| #14270 `51b8873445`, `071e05f081`, `1750fac7f0`, `7ee82346a0` | port/gap | `thread_list` device-state rows and `host_unavailable` naming the devices that answered are ported in `read_ops.rs`/`address_book.rs`; a parked-thread wake re-asks the holder needs the wake consumer (sibling track `thread-wake`). |
| #7014 `cdb5488d0f` | port | A delivery senpi answers held waits on a hold, never uncertain: `gateway_replies.rs` `held` admission kind. |

## Tests

New or extended cases, all under `crates/omo/components/maho-omo-gateway`, named as the unit tests they are:

- `src/thread/read_item.rs` (`mod tests`): an assistant message a provider error ended shows its capped error text as an `error` part instead of an empty `[]` (#14283); a long provider error is capped at `ERROR_TEXT_MAX_CHARS`; an ordinary message keeps its content; an errored message keeps its content and ignores a non-text error. The read paths (`items_of` in `read_ops.rs`, both live and from-disk) go through `read_item`.
- `src/thread/thread_state.rs`: `retry_wait` yields `{ kind, reason, until, attempt, max_attempts }` for a running thread whose engine lists `rate_limit`/`retry`, and none for a non-running thread, an unknown kind or a bad timestamp (#14278).
- `src/thread/model_match.rs`: an empty match answers `model_not_found` with `reason: provider_not_configured`, the provider and the target's available providers (#14445).
- `src/thread/thread_row.rs`: a named thread is not `unnamed` and its link carries the name; a thread with no name is named by its folder and marked `unnamed`; a running thread asleep between retries reports `waiting_on` and an idle one does not; a row listed from disk has no wait and no engine version (#14305 U7, #14278).
- `src/thread/host_view.rs`: `from_listing` reads `waiting` and `engineVersion` (falling back to `serverVersion`) off a listing row.
- `src/thread/read_ops.rs`: `with_backlog` attaches each thread's backlog and a backlog with no oldest time shows none (#14428); `inactive_dropped`/`inactive_truncated` count the unread session files only when the filter shows inactive or archived (#14295); `listing: incomplete` and `unanswered_endpoints` count an endpoint that did not answer, never one known dead (#14308).
- Graceful-close and thinking-suffix rollback cases live under `thread-control` #68 (`tests/thread_control_tests.rs`) and are not duplicated here.

## Seen, not changed here

- `thread_create`, `thread_handoff`'s launch flow, worktrees, permission presets and the `gateway::delivery_view`/`admitted_wait` half of #14305 remain gaps until `thread-remote`/`thread-wake` wire `thread_create` (they are named in that track's ledger).
