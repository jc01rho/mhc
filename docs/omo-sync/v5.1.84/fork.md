# omo v5.1.83..v5.1.84 sync ledger — `fork` track

Source range: `69a1863a457` (`v5.1.83`) .. `8dbccd12842f` (`v5.1.84`)
Upstream: #13031 (named senpi#7085 upstream) in `packages/omo-senpi/src/components/thread/` `control/local-open.ts`,
`control/local-session-host.ts`, `control/open-reply.ts`, `control/relay-session-host.ts`, `control/session-host.ts`,
`live-surface.ts`, `remote/cross-device.ts`, `tools/ports.ts`, and the tests `thread-create-fork.test.ts`,
`thread-remote-fixture.ts`, `thread-remote.test.ts`.
mhc counterparts: `crates/omo/components/maho-omo-gateway/src/thread/` (`control/`, `host_view.rs`, `live_surface.rs`,
`remote/`) and the engine, `crates/maho-rpc`.

## Summary

Counts computed from the per-commit rows below.

| verdict | rows |
|---|---:|
| port | 3 |
| ported | 0 |
| n/a | 0 |
| gap | 0 |
| blocked | 0 |
| **total** | **3** |

## Scope

Upstream forks on an engine that advertises `open_session_fork`, and the engine omo v5.1.84 bundles (senpi v2026.11.23)
does not advertise it; senpi's `main` (2026-11-27) has no fork-on-open either. mhc's own engine was the missing piece, so
this track builds it in `crates/maho-rpc` from the contract the omo tests pin. `open_session { forkFrom: <durable id> }`
finds the source among the host's own session files the way `find_session` does (`host_session_find.rs`
`find_fork_source`), copies it with `SessionManager::fork_from` (senpi `SessionManager.forkFrom`, as senpi's
`session-move/copy.ts` writes a copy) into the folder the open's cwd keeps its sessions in, under the open's durable id,
and opens that file; a fork that names no cwd starts in the source's folder. A source no file of the host carries is
refused `fork_source_not_local`, one with no single readable file `fork_source_not_found`; `forkFrom` with `sessionPath`
or `expectedHostInstance` is an invalid launch profile, and a fork whose open is refused has its file removed. The
capability `open_session_fork` is advertised in `get_protocol_info`.

On the omo side this device's leg forks; the leg on another device asks that device's engine for its capabilities and
refuses `fork_unsupported` when it does not fork, as upstream. The open on another device itself stays refused
`relay_session_host_unsupported`: `mhc-relayd` carries no remote `open_session`, so there is no reply for `openRefusal`'s
fork arm to read.

## Per commit

| upstream commit | verdict | mhc location or reason |
|---|---|---|
| `aef88019f8` fix: fork_from forks on an engine that advertises open_session_fork, on both legs; the engine's fork_source refusals answer not_found | port | `host_view.rs` `ThreadHost::capabilities`, `live_surface.rs` `LiveThreadSurface::capabilities`; `control/session_host.rs` `OPEN_SESSION_FORK`; `control/port.rs` `SessionHost::forks`; `control/local_session_host.rs` `forks`, `open_locally`; `control/open_reply.rs` `fork_refusal`; `control/create.rs` (the fork decision on this device); `control/plan_create.rs` (`fork_unsupported`, `relay_forks`, the source's durable id); `remote/ports.rs` `RemoteSessionsPort::endpoint_command`, `remote/closed_rows.rs`; `contracts_create_params.rs` (`fork_from` describes the fork). The engine: `maho-rpc` `multi_session_host.rs`, `host_session_find.rs`, `host_capabilities.rs`, `rpc_types.rs`. |
| `90dc3fef61` test: fork_from on an engine that advertises open_session_fork forks on both legs; a source that engine does not hold is refused typed | port | `tests/thread_create_tests.rs` `a_fork_on_an_engine_that_forks_opens_with_the_source_and_answers_a_new_thread`, `a_source_the_forking_engine_does_not_hold_is_not_found_with_the_engines_reason` (this device; the case that forks on b needs the remote open, the refused leg above). The fixture's fork answers are the faux host's `capabilities` and `open_failure`. |
| `07060a225b` test: a fork refused on b reads only b's capabilities | port | `tests/remote_list_and_close_tests.rs` `thread_create_fork_on_b_reads_only_bs_capabilities_and_opens_nothing_there` (only `get_protocol_info` reaches b; a b that forks stays refused). |

## Tests

- `crates/maho-rpc/src/multi_session_host.rs` `open_session_fork_from_opens_a_copy_of_a_session_this_host_holds`: the fork is a new file under the open's durable id with the source's entries and `parentSession`, the source is unchanged, the host lists the fork, an unknown source is `fork_source_not_local`, `forkFrom` with `sessionPath` is refused, and `open_session_fork` is advertised.
- `crates/maho-rpc/src/host_session_find.rs` `a_fork_source_is_the_one_file_carrying_the_id_and_anything_else_is_refused_typed`; `rpc_types.rs` `open_session_carries_the_fork_source_as_fork_from`; `host_capabilities.rs` (the advertised set).
- `maho-omo-gateway`: `control/open_reply.rs` (two `fork_refusal` cases), `control/local_session_host.rs` (`forks` from the capabilities, the fork refusal on open), and the integration tests named in the rows.

## Shared files

None: no `Cargo.toml`, `Cargo.lock`, `crates/omo/maho-omo/src/lib.rs` or `scripts/ci-shard.py` edit.
