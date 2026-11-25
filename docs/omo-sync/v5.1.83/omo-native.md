# omo-native track — omo v5.1.80..v5.1.83

Range: `822bd923038` (`v5.1.79`) .. `69a1863a457` (`v5.1.83`; `v5.1.80` was never released)
Upstream paths: `packages/omo-native` (156 non-merge commits in range)
mhc counterparts: `crates/maho-cli`, `crates/maho-rpc`, `crates/omo/components/maho-omo-remote`

## Summary

| verdict | count |
|---|---:|
| port | 4 |
| ported | 13 |
| n/a | 41 |
| **total** | **58** |

Counted programmatically from the verdict column below, one row per upstream PR number named in the
range's `packages/omo-native` subjects; commits that name no PR (releases, `chore(bundles)`, `chore(deps)`,
`chore(train)`) follow the PR they regenerate or pin and are not separate rows.

## What landed

1. **`mhc sync status`** (omo#14437, omo#14456, senpi#7107): `crates/omo/components/maho-omo-remote/src/sync_status.rs`
   reports the account, this device's join and switch, the account's device rows, pending join requests and
   whether an engine runs here, each section with its own failure. The engine section connects to the operator
   socket and closes without sending a request, so it neither authenticates nor attaches. The verb is dispatched
   from `crates/maho-cli/src/main.rs` (`run_sync_entry`) and listed in `crates/maho-cli/src/cli/help.txt`.
2. **Host scope** (omo#14341, #14348, #14384): `crates/maho-rpc/src/host_scope.rs` asks the user manager to move
   a Linux host supervisor's pid into a fresh transient scope (`StartTransientUnit`, `OOMPolicy=continue`,
   `MemoryMax` at 90% of `MemTotal`) before it spawns its host, and never fails the launch. Entered from
   `crates/maho-rpc/src/supervisor_route.rs`.
3. **Brand-named extensions on an attaching start** (omo#14465): `crates/maho-cli/src/cli/held_session_attach.rs`
   treats a `--extension` path the brand profile names as the distribution's own (trailing separator and symlinked
   parent included), so a launcher-prepended plugin no longer refuses an attach. The launcher brand in
   `crates/maho-cli/src/senpi_install.rs` already carried `extensions`.
4. **Person-facing refusal output** (omo#14305 review M4): `person_next_action` in `maho-omo-remote/src/render.rs`
   replaces an eval-only `thread_list` hint with the CLI verb on every `report_failure` path.

## Where the rest of the range already lives

The address/listing core of this range (`candidate_lines`, `typed_device`, `unanswered_devices`,
`resolve_typed_address`) is the `thread-remote` track's ledger (`docs/omo-sync/v5.1.83/thread-remote.md`); the
graceful close, backlog/steer-reserve, `collapse_key` and listing-row 6/7 contracts are in
`maho-omo-gateway` and `device-service-core`, recorded under `5183` fragments there. This track's rows cite
those locations rather than re-porting them.

## Per-PR verdicts

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #7014 | n/a | a senpi dependency pin bump (`chore(deps)`), not an omo-native change |
| #7019 | n/a | a senpi dependency pin bump |
| #7023 | n/a | a senpi dependency pin bump |
| #7024 | n/a | a senpi dependency pin bump |
| #7030 | n/a | a senpi dependency pin bump |
| #7107 | n/a | the senpi issue #14456 answers in code (the engine status line names why details are not shown); ported under #14456 |
| #12022 | n/a | omo updates through an npm side-by-side `versions/current` layout; mhc installs signed release archives into `versions/<v>` with its own launcher and lock (`native_update/`), so there is no wrapper to regenerate |
| #12308 | n/a | hosted sign-in pin retirement; mhc has no hosted sign-in |
| #12383 | n/a | same as #12308 |
| #12384 | n/a | same as #12308 |
| #13077 | n/a | senpi pin bump only |
| #13159 | n/a | TS-only fixture umask test |
| #13640 | n/a | omo's hosted service-auth keyring/sign-in; mhc has no hosted account service |
| #13642 | n/a | same as #13640 |
| #13965 | n/a | TS-only build-extension test harness |
| #13986 | n/a | TS-only store-worker test harness |
| #14180 | ported | `maho-omo-gateway/src/thread` (`cursor_stale` on an older cursor) |
| #14253 | ported | `maho-cli/src/cli/session_of_pid.rs` forwards `session-of-pid <pid> --json` unchanged |
| #14255 | n/a | TS-only test cleanup |
| #14262 | n/a | train regeneration only |
| #14299 | n/a | senpi pin bump only |
| #14305 | ported | `maho-omo-remote/src/{device,render,verbs}.rs` (`candidate_lines`, `person_next_action`) |
| #14313 | ported | `maho-omo-gateway/src/thread/control/close_ladder.rs` (`CloseGraceful`) |
| #14321 | n/a | generated bundle regeneration only |
| #14322 | ported | `maho-omo-gateway/src/thread/address` refuses `self` for a `cli:` caller |
| #14324 | ported | `maho-omo-gateway/src/thread/control/close_ladder.rs` (drain cap, `wait_ms` needs graceful) |
| #14328 | ported | `maho-omo-gateway/src/thread/remote` |
| #14331 | n/a | the omo remote address CLI review; its Rust counterpart landed with the thread-remote track (`maho-omo-remote/src/device.rs`) |
| #14333 | ported | `maho-omo-gateway/src/thread/remote/outcome.rs` (`effective_mode`, `decide_delivery`) |
| #14336 | ported | `mhc doctor` and the CLI dispatch keep their own command lists; there is no shared engine-command registry to split |
| #14337 | ported | same as #14336 |
| #14341 | port | `maho-rpc/src/host_scope.rs`, entered from `src/supervisor_route.rs` |
| #14345 | n/a | formatting and generated-bundle regeneration only |
| #14348 | n/a | TS-only busctl stub test |
| #14356 | n/a | generated bundle regeneration only |
| #14363 | n/a | generated bundle regeneration only |
| #14384 | n/a | TS-only Windows test gating |
| #14404 | n/a | generated bundle regeneration only |
| #14415 | n/a | generated bundle regeneration only |
| #14428 | ported | `maho-omo-gateway/src/thread/backlog.rs` (steer reserve) + `thread/remote/mod.rs` (`collapse_key` refusal) |
| #14431 | ported | `maho-omo-gateway/src/thread/backlog.rs` (collapse-key facts) |
| #14434 | n/a | generated bundle regeneration only |
| #14437 | port | `maho-omo-remote/src/sync_status.rs` (`run_sync`), `maho-cli/src/main.rs` (`run_sync_entry`) |
| #14456 | port | `maho-omo-remote/src/sync_status.rs` (`read_engine_sync_status`: presence only, never attaches) |
| #14465 | port | `maho-cli/src/cli/held_session_attach.rs` (`extensions_except`); the launcher brand already carries `extensions` in `senpi_install.rs` |
| #14466 | n/a | TS-only review test pin |
| #14475 | n/a | generated bundle regeneration only |
| #14510 | ported | `device-service-core/src/local_api/protocol.rs` (`ListingRow.waiting`, `.engine_version`) |
| #14511 | n/a | generated bundle regeneration only |
| #14517 | n/a | generated bundle regeneration only |
| #14528 | n/a | TS-only test harness (a Promise subclass for closed continuations) |
| #14537 | n/a | generated bundle regeneration only |
| #14556 | n/a | test-only, gated on omo's hosted service-auth state file |
| #14622 | n/a | the gateway store's dev-build runtime guard; mhc's store is a Rust SQLite store with no dev-build runtime to detect |
| #14624 | n/a | TS-only socket-client stall edges in `omo-senpi/src/runners/rpc-host`; mhc's client is `crates/maho-rpc/src/socket_client.rs`, owned by the rpc-host tracks |
| #14682 | n/a | TS-only test harness (CPU-progress probe budget) |
| #14755 | n/a | TS-only thread SDK refusal shape; the Rust control ladder already answers a typed refusal as itself |
| #14792 | n/a | TS-only daemon test harness (process-tree CPU budget) |
