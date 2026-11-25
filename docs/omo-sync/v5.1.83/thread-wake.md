# omo v5.1.80..v5.1.83 sync: thread-wake

Range: oh-my-openagent `822bd923038..69a1863a457` (v5.1.79 to v5.1.83; v5.1.80 was never released).
Upstream paths: `packages/omo-senpi/src/components/thread/{wake,run-alerts}`, 22 non-merge commits.
mhc counterpart: `crates/omo/components/maho-omo-gateway/src/thread/{wake,run_alerts}/`.

## Summary

| verdict | rows |
|---|---:|
| port | 4 |
| ported | 0 |
| n/a | 1 |
| gap | 8 |
| total | 13 |

One row per upstream PR, or per commit group where a change carries no PR number; a PR or commit group whose slice
is partly portable without the #13037 wake subsystem is split into two rows. Every non-merge commit in the range is
named in exactly one row.

## Wake subsystem baseline

`maho-omo-gateway`'s store (`thread/schema.rs`) stops at upstream migration v10, and the #13037 wake runtime
(`woken_sessions` v13, `session_waits` v14, `intake_ledger` v12, `carrier`, `session-waker`, `pending-sweep`,
`consumer-start`, `session-target`, `closed-intake`, `runtime`) is not ported in mhc (consistent with
`docs/omo-sync/v5.1.83/thread-gateway.md`).

Self-contained modules under `thread/wake/` (`engine-minimum`, `memory-headroom`) and the entire `thread/run-alerts/`
subsystem (#14430) are portable without `woken_sessions` / `intake_ledger` and are ported here with their unit and
component tests. Parts that require `carrier`, `session-target`, `pending-sweep`, `consumer-start`, `runtime`, or the
v12-v14 tables stay `gap` until #13037 lands.

In addition, `crates/omo/omo-config-core` is owned by the `misc-core` track (`operator-alerts.ts`); until `misc-core`
declares `operator_alerts` in `omo-config-core/src/schema/config.rs`, `load_senpi_omo_config` strips unrecognized
`operator_alerts` keys from `omo.json(c)`. `run_alerts` reads `operator_alerts` from the resolved user-layer config
value and provides `resolve_error_streak_settings` + test/component seams so the full alert pipeline is live and tested
now and picks up `omo.json(c)` automatically once `misc-core` merges.

## Per PR

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #14335 (`d005504cdf`, `6bc00f474f`) | gap | `wake/runtime.ts` `admitIntake` checking `consumer?.serving()` and `component-wiring.test.ts` / `fake-engine-host.ts` need the #13037 wake runtime, delivery consumer, and `woken_sessions` (v13) / `intake` (v12) store tables. |
| #14430 (`c02d7200fa`, `293610e2a2`, `0098c7729e`, `4c123c6ec1`) | port | `crates/omo/components/maho-omo-gateway/src/thread/run_alerts/` (`error_streak.rs`, `run_alerts.rs`), `component.rs` / `native.rs` lifecycle hook wiring (`AgentEnd`, `AgentSettled`, `SessionShutdown`), and `tests/run_alerts_tests.rs`. Schema registration in `omo-config-core` (`operator-alerts.ts`) belongs to `misc-core`. |
| #14383 (`aff3383d15`) | gap | `wake/carrier.ts`, `wake/closed-intake.ts`, `wake/runtime.ts` (`reportDrained` on `nothing_queued`), and `wake/runtime-start.test.ts` need the #13037 wake runtime, carrier, and intake marks. |
| #14363 memory floor (`f1ec5e79ae`, `0fcdd7cd47`, `abd077fd6a`, `b41242db44`) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/memory_headroom.rs` `WAKE_SESSION_COST_BYTES` (409 MiB) and `read_wake_memory_floor` (1/4 of cgroup v2 limit / `MemTotal`, capped at 1 GiB, floored at 409 MiB) with unit tests. |
| #14363 carrier floor & create (`f1ec5e79ae`, `0fcdd7cd47`, `abd077fd6a`, `b41242db44`) | gap | `wake/carrier.ts` (`memoryFloor` deferral) and `wake/carrier.test.ts` need the #13037 wake carrier and `woken_sessions` (schema v13); `thread_create` is not ported in mhc. |
| #14355, #14339 (`640d1bd2ce`) | n/a | Formatting of TS merge resolutions in `wake/parked-resume.test.ts`. |
| `07bec8317f`, `b7a993b53e` (no PR) | gap | `wake/carrier.ts` (`waitBlocksDelivery` check on `sessionWait.read`) and `wake/parked-resume.test.ts` need the #13037 carrier and schema v14 `session_waits` (matches `thread-gateway.md` `07bec8317f`). |
| `01fa0b0220`, `a7ad9d1639` memory headroom probe (no PR) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/memory_headroom.rs` `WAKE_MEMORY_FLOOR_BYTES` (1 GiB) and `read_host_memory_headroom` (min of Linux cgroup v2 room and `/proc/meminfo` `MemAvailable`) with unit tests. |
| `01fa0b0220`, `a7ad9d1639` carrier & session-waker (no PR) | gap | Replacing the woken-session count cap with `host_memory_pressure` in `wake/carrier.ts`, `wake/session-waker.ts`, `wake/carrier.test.ts`, and `wake/session-waker.test.ts` needs #13037 `carrier`, `session-waker`, and schema v13 `woken_sessions` (matches `thread-gateway.md` `01fa0b0220`). |
| `8dfb297ba1`, `9d4469b155` CalVer prefix engine gate (no PR) | port | `crates/omo/components/maho-omo-gateway/src/thread/wake/engine_minimum.rs` `MIN_WAKE_ENGINE_VERSION` and `engine_drops_retained_record` reading dev-engine CalVer prefix (`YYYY.MM.DD-N[-omob...]`) with unit tests. |
| `d54aa61b60` parked resume test (no PR) | gap | `wake/parked-resume.test.ts` end-to-end delivery to a parked session needs the #13037 wake runtime, carrier, `FakeSessionRuntime`, and `woken_sessions` (schema v13). |
| #14313, #14324 (`336f5fa4b9`, `ae55079bdb`) | gap | Under `wake/`, changes only `wake/session-target.ts` (`closeInstantOf` reading `SESSION_CLOSED_ENTRY`); needs #13037 `session-target` and `senpi#7046` `session-closed` transcript entries in `maho-core` (`graceful_close_returns.rs` itself was ported in `thread-gateway`). |
| #14273 (`a397780f93`) | gap | `wake/closed-intake.ts`, `wake/carrier.ts`, `wake/consumer-start.ts`, `wake/pending-sweep.ts`, and `wake/session-target.ts` need the #13037 wake subsystem and schema v12/v13 intake ledger & `woken_sessions` (matches `thread-gateway.md` `#14273 closed intake settle`). |
