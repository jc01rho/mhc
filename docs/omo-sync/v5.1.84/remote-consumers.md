# omo v5.1.83..v5.1.84 sync ledger — `remote-consumers` track

Source range: `69a1863a457` (`v5.1.83`) .. `8dbccd12842f` (`v5.1.84`)
Upstream paths: `packages/omo-senpi/src/components/remote/port/lookup-gate.ts`, `packages/omo-senpi/src/components/thread/remote/{remote-watch,session-state,watch-queue,remote-ops}.ts`, `packages/omo-senpi/src/components/thread/control/{relay-session-host,create,plan-create,open-reply,port}.ts`, `packages/omo-senpi/src/components/thread/worktree/{remote-worktree,worktree-rpc,thread-worktree,discard-worktree,failures}.ts`
mhc counterparts: `crates/omo/components/maho-omo-gateway/src/thread/{remote,control,worktree}/`

## Summary

| verdict | count |
|---|---:|
| port | 3 |
| ported | 0 |
| blocked | 0 |
| gap | 0 |
| n/a | 0 |
| **total** | **3** |

## Scope

Closes the last three blocked rows of the omo v5.1.84 migration (`thread-remote.md` #14748 and `25dc4d8220`, and
`thread-gateway.md` relay create recovery #14560 / #14569 / #14623 review M5) by porting the callers and state machines
over the tailnet `agentd` client (`crates/omo/components/maho-omo-gateway/src/thread/remote/agentd/`), tested against
both unit rigs and the in-process loopback TCP `FakeAgentd`.

## Per-PR verdicts

| upstream PR | verdict | mhc location or reason |
|---|---|---|
| #14748 lookup gate fence and admission rules, and remote session watch retrying a fence refusal (`4f4215f9f4`, `bd8df618cb`, `4051b7d173`, `2cb91cd067`, `de9066508b`, `049533bb09`, `e2abf877ad`) | port | `maho-omo-gateway/src/thread/remote/lookup_gate.rs` (`LookupGate`, `LookupStart`), `src/thread/remote/remote_watch.rs` (`open_session_watch`, `WatchLink`), `src/thread/remote/session_state.rs`, `src/thread/remote/watch_queue.rs`, `src/thread/remote/ports.rs` (`LookupFenceRefused`, `RemoteSessionsPort::{read,watch}`), `src/thread/remote/closed_rows.rs`, and `src/thread/remote/agentd/ports.rs` (`AgentdClient::lookup_gate`, `AgentdPorts::{lookup,lookup_alone,locate,session_command,watch}`). Tests: `src/thread/remote/lookup_gate_tests.rs`, `src/thread/remote/remote_watch_tests.rs`, `tests/remote_consumers_tests.rs`. |
| retain buffered output from the identified active send (`25dc4d8220`) | port | `maho-omo-gateway/src/thread/remote/remote_ops.rs` (`follow_remote`, `RemoteFollow`, `RemoteFollowLane`, `FollowRemoteOptions`, `anchor_matches`, `is_user_entry`), reading `AgentdFollow` and `mhc.session.read` via `AgentdPorts::{watch,read}` (`src/thread/remote/agentd/ports.rs`). Tests: `src/thread/remote/remote_ops_tests.rs` (all cases of `remote-ops.test.ts` plus orderings C/D/F of `handoff-reply-twice.test.ts`), `tests/remote_consumers_tests.rs`. |
| #14560, #14569, #14623 review M5 relay create recovery (`877fe926ec`, `cdc0aa9720`, `e264cb42af`, `4f15b62212`, `36eb5458bb`, `cf05fcd9cd`) | port | `maho-omo-gateway/src/thread/control/port.rs` (`SessionHost::{supports_adopt,adopt}`, `SessionAdoptRequest`, `SessionOpenRequest.worktree`), `src/thread/control/relay_session_host.rs` (`RelaySessionHost`, `create_relay_session_host`: `open`, `adopt`, `adopted`, `finish`, `held_under`, `unreachable` with #14623 review M5 confirmed-unload guard before `remove_remote_worktree`), `src/thread/remote/agentd/ports.rs` (`AgentdSessionHost`, `AgentdPorts::endpoint_command`), `src/thread/control/create.rs` (`kept_worktree`, `retries_elsewhere` adoption), `src/thread/control/plan_create.rs` (remote `worktree` allowed on the agentd leg, typed device resolution via `target_device`), `src/thread/control/create_folder.rs`, `src/thread/control/open_reply.rs` (`opened_engine_id`, `open_refusal`), and `src/thread/worktree/{thread_worktree,discard_worktree,worktree_rpc,remote_worktree,failures}.rs`. Tests: `tests/remote_consumers_tests.rs` (`thread-create-remote-unreachable.test.ts` and `thread-create-remote-lost-reply.test.ts` over `FakeAgentd`). |

## Server-side contract note (`omo-nyang` daemon)

The mhc client speaks the eight `mhc.*` op names of `AGENTD-CONTRACT.md` unchanged (`mhc.sessions.list`,
`mhc.session.locate`, `mhc.session.read`, `mhc.session.open`, `mhc.session.command`, `mhc.session.follow`,
`mhc.delivery.push`, `mhc.delivery.outcome`). For live end-to-end execution of remote create recovery and remote
worktrees against the `omo-nyang` daemon (`src/relay/mhc-ops.ts` / `src/relay/mhc-host.ts`), the server-side validator
needs:

1. `mhc.session.open` `launchArgs` (`mhc-ops.ts`): accept `durableSessionId` (so the far engine opens a created session
   under `createdThreadId(callerId, operationId)` for #14560/#14569 adoption), `name` and `initialSkills`, and return
   the engine's RPC handle as `routing_id` (or include it on `mhc.sessions.list` rows) when a session is unrouted.
2. `mhc.session.command` `RECORDS` allowlist (`mhc-ops.ts`): admit `set_session_name` and `extension_request`
   (`omo.thread.worktree`, or add dedicated `mhc.worktree.create` / `mhc.worktree.remove` ops) alongside the existing
   `unload_session` / `close_session` / `get_state` records.
