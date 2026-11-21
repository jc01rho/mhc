# SNP-02 (`snp-session`): `senpi` Core Session Lifecycle & Compaction Delta Mapping

## 1. Scope & Upstream Sources

| Upstream PR / Issue | Commit | Summary | `mhc` Target Path(s) |
|---|---|---|---|
| `senpi#7063` (`senpi#7020`) | `6f84ae24ed` (`efab055302`, `904a068bc3`) | Host restart continuation for live sessions, `HOST_DRAIN_HOLD_KEY`, and resume-window exemption for released turns | `crates/maho-core/src/host_restart_continuation.rs`, `crates/maho-core/src/session_lifecycle.rs`, `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/engine_turn_hold.rs` |
| `senpi#7080` (`senpi#7079`) | `67cdcbd455` | Restart continuation progress gate and `RESTART_CONTINUATION_LIMIT` crash-loop stop with hold reason data | `crates/maho-core/src/host_restart_continuation.rs`, `crates/maho-core/src/agent_session.rs` |
| `senpi#6677` | `31686a24bd` (`e3d4c9d381`, `c2af3ae305`) | Durable `session_archived` flag cached in O(1) on `SessionManager`, scoped by `sessionId` | `crates/maho-core/src/session_archived.rs`, `crates/maho-core/src/session_manager.rs`, `crates/maho-core/src/agent_session.rs` |
| `senpi#6768` | `db2db3bbbb` (`88b47cdfd2`, `4e760757c2`) | Durable `session_settled` (Done) flag with `ThreadSettledBy` validation cached in O(1) on `SessionManager` | `crates/maho-core/src/session_settled.rs`, `crates/maho-core/src/session_archived.rs`, `crates/maho-core/src/session_manager.rs`, `crates/maho-core/src/agent_session.rs` |
| `omo#14296` | `abce6764cd` (`1a2d7efd27`, `c33f7e5003`) | Reopen names the session's recorded absolute `cwd` immediately without probing the file path on the host loop | `crates/maho-core/src/moved_session_cwd.rs` |
| `senpi#6904` (`senpi#6906`) | `3d7f9fc0b8` (`1e6a51d7d2`) | Nested tool result with top-level `isError: true` counts as a failure in `toolResultIsError` and hook rewrite | `crates/maho-core/src/messages.rs`, `crates/maho-codemode/src/tool/tool_result_marshal.rs`, `crates/maho-core/src/agent_session.rs` |
| `senpi#6157` (`senpi#6166`) | `9662d789ec` (`049032fa36`) | Keep `turnId` off `agent_start` and `agent_settled` on the legacy wire | `crates/maho-core/src/messages.rs`, `crates/maho-rpc/src/session_open_turns.rs` |
| `senpi#5203` (`senpi#4617`) | `493ea29149` (`f2d58831f7`) | Weight CJK text in compaction cut-point budgets (`cjkExtraChars`, `estimateCutPointTokens`) | `crates/maho-core/src/compaction/compaction.rs`, `crates/maho-core/src/compaction/utils.rs`, `crates/maho-core/src/compaction/mod.rs` |
| `senpi#6833` (`omo#13785`) | `6735e0285e` (`4782322885`) | Keep required compaction errors out of provider fallback (`!_isRequiredCompactionError`) | `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/compaction/compaction_execution.rs` |
| `senpi#6764` + `senpi#6772` (`senpi#6760`) | `29c7bae28a`, `147a0df07e` | `transient-failure` rejection cause and non-latching admission on transient summary failure or circuit-breaker cooldown | `crates/maho-ext-api/src/types.rs`, `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/compaction/compaction_execution.rs`, `crates/builtins/maho-ext-compaction/src/speculative.rs`, `crates/builtins/maho-ext-compaction/src/extension_wiring.rs` |
| `senpi#4093` (`senpi#5043`) | `af11e3ac8e` (`6b952de61a`, `ee384c6e9c`) | Account-keyed vs material-keyed credential blocks (`StoredSlotRevisions`, `applicableStoredState`, `retireLoginSlotHealth`) | `crates/maho-core/src/credential_pool/state_store.rs`, `crates/maho-core/src/credential_pool/rotation_stream.rs`, `crates/maho-core/src/credential_accounts.rs`, `crates/maho-core/src/auth_storage.rs`, `crates/maho-core/src/model_runtime.rs` |
| `senpi#6342` (`senpi#6347`) | `2031def19f` (`f27ad36f17`, `dd2bbfdfae`, `87af244000`) | Same-account delegated connections follow host turn origin; turn origin scoped to record handling and retry/recovery continuation | `crates/maho-core/src/input_origin.rs`, `crates/maho-core/src/auth_storage.rs` |

## 2. 1:1 Symbol Mapping Table

