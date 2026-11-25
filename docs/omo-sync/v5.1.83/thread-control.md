# omo v5.1.80..v5.1.83 Sync Ledger — Track `thread-control`

Range: `822bd923038` (v5.1.79) .. `69a1863a457` (v5.1.83), `code-yeongyu/oh-my-openagent` (`dev`).
Owned upstream path: `packages/omo-senpi/src/components/thread/control`.
mhc target: `crates/omo/components/maho-omo-gateway/src/thread/control/*`.

## Summary

| Verdict | Count | Notes |
|---|---:|---|
| `port` | 6 | `#14273`, `#14313`, `#14324`, `#14352`, `#14394`, `#14482` (within `src/thread/control/{port,close_ladder,local_control,operations}.rs`) |
| `ported` | 0 | No row in the v5.1.80..v5.1.83 `thread/control` delta was already present on `main` |
| `n/a` | 0 | All 59 commits in range carry behavioral or test changes tied to code PRs |
| `gap` | 13 | `#13927`, `#14050`, `#14228`, `#14274`, `#14296`, `#14305`, `#14335`, `#14363`, `#14397`, `#14429`, `#14474`, `#14537` (unnamed-row), `#14665` — all live in subsystems not yet ported into `mhc` (`thread_create`, `RelayControl`/`RelaySessionHost`, `address::resolve_address`) |
| **Total** | **19** | |

## Per-PR Verdicts

