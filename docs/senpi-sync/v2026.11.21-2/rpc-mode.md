# senpi 59d58cee5..v2026.11.21-2: `rpc-mode` track ledger

Every row of `/tmp/omo-mig2/tracks/rpc-mode.tsv` (176 rows) judged against `jc01rho/mhc` `main` and ported in this track where applicable.

- Verdict summary: **port**: 11, **ported**: 48, **n/a**: 117 (total: 176)

| senpi PR | verdict | mhc location or reason |
|---|---|---|
| `c813d412ef` | n/a | sync merge of origin/main; its content is covered by the individual PR rows in the track ledgers |
| `f6c38aa4ea` | n/a | sync merge of origin/main; its content is covered by the individual PR rows in the track ledgers |
| `8f7ecd5b37` | n/a | sync merge of origin/main; its content is covered by the individual PR rows in the track ledgers |
| `de379c0245` | n/a | sync merge of origin/main; its content is covered by the individual PR rows in the track ledgers |
| `27c2f335ca` | n/a | sync merge of origin/main; its content is covered by the individual PR rows in the track ledgers |
| `e6c92b6fdc` | n/a | sync merge of origin/main; its content is covered by the individual PR rows in the track ledgers |
| `29eedcbe7c` | n/a | sync merge of origin/main; its content is covered by the individual PR rows in the track ledgers |
| `2282223885` | n/a | sync merge of origin/main; its content is covered by the individual PR rows in the track ledgers |
| `51e3aba974` | n/a | sync merge of origin/main; its content is covered by the individual PR rows in the track ledgers |
| #4271 (`9fcc41844a`) | ported | already in mhc (round-1 PR #35): `crates/maho-interactive/src/{attach,attach_client,attach_dialogs,record_renderer,run_attached_tui}.rs`, `crates/maho-cli/src/cli/attach_*.rs`, `crates/maho-rpc/src/{session_registry,worker_session_registry}.rs` (`retain_on_disconnect`) |
| #4293 (`800b12aab2`) | ported | already in mhc (round-1 PR #39): `crates/maho-rpc/src/host_gc.rs` (`win32_socket_silence`, `credential_unavailable`, `endpoint_in_use_by`, `judge_evidence`) |
| #4294 (`d8d0bbca8b`) | ported | already in mhc (round-1 PR #39): `crates/maho-socket-secret/src/lib.rs` and `crates/maho-rpc/src/socket_transport.rs` (regular-file check, symlink refusal) |
| #4369 (`32c268155b`) | ported | already in mhc (round-1 PR #39): `crates/maho-socket-secret/src/lib.rs` and `crates/maho-rpc/src/socket_transport.rs` (staging file + `link(2)` first-publisher-wins, staging cleanup) |
| #4371 (`2a5470e119`) | ported | already in mhc (round-1 PR #39): `crates/maho-socket-secret/src/lib.rs` and `crates/maho-rpc/src/socket_transport.rs` (serialized repair under `ownership_safe_lock`) |
| #4414 (`5c0460010c`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #4420 (`b0d190e66f`) | ported | already in mhc: `crates/maho-rpc/src/{host_lifecycle_activity,host_run_activity,session_event_writer,session_open_turns,session_relocation}.rs` and `crates/maho-ext-host/src/handler_budget.rs` |
| #4425 (`39311490c4`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #4445 (`88809eeae3`) | port | ported in this PR: `crates/maho-rpc/src/login_prompts.rs` (`RpcLoginPromptCallbacks::on_progress` emits `auth_login_progress` + unit test) |
| #4519 (`c10a64f1ed`) | n/a | depends on unported senpi runtime surface (fix/4381-per-spawn-caller-env); not applicable to mhc's native RPC host architecture |
| #4521 (`df6aa9fdae`) | ported | already in mhc: `crates/maho-rpc/src/{compact_content_index,compact_content_store,compact_index_compactor,compact_segment_storage}.rs` |
| #4526 (`ded7e81c02`) | n/a | depends on unported senpi runtime surface (feat/engine-trust-marker); not applicable to mhc's native RPC host architecture |
| #4545 (`ca996090a4`) | ported | already in mhc: `crates/maho-rpc/src/{compact_content_store,compact_paged_tree,compact_segment_storage,compact_spool_storage,compact_storage_io}.rs` |
| #4557 (`7e4d73852e`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #4568 (`b3b5b902b7`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #4589 (`2681fb7659`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4590 (`c82599cc0a`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4591 (`f876881b16`) | port | ported in this PR: `crates/maho-rpc/src/compact_content_index.rs` (`IndexState::ready` queue) and `crates/maho-rpc/src/compact_index_compactor.rs` (readiness-ordered selection) |
| #4593 (`7c1c72e6be`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4598 (`e699f0ac82`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #4606 (`1c5f074bba`) | port | ported in this PR: `crates/maho-rpc/src/compact_paged_tree.rs` (`ROUND_CACHE_BYTES = 1024 * 1024` round-local LRU budget) |
| #4615 (`cb3c0df8fd`) | ported | already in mhc: `crates/maho-rpc/src/host_gc_pass.rs` (`passes_running`, `schedule_opportunistic_host_gc`, `settled_opportunistic_host_gc`) |
| #4618 (`e2d473f786`) | ported | already in mhc: `crates/maho-rpc/src/{host_daemon_env,host_lifecycle_launch,internal_bootstrap}.rs` (`INTERNAL_KEY_STDIN_ENV` cleared before child spawn) |
| #4635 (`85e6d0c6ba`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4690 (`b600a95360`) | ported | already in mhc: `crates/maho-rpc/src/host_lifecycle_drain.rs` (`watch_win32_child_identity` reads start time once at registration and backs off on inconclusive liveness) |
| #4693 (`0cd750285a`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4694 (`2541ddec2a`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4698 (`be0dbdcf4a`) | ported | already in mhc: `crates/maho-rpc/src/host_lifecycle.rs` (30 s child listener readiness timeout) |
| #4704 (`713b5c5618`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4710 (`bcbe9d257e`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4715 (`49a30f1ebe`) | ported | already in mhc: `crates/maho-rpc/src/internal_bootstrap.rs` (`bootstrap` restores `/dev/null` on fd 0 before returning on both success and failure) |
| #4720 (`16fd476dad`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4754 (`79e83c91ef`) | n/a | depends on unported senpi runtime surface (train/login-drop-nonsec-1); not applicable to mhc's native RPC host architecture |
| #4755 (`efe6b61702`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4771 (`a56787728b`) | ported | already in mhc: `crates/maho-rpc/src/{host_lifecycle_launch,host_lifecycle,host_successor,mcp_presenter_bootstrap}.rs` |
| #4775 (`38ac52b4b6`) | n/a | depends on unported senpi runtime surface (feat/extension-rpc-owner-context); not applicable to mhc's native RPC host architecture |
| #4810 (`de5df4d0f8`) | ported | already in mhc: `crates/maho-rpc/src/host_lifecycle.rs` and `crates/maho-rpc/src/host_daemon_paths.rs` (`create_generation_directory` before child/owner setup) |
| #4824 (`f75cc60e1c`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4825 (`7b2eca7d94`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4834 (`447df0708d`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4840 (`3b2bb924b3`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4843 (`d1d35afeeb`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #4847 (`520ef7f836`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4857 (`cc3bbd17c0`) | ported | already in mhc: `crates/maho-rpc/src/internal_bootstrap.rs` (`arm_key_deadline` watcher thread) |
| #4877 (`b37fb7863c`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4879 (`fecca3f8ca`) | n/a | depends on unported senpi runtime surface (fix/desktop-4203-account-reset-main); not applicable to mhc's native RPC host architecture |
| #4912 (`5071aba30e`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4949 (`f512c6f57b`) | n/a | senpi dynamic `method-classes*.ts` table and runtime inbound schema validator (`docs/fork/senpi-rpc-delta.md`: mhc validates inbound commands through typed serde `RpcCommandBody` deserialization) |
| #4953 (`1fc9183a04`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4957 (`a32919327d`) | n/a | senpi dynamic `method-classes*.ts` table and runtime inbound schema validator (`docs/fork/senpi-rpc-delta.md`: mhc validates inbound commands through typed serde `RpcCommandBody` deserialization) |
| #4987 (`ae94fd1039`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #4993 (`38530e8acc`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #5087 (`4d3c2a91a0`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #5110 (`edd2d604c3`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #5120 (`33da98b668`) | ported | already in mhc (round-1 PR #35): `crates/maho-tui/src/components/input.rs`, `crates/maho-ext-api/src/secret_input.rs`, `crates/maho-core/src/extensions/secret_input.rs`, `crates/maho-interactive/src/components/extension_input.rs` |
| #5122 (`f5741cbcee`) | n/a | depends on unported senpi runtime surface (fix/4385-kimi-api-key-region); not applicable to mhc's native RPC host architecture |
| #5212 (`d1f6ec2420`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #5283 (`caa3c87021`) | ported | already in mhc: `crates/maho-rpc/src/host_ensure_liveness.rs` and `crates/maho-rpc/src/socket_ownership.rs` (`stat_socket_identity` non-socket refusal) |
| #5302 (`467e10f487`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #5304 (`6f5fb81b7e`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #5318 (`42550cdb56`) | ported | already in mhc: `crates/maho-rpc/src/host_lifecycle_proxy.rs` (`prepare_socket_path` symlink refusal) |
| #5333 (`b899881fd2`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #5347 (`28c27fd796`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #5368 (`75ed1b1491`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #5372 (`c46d8afa76`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #5379 (`9fb42750f6`) | ported | already in mhc: `crates/maho-rpc/src/{connection_handler,effect_commands,effect_record}.rs` (`set_model` effect ledger command) |
| #5388 (`22d6d808f4`) | ported | already in mhc: `crates/maho-rpc/src/{connection_handler,connection_question_bridge}.rs` (question UI requests scoped per connection/session) |
| #5389 (`b2c54a166c`) | n/a | depends on unported senpi runtime surface (fix/abort-wire-order); not applicable to mhc's native RPC host architecture |
| #5394 (`db54b35da7`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #5404 (`f963cec46e`) | ported | already in mhc: `crates/maho-rpc/src/{host_ensure,host_gc,host_lifecycle_proxy,host_public_clients,socket_ownership}.rs` |
| #5420 (`619f6e7575`) | ported | already in mhc (round-1 PR #39): `crates/maho-rpc/src/{session_extension_ui_requests,connection_question_bridge,connection_handler}.rs` (single `&mut` behind connection lock guarantees first-answer-wins; pinned by unit tests) |
| #5438 (`62a8d3ffbe`) | n/a | loopless worker pool and remote hand selection (`loopless-*`, `hand-selection-*`): mhc has no loopless worker isolate or remote hand plane (`docs/fork/senpi-agent-tools-delta.md`) |
| #5457 (`d64b8f87fe`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #5479 (`8156166e9e`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #5607 (`1a06467881`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #5628 (`8dbd0fbac7`) | port | ported in this PR: `crates/maho-rpc/src/host_observe_request.rs` (`observing_hold_change`) and `crates/maho-rpc/src/host_client_occupancy.rs` (`ClientOccupancy::held` observer hold lease + unit test) |
| #5637 (`937428662e`) | ported | already in mhc: `crates/maho-rpc/src/rpc_command_surface.rs` (`RpcCommandInput` is a typed struct carrying only `name`, `description`, `source_info`, so internal `allowed_set` is never serialized onto the wire) |
| #5651 (`35ed878291`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #5699 (`cc995dedc7`) | n/a | depends on unported senpi runtime surface (fix/5698-account-reset-capability); not applicable to mhc's native RPC host architecture |
| #5705 (`72ef54c4fc`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #5734 (`158191220d`) | n/a | depends on unported senpi runtime surface (feat/s37-quiet-takeover); not applicable to mhc's native RPC host architecture |
| #5751 (`7477d96c6a`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #5763 (`d45c9e173c`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #5767 (`73f7c17bc5`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #5779 (`5aba1da424`) | ported | already in mhc: `crates/maho-rpc/src/{automatic_input_gate,client_input_handler,rpc_input_validation,rpc_session_state,rpc_types}.rs` and `crates/maho-core/src/{pending_input,turn_conflict}.rs` (`expectedTurnId` / `TurnConflictError`) |
| #5787 (`afb2c379a0`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #5805 (`1581c9da23`) | ported | already in mhc: `crates/maho-rpc/src/session_relocation.rs` (`RpcSessionRelocation::ask` inserts pending entry before sending on channel in async future) |
| #5863 (`2081ceaea2`) | port | ported in this PR: `crates/maho-rpc/src/session_relocation.rs` (`RelocationRunEvent::AgentSettled` re-checks `stopped_by_user()` so a late user abort drops the scheduled move + unit test) |
| #5888 (`4789fd683b`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #5895 (`c9bba00204`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #5906 (`7a25deba4e`) | ported | already in mhc: `crates/maho-rpc/src/client_input_handler.rs` and `crates/maho-core/src/session_manager.rs` (`SessionMovedAwayError` / `session_moved_away` code) |
| #5920 (`21eb9dcbfd`) | ported | already in mhc: `crates/maho-rpc/src/{host_capabilities,session_move_commands,session_move_wire}.rs` |
| #5923 (`f4dc3acc33`) | ported | already in mhc (round-1 PR #39): `crates/maho-agent/src/chain_count.rs`, `crates/maho-rpc/src/{chain_admission,connection_handler}.rs`, `crates/maho-core/src/settings_manager.rs` |
| #5943 (`1e492bc651`) | n/a | depends on unported senpi runtime surface (feat/5933-session-model-scope-capability); not applicable to mhc's native RPC host architecture |
| #5946 (`b27ea5e092`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #5949 (`4b4126c031`) | n/a | loopless worker pool and remote hand selection (`loopless-*`, `hand-selection-*`): mhc has no loopless worker isolate or remote hand plane (`docs/fork/senpi-agent-tools-delta.md`) |
| #5959 (`634678109d`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #5972 (`ca06784ede`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #5985 (`24fce4d1a3`) | ported | already in mhc: `crates/maho-rpc/src/{host_lifecycle_launch,multi_session_host}.rs` (`PUBLIC_SOCKET_ENV` passed on every platform) |
| #6020 (`b85376e904`) | ported | already in mhc: `crates/maho-rpc/src/{host_lifecycle_activity,host_lifecycle}.rs` (`IdleExitDecider` and startup handoff window) |
| #6070 (`c024c92dd8`) | n/a | senpi #4272 compact recovery catalog / seeded block reducer / compact call table / event lease / `@senpi/client` materializer stack (`docs/fork/senpi-rpc-delta.md`: mhc carries only the base `compact_*` storage/index modules, not the compact recovery wire protocol) |
| #6076 (`f50e864e9d`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #6098 (`3490141d08`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #6101 (`db6d44fba8`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #6126 (`acf28a6208`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #6135 (`070512c838`) | ported | already in mhc: `crates/maho-rpc/src/{client_admission_record,client_admissions,client_input_handler}.rs` (`admitted.turnId` on prompt admission) |
| #6138 (`29e2e1505a`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #6144 (`bffb18e0f9`) | ported | already in mhc: `crates/maho-rpc/src/host_lifecycle_shutdown.rs` (`perform_shutdown` logs original failure once and `reap_child_after_failure` logs only recovery failure) |
| #6148 (`638d3c5b40`) | n/a | depends on unported senpi runtime surface (fix/rpc-host-boot-extensions-private-fds); not applicable to mhc's native RPC host architecture |
| #6159 (`d007e5fb29`) | n/a | senpi dynamic `method-classes*.ts` table and runtime inbound schema validator (`docs/fork/senpi-rpc-delta.md`: mhc validates inbound commands through typed serde `RpcCommandBody` deserialization) |
| #6166 (`9662d789ec`) | ported | already in mhc (round-1 PR #32): `crates/maho-core/src/messages.rs` and `crates/maho-rpc/src/session_open_turns.rs` (keep `turnId` off `agent_start`/`agent_settled` on legacy wire) |
| #6174 (`00a6b766d5`) | ported | already in mhc: `crates/maho-rpc/src/rpc_types.rs` (`RpcCommandBody::{Prompt,Steer,FollowUp}` require `message: String` at serde deserialization boundary) and `crates/maho-rpc/src/pause_command_validation.rs` |
| #6177 (`4e5c695075`) | n/a | remote desktop project-file / folder-MCP / folder-skill / worktree-trust RPC verbs (`project-*-commands.ts`): belong to the delegated remote-desktop project editor surface not carried in `maho-rpc` |
| #6193 (`01e746020a`) | ported | already in mhc: `crates/maho-rpc/src/{host_handover_parked,host_handover_reopen,session_attachment_commands,session_event_writer,session_status}.rs` |
| #6195 (`d0bae9336b`) | n/a | depends on unported senpi runtime surface (fix/6190-bare-host-registers-endpoint); not applicable to mhc's native RPC host architecture |
| #6204 (`920a085089`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #6206 (`5b6943e7e6`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #6211 (`bb2a3ca52b`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #6221 (`c88ab9915d`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #6240 (`ba780418db`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #6251 (`1234211db5`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #6307 (`f75b592c51`) | n/a | remote desktop project-file / folder-MCP / folder-skill / worktree-trust RPC verbs (`project-*-commands.ts`): belong to the delegated remote-desktop project editor surface not carried in `maho-rpc` |
| #6316 (`34b90c4e2d`) | n/a | unported senpi-only surface (Merge pull request #6316 from code-yeongyu/fix/12951-host-child-trust-handoff) |
| #6340 (`76822d7047`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #6347 (`2031def19f`) | ported | already in mhc (round-1 PR #32): `crates/maho-core/src/input_origin.rs` (same-account delegated connections follow host turn origin) |
| #6350 (`def2323faf`) | n/a | depends on unported senpi runtime surface (feat/6344-park-claim); not applicable to mhc's native RPC host architecture |
| #6372 (`85156eeed1`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #6416 (`5278b32a7e`) | ported | already in mhc: `crates/maho-rpc/src/session_move_commands.rs` |
| #6418 (`02c4de9eda`) | port | ported in this PR: `crates/maho-rpc/src/session_status.rs` (`SessionStatusEntryView::last_activity_at`), `crates/maho-rpc/src/session_status_source.rs`, `crates/maho-rpc/src/session_status_stream.rs` (`last_change_of` uses `last_activity_at` + unit test), `crates/maho-rpc/src/multi_session_host.rs` |
| #6423 (`d6edfb995b`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #6434 (`e5c8bd61e1`) | n/a | win32 native socket-secret N-API loader (`socket-secret-native.ts` / `socket-secret-publish.ts`), unwired upstream (`crates/maho-rpc/parity.d/5179r.md`) |
| #6450 (`a1175ecf0e`) | ported | already in mhc (round-1 PR #39): `crates/maho-rpc/src/{host_session_find,multi_session_host,host_capabilities}.rs` (`find_session` capability and handler) |
| #6456 (`f823845e31`) | n/a | depends on unported senpi runtime surface (fix/move-status-landing); not applicable to mhc's native RPC host architecture |
| #6532 (`7f3c2aabe6`) | n/a | remote desktop project-file / folder-MCP / folder-skill / worktree-trust RPC verbs (`project-*-commands.ts`): belong to the delegated remote-desktop project editor surface not carried in `maho-rpc` |
| #6557 (`e967a7d690`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #6659 (`74dc6f5bd7`) | n/a | remote desktop project-file / folder-MCP / folder-skill / worktree-trust RPC verbs (`project-*-commands.ts`): belong to the delegated remote-desktop project editor surface not carried in `maho-rpc` |
| #6664 (`8a99f212fa`) | n/a | comment/doc wording only in `project-expected-cwd.ts` |
| #6672 (`e32b301603`) | port | ported in this PR: `crates/maho-rpc/src/session_status.rs` (`cause_of_record` maps `session_info_changed` to `"rename"` + unit test) |
| #6675 (`08009f7998`) | ported | already in mhc (round-1 PR #38): `crates/maho-rpc/src/{rpc_types,pause_command_validation,session_control_actions_data,connection_handler}.rs` (`interrupt { background: "stop" }`) |
| #6677 (`31686a24bd`) | ported | already in mhc (round-1 PR #32): `crates/maho-core/src/{session_archived,session_manager}.rs` (`session_archived` flag cached O(1)) |
| #6684 (`9b0acf713c`) | n/a | depends on unported senpi runtime surface (fix/close-parked-permission-prompt); not applicable to mhc's native RPC host architecture |
| #6755 (`1d6bb40b9c`) | ported | already in mhc (round-1 PR #39): `crates/maho-machine-ops` (scope-confined file operations, glob root prefix, symlink/`..` escape refusal; RPC `machine_op` verb excluded per `crates/maho-rpc/parity.d/5179r.md`) |
| #6768 (`db2db3bbbb`) | ported | already in mhc (round-1 PR #32): `crates/maho-core/src/{session_settled,session_manager}.rs` (`session_settled` flag and `ThreadSettledBy` validation) |
| #6809 (`986708ba27`) | n/a | `machine_op` RPC verb and remote hand wake terminal restore (`rpc/machine-op/*`, `hand-wakes.ts`): excluded per `crates/maho-rpc/parity.d/5179r.md` and `docs/fork/senpi-agent-tools-delta.md` (mhc has no delegated `machine_op` verb or remote hand plane) |
| #6823 (`614a5cb60e`) | n/a | remote desktop project-file / folder-MCP / folder-skill / worktree-trust RPC verbs (`project-*-commands.ts`): belong to the delegated remote-desktop project editor surface not carried in `maho-rpc` |
| #6831 (`6d462f72ff`) | n/a | depends on unported senpi runtime surface (fix/6684-unload-parked-only-when-all-work-waits); not applicable to mhc's native RPC host architecture |
| #6896 (`f41c21c92d`) | n/a | remote desktop project-file / folder-MCP / folder-skill / worktree-trust RPC verbs (`project-*-commands.ts`): belong to the delegated remote-desktop project editor surface not carried in `maho-rpc` |
| #6923 (`1806a2f129`) | ported | already in mhc: `crates/maho-core/src/{agent_session,edited_assistant_message,lifecycle_edit_leaf}.rs` (`expected_leaf_id` checked via `assert_expected_leaf` + `lifecycle_edit_leaf`) |
| #6925 (`c51802622c`) | n/a | loopless worker pool and remote hand selection (`loopless-*`, `hand-selection-*`): mhc has no loopless worker isolate or remote hand plane (`docs/fork/senpi-agent-tools-delta.md`) |
| #6954 (`541a235815`) | n/a | remote desktop project-file / folder-MCP / folder-skill / worktree-trust RPC verbs (`project-*-commands.ts`): belong to the delegated remote-desktop project editor surface not carried in `maho-rpc` |
| #6958 (`c4ecbbe846`) | n/a | remote desktop project-file / folder-MCP / folder-skill / worktree-trust RPC verbs (`project-*-commands.ts`): belong to the delegated remote-desktop project editor surface not carried in `maho-rpc` |
| #6971 (`40166a2a4b`) | ported | already in mhc (round-1 PR #39): `crates/maho-rpc/src/{session_release_interrupt,session_release,session_release_adapter,session_unload,effect_record,effect_ledger}.rs` (`abort_and_settle` returns `dropped.deliveries`) |
| #6989 (`807b0acef3`) | ported | already in mhc: `crates/maho-rpc/src/host_handover_reopen.rs` (`reopen_handover_parked_sessions` retries `session_path_in_use` without counting against attempt limit while predecessor holds claim) |
| #7006 (`d662255a49`) | ported | already in mhc (round-1 PR #39): `crates/maho-rpc/src/{session_unload,effect_record,effect_ledger,effect_status,rpc_input_validation,rpc_types,host_capabilities,multi_session_host}.rs` (`unload_session { deliveries: "refuse" }`) |
| #7008 (`2d7f5db635`) | n/a | TS knip unused-export tag only on `UNLOAD_REFUSE_DELIVERIES_CAPABILITY` (capability ported in PR #39) |
| #7014 (`404530c838`) | n/a | delegated connection enforcement, D11 hosted account / HPKE secret-sealed presenter authority, or delegated external-message admission (`docs/fork/senpi-rpc-delta.md`: mhc has no delegated connections or hosted account authority in local mode) |
| #7021 (`3226c4f9eb`) | port | ported in this PR: `crates/maho-rpc/src/host_session_find.rs` (`session_files` scans root `.jsonl` files directly under `sessions/` as well as subfolders + unit test) |
| #7027 (`2b063d7e6b`) | n/a | depends on unported senpi runtime surface (fix/session-status-retry-waiting); not applicable to mhc's native RPC host architecture |
| #7039 (`54e5fb7abe`) | port | ported in this PR: `crates/maho-rpc/src/host_occupancy_watch.rs` (new module + unit test), `crates/maho-rpc/src/host_observe_request.rs` (`WATCH_HOST_OCCUPANCY`, `HOST_OCCUPANCY_RECORD`, `is_host_occupancy_watch`, `host_occupancy_holders`), `crates/maho-rpc/src/host_client_occupancy.rs` (`occupancy`, `is_attached`), `crates/maho-rpc/src/host_lifecycle.rs` (`IdleExitDecider::restart`), `crates/maho-rpc/src/host_lifecycle_activity.rs` (`session_holders` cap + unit test) |
| #7041 (`792f53378f`) | n/a | depends on unported senpi runtime surface (fix/7020-handover-listing); not applicable to mhc's native RPC host architecture |
| #7046 (`87d99127a1`) | n/a | `unload_session { graceful: true }` (`session-graceful-close.ts`): excluded per `crates/maho-rpc/parity.d/5179r.md` and `docs/fork/senpi-rpc-delta.md` (requires session closing hold and `session-closed` record not in mhc) |
| #7047 (`bd02f31760`) | port | ported in this PR: `crates/maho-core/src/engine_build_identity.rs` (`FIRST_CALVER_YEAR`, `comparable_engines`, `engine_identity_of`, `compare_engine_ordinal` CalVer guard, `older_engine_detail` + unit tests), `crates/maho-rpc/src/host_decision.rs` (`newer` uses `compare_engine_ordinal` + unit test), `crates/maho-rpc/src/host_ensure_client.rs` (`report_uncomparable_engine`), `crates/maho-rpc/src/host_ensure.rs` |
| #7063 (`6f84ae24ed`) | ported | already in mhc (round-1 PR #32): `crates/maho-core/src/{host_restart_continuation,session_lifecycle,agent_session,engine_turn_hold}.rs` |
| #7077 (`70ee605d2c`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #7105 (`1d2ec22594`) | port | ported in this PR (drain overdue alarm & strict-transfer reservation slice): `crates/maho-rpc/src/host_crash_record.rs` (`is_host_crash` excludes `rpc-host-drain-overdue` + unit test), `crates/maho-rpc/src/host_lifecycle_drain.rs` (repeating `arm_grace`, `set_overdue`, `record_drain_overdue` + unit test), `crates/maho-rpc/src/host_lifecycle.rs`, `crates/maho-rpc/src/host_reservations.rs` (`SessionPathOwner::strict_transfer` + unit test); predecessor route table / systemd-run unit placement slices are n/a in mhc |
| #7143 (`35743c7dab`) | n/a | senpi TS test-harness / fixture / docs change only; no production runtime change |
| #7146 (`357e9b73c2`) | n/a | `predecessor-route-table.ts` / `predecessor-routes.ts` (`RouteTable::forget`): predecessor routing proxy subsystem is not carried in mhc |