| Upstream File & Symbol | `mhc` Module & Symbol | Notes |
|---|---|---|
| `core/session-archived.ts::sessionMarkerData` | `maho_core::session_archived::session_marker_data` | Validates `type == "custom"`, `customType`, and `data.sessionId == session_id` |
| `core/session-archived.ts::archivedFlagOf` | `maho_core::session_archived::archived_flag_of` | Extracts `Option<bool>` for matching `sessionId` |
| `core/session-archived.ts::isSessionArchived` | `maho_core::session_archived::is_session_archived` | Reads cached `SessionManager::is_archived()` in O(1) |
| `core/session-archived.ts::setSessionArchived` | `maho_core::session_archived::set_session_archived` | Appends `session_archived` custom entry with `{ archived, sessionId }` |
| `core/session-settled.ts::ThreadSettledBy` | `maho_core::session_settled::ThreadSettledBy` | Variants: `User`, `Agent`, `PrMerged`, `PrClosed`, `Inactivity` |
| `core/session-settled.ts::SessionSettled` | `maho_core::session_settled::SessionSettled` | `{ settled: bool, by: Option<ThreadSettledBy> }` |
| `core/session-settled.ts::NOT_SETTLED` | `maho_core::session_settled::NOT_SETTLED` | Constant `{ settled: false, by: None }` |
| `core/session-settled.ts::parseSessionSettled` | `maho_core::session_settled::parse_session_settled` | Validates `settled` boolean and optional `by` payload |
| `core/session-settled.ts::settledStateOf` | `maho_core::session_settled::settled_state_of` | Extracts `Option<SessionSettled>` for matching `sessionId` |
| `core/session-settled.ts::settledFields` | `maho_core::session_settled::settled_fields` | Projects `SettledFields { settled, settled_by }` |
| `core/session-settled.ts::readSessionSettled` | `maho_core::session_settled::read_session_settled` | Reads cached `SessionManager::get_settled()` in O(1) |
| `core/session-settled.ts::setSessionSettled` | `maho_core::session_settled::set_session_settled` | Appends `session_settled` custom entry with `{ settled, by?, at, sessionId }` |
| `core/host-restart-continuation.ts::HOST_DRAIN_HOLD_KEY` | `maho_core::host_restart_continuation::HOST_DRAIN_HOLD_KEY` | `"host-drain"` |
| `core/host-restart-continuation.ts::RESTART_CONTINUATION_LIMIT` | `maho_core::host_restart_continuation::RESTART_CONTINUATION_LIMIT` | `3` |
| `core/host-restart-continuation.ts::RestartHoldReason` | `maho_core::host_restart_continuation::RestartHoldReason` | `RepeatedRestartNoProgress`, `RepeatedRestartLimit` |
| `core/host-restart-continuation.ts::engineStartedLastRun` | `maho_core::host_restart_continuation::engine_started_last_run` | Walks branch entries to classify run starter |
| `core/host-restart-continuation.ts::restartHoldData` | `maho_core::host_restart_continuation::restart_hold_data` | Produces `{ reason }` for `interrupted-run` hold on repeated restarts |
| `core/host-restart-continuation.ts::restartRelease` | `maho_core::host_restart_continuation::restart_release` | Decides `RestartRelease::Resume` vs `RestartRelease::Release` |
| `core/session-lifecycle.ts::cutByHostShutdown` | `maho_core::session_lifecycle::SessionLifecycleController::cut_by_host_shutdown` | Marks `ledger.interrupted = true` and checkpoints |
| `core/session-lifecycle.ts::continueAfterRestart` | `maho_core::session_lifecycle::SessionLifecycleController::continue_after_restart` | Sets `restart_continues = true` so `requires_human_continuation()` is false until run start |
| `core/agent-session.ts::holdAutomaticTurns` | `maho_core::agent_session::AgentSession::hold_automatic_turns` | Adds `HoldKind::Unattended` with key `HOST_DRAIN_HOLD_KEY` |
| `core/agent-session.ts::_continueAfterHostRestart` | `maho_core::agent_session::AgentSession::continue_after_host_restart` | Releases restart-owned holds or resumes cut engine run once |
| `core/agent-session.ts::_stopEngineTurn(customType, released)` | `maho_core::agent_session::AgentSession::stop_engine_turn_with_release` | Exempts released wake/peer turns from `max_per_resume_window` |
| `core/moved-session-cwd.ts` + `omo#14296` `fileOpen` | `maho_core::moved_session_cwd::{recorded_reopen_cwd, read_recorded_reopen_cwd_from_file, reopen_session_open_request}` | Immediate recorded absolute folder extraction without filesystem probes |
| `core/compaction/compaction.ts::cjkExtraChars` | `maho_core::compaction::compaction::cjk_extra_chars` | Weights CJK codepoints by `CJK_CHAR_WEIGHT = 3` (UTF-16 units) |
| `core/compaction/compaction.ts::estimateCutPointTokens` | `maho_core::compaction::compaction::estimate_cut_point_tokens` | `estimate_tokens(msg) + ceil(cjk_extra_chars(serialized) / 4)` |
| `core/agent-session.ts::_failedCompactionLatchesAdmission` | `maho_core::compaction::compaction_execution::failed_compaction_latches_admission` | Excludes `external-owner`, `transient-failure`, and `circuit-breaker` |
| `core/credential-pool/state-store.ts::StoredSlotRevisions` | `maho_core::credential_pool::state_store::StoredSlotRevisions` | `{ account, material }` HMAC revisions |
| `core/credential-pool/state-store.ts::applicableStoredState` | `maho_core::credential_pool::state_store::applicable_stored_state` | Account blocks survive refresh; `auth_error` retires with material |
| `core/credential-pool/login-health.ts::slotsStartedByLogin` | `maho_core::credential_pool::state_store::slots_started_by_login` | Detects newly created or material-replaced stored slots |
| `core/credential-pool/login-health.ts::retireLoginSlotHealth` | `maho_core::credential_pool::state_store::retire_login_slot_health` | Clears sidecar health for slots started by login |
| `core/turn-origin.ts` (`senpi#6342`) | `maho_core::input_origin::{TurnOrigin, InputTurnMark, ConnectionOrigin, TurnOriginState}` | Same-account host following and record-scoped delegated run origin |