| omo PR | verdict | mhc location or reason |
|---|---|---|
| `#13927` (`9baf96c592`, train 5 `#14332`) | `gap` | Needs `thread_create` subsystem (`create.ts` `gateway_schema_too_new` refusal on first prompt; `maho-omo-gateway` does not register `thread_create`, `create.rs`, `LocalSessionHost`, or `worktree` yet). |
| `#14050` (`c9d2531d61`, train 4 `#14284`) | `gap` | Needs `thread_create` subsystem (`create.ts` blank-prompt refusal and `session-host.test.ts`). |
| `#14228` (`152e8dd915`, `eaf6231620`, train 5 `#14332`) | `gap` | Needs `thread_create` subsystem (`create.ts` schema-mismatch refusal message order). |
| `#14273` (`a397780f93`, train 6 `#14356`) | `port` | `crates/omo/components/maho-omo-gateway/src/thread/control/close_ladder.rs` (`close_effect_id()` emits `effectId: "omo-thread-close:<uuid>"` on every `unload_session` request). Note: `relay-control.ts` `closedAt`/`SESSION_GONE` listing mark is `gap` (no `RelayControl` subsystem). |
| `#14274` (`1f4aa0783d`, `530d5007bd`, train 5 `#14332`) | `gap` | Needs `thread_create` subsystem (`create.ts` `keepName`, conflict check on reopen, `thread-reopen-name.test.ts`). |
| `#14296` (`c33f7e5003`, `abce6764cd`, train 7 `#14404`) | `gap` | Needs `thread_create` subsystem (`create.ts` `fileOpen` passing session file `cwd`, `thread-reopen-folder.test.ts`). |
| `#14305` (with `#14328`, `#14386`; trains 6–8 `#14356`, `#14404`, `#14415`) | `gap` | Needs `address::resolve_address` (`thread-address` track), `thread_create`, and `RelayControl`/`RelaySessionHost` (`thread-remote` track) for `locate.ts`, `create.ts`, `relay-control.ts`, `relay-session-host.ts`, and `HostedThread::unnamed`. |
| `#14313` (`336f5fa4b9`, `dd8f9503b1`, train 6 `#14356`) | `port` | `crates/omo/components/maho-omo-gateway/src/thread/control/{port.rs,close_ladder.rs,local_control.rs,operations.rs}` and `tests/thread_control_tests.rs` (`CloseOptions`, `CloseGraceful`, `UNLOAD_GRACEFUL`, `advertises`, `graceful_close` with `call_budget::wait_within_call_budget`, TUI graceful-only close in `LocalControl::close`, `close_options` + `close_thread_with_options`, and all 5 core graceful ladder unit tests). Tool wiring in `control_tools.rs` (`thread-core`), `ThreadCloseOk::graceful` (`thread-gateway`), and host `unload_graceful` (`maho-rpc`) live outside this track. |
| `#14324` (`ae55079bdb`, train 6 `#14356`) | `port` | `crates/omo/components/maho-omo-gateway/src/thread/control/{close_ladder.rs,operations.rs}` (`REMOTE_GRACEFUL_MAX_WAIT_MS = 35_000`, `leg_max_wait_ms` cap + `wait_capped` details, and `wait_ms` without `graceful` refused as `invalid_arguments`, plus unit tests). `relay-control.ts` caller leg is `gap`. |
| `#14335` (`929de417e1`, `0f327237b5`, train 6 `#14356`) | `gap` | Needs `thread_create` subsystem (`create.ts` and `create-launch.ts` delivering `prompt` on reopen of an already-live thread). |
| `#14352` (`c315cf2b88`, train 6 `#14356`) | `port` | `crates/omo/components/maho-omo-gateway/src/thread/control/close_ladder.rs` (`capped` is set only when `wait == leg_cut`, omitting `wait_capped` when the call budget is tighter than `leg_max_wait_ms`, plus unit test). |
| `#14363` (with `#14355`, `#14339`, prompt-start & unknown fields; train 6 `#14356`) | `gap` | Needs `thread_create` and `gateway::delivery_view` / `admitted_wait` (`thread-gateway` track) for `create.ts` (`prompt: null`, unknown-field refusal, `promptThatRan`) and `prompt-start.ts` (`awaitPromptStart`). |
| `#14394` (`ca6a781cee`, `61068b5c44`, `e59b6249c2`, train 8 `#14415`) | `port` | `crates/omo/components/maho-omo-gateway/src/thread/control/port.rs` (`SessionOpenOptions { confirm_reach }` on `SessionHost::open`). Note: `create.ts` and `relay-session-host.ts` unreachable-session teardown remain `gap` (no `thread_create` or `RelaySessionHost` subsystem). |
| `#14397` (`51b9dace02`, `758c9c0357`, train 8 `#14415`) | `gap` | Needs `thread_create` and `RelaySessionHost` / remote worktree subsystem (`create.ts`, `relay-session-host.ts` removing remote worktree and retrying single `route_unlisted`). |
| `#14429` (merged as `#14458`: `96bd3f498d`, `b3aeaf0968`, `849bdb1fc2`) | `gap` | Needs `RelaySessionHost` / `RemoteThreadSurface` subsystem (`relay-session-host.ts` using `rowFolder` instead of `row.project`, `relay-row-cwd.test.ts`). |
| `#14474` (merged as `#14479`: `3b41b5d6f6`) | `gap` | Needs `RelaySessionHost` / `RemoteThreadSurface` subsystem (`relay-session-host.ts` deriving `created_at` from UUIDv7 thread id via `rowCreatedAt`). |
| `#14482` (`3d4af11b30`, `80d5731668`, `65399addb3`, `b848160e4c`, `dd22c8ef7b`, train 11 `#14537`) | `port` | `crates/omo/components/maho-omo-gateway/src/thread/control/{port.rs,local_control.rs,operations.rs}` and `tests/thread_control_tests.rs` (`SessionModelState`, `session_model_state`, `SessionControl::get_model_state`, `LocalControl::get_model_state`, `:thinking` suffix handling in `set_model_on`, pre-switch snapshot `model_state_unavailable`, switch confirmation `model_switch_unconfirmed`, full model+thinking rollback on refusal, `partial_commit` on failed restore, rematch handling of `model_state_unavailable` vs transport error, plus `set-model-rematch` and rollback tests). `model_match.rs` `provider_not_configured` belongs to `thread-core`; `relay-control.ts` `getModelState` is `gap`. |
| `#14537` (`0498050eaa`, `c7195fff86`, `c9fe02029d` unnamed-row, train 11 `#14537`) | `gap` | Needs `RelaySessionHost` / `RemoteThreadSurface` subsystem (`relay-session-host.ts` naming unnamed rows via `rowName` with `rowFolder`, `relay-row-cwd.test.ts`). |
| `#14665` (`d9f50119d7`, `f820daae67`) | `gap` | Needs `thread_create` subsystem (`create.ts` `deduplicated: true` on same-key create replay, `thread-create-atomic.test.ts`, `thread-create-options.test.ts`). |
