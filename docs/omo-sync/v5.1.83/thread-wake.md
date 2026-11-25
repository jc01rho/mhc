# omo v5.1.80..v5.1.83 sync: thread-wake

Range: oh-my-openagent `822bd923038..69a1863a457` (v5.1.79 to v5.1.83; v5.1.80 was never released).
Upstream paths: `packages/omo-senpi/src/components/thread/{wake,run-alerts}`, 22 non-merge commits.
mhc counterpart: `crates/omo/components/maho-omo-gateway/src/thread/{wake,run_alerts}/`.

## Summary

| verdict | rows |
|---|---:|
| port | 7 |
| ported | 0 |
| n/a | 1 |
| gap | 7 |
| total | 15 |

One row per upstream PR, or per commit group where a change carries no PR number; a PR or commit group whose slice
is partly portable without the #13037 wake runtime / schema v12-v14 tables is split into two rows. Every non-merge
commit in the range is named in exactly one row.

## Wake subsystem baseline

`maho-omo-gateway`'s store (`thread/schema.rs`) carries upstream migrations v1..v16 since the gateway-schema track
(PR #86), so the v12-v14 tables exist (the intake ledger columns, `woken_sessions`, `session_wait`, `wake_counts`,
`intake_marks`, `intake_commands`); the #13037 wake runtime that writes and reads them (`carrier`, `session-waker`,
`pending-sweep`, `consumer-start`, `runtime`) is not ported in mhc (consistent with
`docs/omo-sync/v5.1.83/thread-gateway.md`).

