# omo v5.1.83..v5.1.84 sync ledger — `native-thread-host` track

Source range: `69a1863a457` (`v5.1.83`) .. `8dbccd12842f` (`v5.1.84`), read at `v5.1.84`.
Upstream: `packages/omo-senpi/src/components/thread/` `component.ts`, `lazy-live-surface.ts`, `live-surface.ts`,
`gateway/registration.ts`, `tools/gateway-services.ts`, `control/local-session-host.ts`.
mhc counterparts: `crates/omo/components/maho-omo-gateway/src/thread/` (`native.rs`, `registration.rs`,
`component.rs`), `crates/omo/components/maho-omo-remote/src/thread_host.rs`, `crates/omo/maho-omo/src/component_list.rs`,
`crates/maho-cli/src/cli/omo_mount.rs`.

## Summary

Counts computed from the rows below.

| verdict | rows |
|---|---:|
| port | 14 |
| ported | 0 |
| n/a | 1 |
| gap | 1 |
| blocked | 0 |
| **total** | **16** |

## Scope

The thread tools the v5.1.84 tracks ported were reachable only from tests: an ordinary mhc session (interactive, `-p`,
or a session of the multi-session host) registered `thread_send` alone, and only with a device service on the device,
because `native.rs` filtered `component.tools` and gave the component inert ports (admission "native host admission is
not connected", resolve `host_unavailable`, `GatewayEndpointPort::inert()`, no host, no session path). Upstream's
`createThreadComponent` reads all of them off `pi`. This track wires them the way upstream does: the engine side is a
binding (`NativeThreadHost`: the wire to this device's session hosts and the session's own `pi.session`) that the
`maho-omo` composition root takes in `OmoComponentOptions` and `maho-cli` binds per session from `TaskParent`, with
the maho-rpc wire and the maho-core adapter in `maho-omo-remote`, which already depends on the gateway, maho-rpc and
maho-core.

## Rows

| upstream | verdict | mhc location or reason |
|---|---|---|
| `thread/component.ts` `createThreadComponent`: `host = createLazyLiveThreadSurface(pi)` | port | `maho-omo-gateway/src/thread/native.rs` builds `LiveThreadSurface` over the wire the host binds (`NativeThreadHost.wire`, socket + transport) at `session_start`; `maho-omo-remote/src/thread_host.rs` `native_thread_host` binds the shared host's socket (`RPC_SOCKET`, then `<agentDir>/rpc/rpc.sock`, `sync_status::operator_socket`). The lazy shell is not needed: building the surface does no I/O. |
| `thread/live-surface.ts` `requestFrame` over senpi-task `withRpcSocket` + `authenticateRpcHost` (an `rpc_host` endpoint) | port | `maho-omo-remote/src/thread_host.rs` `HostTransport` (`RpcTransport`): one authenticated connection per request (`maho_rpc::host_probe::authenticated_host_socket`, observing `get_protocol_info` first); a fenced command's `attach_session`, the command and `release_attachment` share the attach's connection, as upstream runs them in one `withRpcSocket`. A missing or refusing socket is `host_unavailable:<socket>`, an identity refusal keeps its code (`auth_required:<socket>`). |
| `thread/component.ts` `registerThreadTools(pi, …)` + `tools/extension-tools.ts` `registerExtensionTools` | port | `component.rs` `register_tools` registers every `component.tools` entry (no subset) and the store-declared extension tools, sharing `extension_tools` with `tools_with_extensions`; `native.rs` keeps the device-service decoration of `thread_send` (`with_remote_thread_tools`). Before: only `thread_send`, only with a device service. |
| `thread/tools/gateway-services.ts` `createGatewayServices` (`resolve` over the send view's address book, `endpoints = host.gateway`) | port | `native.rs` `address_book_resolve` (`list_view`, nothing live is no failure, `book_of` + `send_address_book` + `resolve_from_entries`); `endpoints` = `LiveThreadSurface::gateway()` (wake under the attachment fence, `release_session`, liveness). |
| `thread/gateway/registration.ts` (`SessionControlActionsPort`, `controlSessionOf`, `createControlEndpointRegistrant`) | port | New `registration.rs`: the structural `pi.session` port, `session_runtime_port` (gate, admission in senpi's own field names `deliverAs`/`whenIdle`, ledger), `register_control_endpoint` (header first, drain behind the recorded incarnation, `endpoint_kind_of`, first pass) and `ControlEndpoint::release`. `native.rs` registers off the `session_start` path and releases at `session_shutdown`; `maho-omo-remote` `SessionControl` adapts core `GenerationSessionControlActions` (`AgentSession::session_control()`), bound from `TaskParent` by `maho-cli` `omo_mount.rs`. |
| `thread/component.ts` `registerControlEndpoint`: phase (`session_before_compact`/`session_compact`, `tool_execution_start`/`_end` of a waiting question) | port | `registration.rs` `SessionActivity` + `waits_for_answer`: compacting from `ctx.is_compacting_fn`, idle from `ctx.is_idle_fn`, open questions from `ToolExecutionStart/End` in `native.rs`. |
| `thread/component.ts` run context (`agent_start`, `message_start`, `message_end`, `agent_settled`: the reply allowance, #13042) | port | `component.rs` `note_agent_start`/`note_message_start`/`note_message_end`/`note_settled`, forwarded by `native.rs` (until now only `register_session_tools` wired them, and no production host mounted it). |
| `thread/component.ts` `callerName: pi.getSessionName`, the registrant's `session.notify` | port | `native.rs`: `caller_name` reads `SessionManager::get_session_name`, `notify` shows the queued notice through `ctx.ui.notify(…, Info)`, `session_path` is the session file. |
| `omo-senpi` `extension/index.ts` hands `pi` (the session surfaces) to each component | port | mhc assembly: `maho-omo` `OmoComponentOptions::thread_host` (default binds nothing) passed to `NativeThreadComponent { host }` in `component_list.rs`; `maho-cli` `omo_mount.rs` binds it per session from `TaskParent`, so interactive, `-p` and every session of the multi-session host get it. |
| `thread/endpoint-registry.ts` `listRegistryEndpoints` in `listView` (host shards `i-*`/`p-*`, terminal `tui` endpoints) | port | `crates/omo/components/maho-omo-gateway/src/thread/endpoint_registry.rs` reads the registry (host shards `i-*`/`p-*`, terminal `tui` endpoints) and `live_surface.rs` merges it into `list_view` through an async enumeration with the upstream cache and shared-enumeration rules; tests `tests/endpoint_registry_tests.rs`, `tests/live_surface_endpoints_tests.rs`. A session a terminal or task-host shard holds is now listed from the registry, not only from its file. |
| `thread/live-surface.ts` `dialFor` on a `tui` endpoint (`<socket>.secret`, 32 bytes before the first request) | port | `crates/omo/components/maho-omo-remote/src/thread_host.rs` `wire::open` dials a `tui` control socket (`t-<16 hex>.sock`) by reading `<socket>.secret` and sending its 32 bytes before the first request; `tests/tui_dial.rs` checks the byte order against a fake endpoint. |
| `thread/component.ts` `startWake` (wake runtime and delivery consumer at `session_start`; cold wake on the operator daemon) | gap | Ported: `wake/carrier_connection.rs` (the `CarrierHostConnection` socket half over the authenticated host connection), `wake/host_ensure.rs`, and `wake_wiring.rs` + `native.rs`, which start the process wake at `session_start` behind upstream's mode/CI/task-child/engine gates; tests in both files. Still a gap: the engine host ensure is a no-op (the host spawn belongs to `maho-cli` `mhc host ensure`), the delivery consumer is not started (the device-service delivery port `consumer/device-service-port.ts` is not ported, so the wake runs with no delivery port), `cold_wake_on_operator_daemon` exists but nothing calls it, and the wake is not stopped at `session_shutdown`. |
| `thread/component.ts` `importLegacyMailbox` (pre-gateway `thread_send` mailbox) | n/a | No Rust counterpart: mhc never wrote the pre-gateway mailbox and the Rust store carries only the `legacy_mailbox_directories` option, no importer. |
| `thread/component.ts` `settleGracefulClose` on `session_shutdown { closed: true }` (omo#14313) | port | Consumer: `thread/component.rs` registers a `SessionShutdown` handler that calls `settle_graceful_close` only when `closed` is true. Event: `maho_ext_api::SessionShutdownEvent.closed`, set from `AgentSession::closed_on_request`. Producer: `crates/maho-rpc/src/session_graceful_close.rs` and `session_unload.rs` `close_gracefully` (`unload_session { graceful, waitMs }`) record `session-closed` and mark the session closed only after a drain; a plain or forced unload leaves `closed` false; tests in `session_unload.rs`. Not ported: `holdForClose`/`releaseCloseHold` are no-op trait seams (mhc has no closing engine-turn hold), the interactive producer (`session-control-close.ts`; `maho-interactive` has no close command), and the drain re-reads the busy list on a 5 ms tick because the session has no event seam. |
| `thread/component.ts` `registerRemoteMonitorTargets(pi, remote)` (monitor `type: "thread"` targets) | port | `maho-ext-api` `rpc_remote_source.rs` (the `register_rpc_remote_source` seam, latest registration wins), `maho-ext-terminal` (remote target kind and `RpcRemoteFeed` in the rpc watch), gateway `thread/remote/monitor_source.rs` (`create_remote_monitor_source` over `RemoteSessionsPort::watch`, `<device>/*` listing, target resolution) registered from `thread/component.rs` when the agentd-backed remote surface exists; tests in `monitor_source.rs`. A remote watch does not resume its cursor after a restart (`saved: None`). |
| `thread/live-surface.ts` `requestFrame` `openedName`: `set_session_name` on the connection `open_session` opened | port | `RpcTransport::conversation` (`attachment_fence.rs`) keeps one connection for `open_session` and its follow-ups; `live_surface.rs` `open_session` sends `set_session_name` on that connection, and `thread_host.rs` `wire::conversation` holds it for the conversation; test `conversation_runs_first_and_followup_on_the_same_transport`. |

## Tests

- `crates/omo/components/maho-omo-gateway/tests/native_thread_host_tests.rs`: a real session through the extension host
  (`maho-test-support` `FauxSession` with the component as a native extension) registers every thread tool, then
  `thread_create` -> `thread_list` -> `thread_read`, `thread_create { fork_from }` (the host sees `forkFrom`) and
  `thread_send` (resolved through the address book, a fenced `wake` on the target's routing id) against an in-memory
  session host behind the bound wire; and the receiver half: the control endpoint registered with the session's
  `pi.session` (inbox directory, published endpoint row), a queued delivery admitted through the registered drain with
  senpi's field names. No sleeps: the first drain pass is awaited through the ledger read every pass starts with.
- `crates/omo/components/maho-omo-gateway/src/thread/registration.rs` unit tests: the admission input's wire names, the
  wake reason mapping, the terminal socket name rule, an unreadable gate, the waiting-question rule.