Modules under `thread/wake/` that operate on the store and session files (`engine_minimum`,
`memory_headroom`, `closed_intake`, `session_target`) and the entire `thread/run_alerts/` subsystem (#14430,
paired with `omo-config-core/src/schema/operator_alerts.rs` from `misc-core` #75) are ported here with their unit
and component integration tests. Parts that require `carrier`, `pending-sweep`, `consumer-start` or `runtime` stay
`gap` until #13037 lands.

## Per PR

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #14335 (`d005504cdf`, `6bc00f474f`) | gap | `wake/runtime.ts` `admitIntake` checking `consumer?.serving()` and `component-wiring.test.ts` / `fake-engine-host.ts` need the #13037 wake runtime and delivery consumer (the v12/v13 tables exist since PR #86). |
| #14430 (`c02d7200fa`, `293610e2a2`, `0098c7729e`, `4c123c6ec1`) | port | `crates/omo/components/maho-omo-gateway/src/thread/run_alerts/` (`error_streak.rs`, `run_alerts.rs`), `tools.rs` (`create_thread_send_as`), `component.rs` / `native.rs` lifecycle hook wiring (`AgentEnd`, `AgentSettled`, `SessionShutdown`), and `tests/run_alerts_tests.rs` (13 unit tests + 5 component integration tests). Schema registration in `omo-config-core` (`operator_alerts.rs`) landed in `misc-core` (#75). |
| #14383 closed-intake race drop (`aff3383d15`) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/closed_intake.rs` `settle_closed_intake` drops rows whose `refuse_queued` lost the race to another claimant instead of returning them as remaining queued rows. Test: `given_a_row_already_claimed_before_refusal_when_settled_then_it_is_dropped_from_remaining`. |
| #14383 runtime nothing_queued drained marks (`aff3383d15`) | gap | `wake/carrier.test.ts`, `wake/runtime.ts` (`reportDrained` on `nothing_queued`), and `wake/runtime-start.test.ts` need the #13037 wake runtime and carrier (the v14 `intake_marks` table exists since PR #86). |
| #14363 memory floor (`f1ec5e79ae`, `0fcdd7cd47`, `abd077fd6a`, `b41242db44`) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/memory_headroom.rs` `WAKE_SESSION_COST_BYTES` (409 MiB) and `read_wake_memory_floor` (1/4 of cgroup v2 limit / `MemTotal`, capped at 1 GiB, floored at 409 MiB) with 6 unit tests. |
| #14363 carrier floor & create (`f1ec5e79ae`, `0fcdd7cd47`, `abd077fd6a`, `b41242db44`) | gap | `wake/carrier.ts` (`memoryFloor` deferral) and `wake/carrier.test.ts` need the #13037 wake carrier (`woken_sessions`, v13, exists since PR #86); `thread_create` is not ported in mhc. |
| #14355, #14339 (`640d1bd2ce`) | n/a | Formatting of TS merge resolutions in `wake/parked-resume.test.ts`. |
| `07bec8317f`, `b7a993b53e` (no PR) | gap | `wake/carrier.ts` (`waitBlocksDelivery` check on `sessionWait.read`) and `wake/parked-resume.test.ts` need the #13037 carrier (`session_wait`, v14, exists since PR #86; matches `thread-gateway.md` `07bec8317f`). |
| `01fa0b0220`, `a7ad9d1639` memory headroom probe (no PR) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/memory_headroom.rs` `WAKE_MEMORY_FLOOR_BYTES` (1 GiB) and `read_host_memory_headroom` (min of Linux cgroup v2 room and `/proc/meminfo` `MemAvailable`) with 5 unit tests. |
| `01fa0b0220`, `a7ad9d1639` carrier & session-waker (no PR) | gap | Replacing the woken-session count cap with `host_memory_pressure` in `wake/carrier.ts`, `wake/session-waker.ts`, `wake/carrier.test.ts`, and `wake/session-waker.test.ts` needs #13037 `carrier` and `session-waker` (`woken_sessions`, v13, exists since PR #86; matches `thread-gateway.md` `01fa0b0220`). |
| `8dfb297ba1`, `9d4469b155` CalVer prefix engine gate (no PR) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/engine_minimum.rs` `MIN_WAKE_ENGINE_VERSION` (`2026.11.15-2`) and `engine_drops_retained_record` reading dev-engine CalVer prefix (`YYYY.MM.DD-N[-omob...]`) with 3 unit tests (22 version cases). |
| `d54aa61b60` parked resume test (no PR) | gap | `wake/parked-resume.test.ts` end-to-end delivery to a parked session needs the #13037 wake runtime, carrier and `FakeSessionRuntime` (`woken_sessions`, v13, exists since PR #86). |
| #14313, #14324 session-target close instant (`336f5fa4b9`, `ae55079bdb`) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/session_target.rs` `close_instant_of`, `closed_at_of`, and `resolve_wake_target` reading `SESSION_CLOSED_ENTRY` (`session-closed`) and `rpc_effect` (`unload`/`done`) timestamps from the session transcript tail, with unit tests. |
| #14273 closed-intake & session-target (`a397780f93`) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/closed_intake.rs` `settle_closed_intake` (refusing rows queued at or before `closed_at` as `target_closed`) and `crates/omo/components/maho-omo-gateway/src/thread/wake/session_target.rs` `resolve_wake_target` (`closed_at` from `rpc_effect` unload `done`) with unit tests. |
| #14273 carrier, consumer-start & pending-sweep (`a397780f93`) | gap | `wake/carrier.ts`, `wake/consumer-start.ts`, `wake/pending-sweep.ts`, `wake/pending-sweep.test.ts`, and `wake/testing/fake-wake-host.ts` need the #13037 wake carrier and pending sweep (the v12/v14 intake tables exist since PR #86; matches `thread-gateway.md` `#14273 closed intake settle`). |

## Tests

New cases, all under `crates/omo/components/maho-omo-gateway`:

- `src/thread/wake/engine_minimum.rs`: 3 unit tests covering `MIN_WAKE_ENGINE_VERSION`, 22 senpi CalVer / omo release /
  `0.0.0-omob.*` / dev-engine CalVer prefix stamps (`2026.11.19-2-1-omob...`), and caller-supplied minimum overrides.
- `src/thread/wake/memory_headroom.rs`: 11 unit tests covering `WAKE_MEMORY_FLOOR_BYTES` (1 GiB),
  `WAKE_SESSION_COST_BYTES` (409 MiB), `read_host_memory_headroom` (cgroup v2 limit vs `MemAvailable`, `max`, missing
  cgroup, non-Linux), and `read_wake_memory_floor` (768 MiB and 256 MiB cgroup limits floored at 409 MiB, 2 GiB host
  quarter 512 MiB, 16 GiB host capped at 1 GiB).
- `src/thread/wake/closed_intake.rs`: 2 unit tests covering `settle_closed_intake` (`target_closed` refusal at or
  before `closed_at`, preservation of rows created after `closed_at`, and dropping rows whose `refuse_queued` lost the
  race to an active claim).
- `src/thread/wake/session_target.rs`: 2 unit tests covering `resolve_wake_target` (`rpc_effect` unload `done` and
  `session-closed` timestamps, `Interactive` / `Worker` / `Child` classification, `Gone` and `Ambiguous`).
- `src/thread/run_alerts/error_streak.rs`: 13 unit tests covering the 10-run threshold, 60-minute re-alert suppression
  and still-failing repeat alert, recovery notice on normal completion, multi-class error counts, aborted runs counted
  beside a streak without alerting, `forget` on session end, per-session isolation, `realert_ms: 0`, the 2,769-run
  replay, `classify_run_error`, and `run_end_of`.
- `tests/run_alerts_tests.rs` (registered in `tests/main.rs`): 5 component integration tests through
  `GatewayThreadComponent` (`AgentEnd`, `AgentSettled`, `SessionShutdown`) and `SqliteGatewayStore`:
  - alert + recovery notice to the duty thread and webhook with sentinel redaction check;
  - outbound alert delivery when the failing seat's own inbox holds 128 undelivered messages from 16 peers;
  - non-blocking settle when the webhook returns 500 once and holds the retry, without logging the webhook URL;
  - user-layer-only `operator_alerts` loading (workspace `.omo/omo.json` ignored for thread/webhook targets);
  - `SessionShutdown` resetting an in-progress streak so the next streak starts fresh.
