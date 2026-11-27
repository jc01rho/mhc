# omo v5.1.83..v5.1.84 sync ledger — `omo-native` track

Source range: `69a1863a457` (`v5.1.83`) .. `8dbccd12842f` (`v5.1.84`)
Upstream paths: `packages/omo-native` (52 non-merge commits in range, plus `account-cli.ts` / `device-service-handoff.ts` for `omo account adopt`)
mhc counterparts: `crates/omo/components/maho-omo-remote`, `crates/maho-cli`, `crates/omo/device-service-core`

## Summary

| verdict | count |
|---|---:|
| port | 4 |
| ported | 3 |
| gap | 0 |
| blocked | 0 |
| n/a | 5 |
| **total** | **12** |

Counted programmatically from the verdict column below. Every one of the 52 upstream commit SHAs in
`tracks/omo-native.md` is cited in the table.

## What landed

1. **`mhc account` and device-service hand-off (`account-adopt=omo-account-v1`)** (`packages/omo-native/account-cli.ts`, `account-show.ts`, `account-session-outcome.ts`, `account-store.ts`, `device-service-handoff.ts`, `packages/device-service-core/src/hand-device-keys.ts`):
   - `crates/omo/components/maho-omo-remote/src/account_store.rs`: `AccountExit` (`0..=10`), `Credentials` (`parse`, `parse_adopt_input` stripping `refreshState`/`refreshAttempt`), `SignedOutMarker` (`signed_out_marker_path`, `read_signed_out_marker`, `write_signed_out_marker`, `clear_signed_out_marker`), `AccountCredentialStore` (`FileAccountCredentialStore` under `<home>/.omo/account-state/omo-cli-<id>.json`), and `retire_sync_pin_on_discard`.
   - `crates/omo/components/maho-omo-remote/src/device_service_handoff.rs`: `ADOPT_ACCOUNT_MIN_SERVICE_VERSION` (`"0.1.3"`), `connect_or_not_running`, `hand_device_keys`, `forget_device_service`, `MachineDeviceState`, and `CliDeviceServiceHandoff` (`adopt`, `forget`, `machine_device`).
   - `crates/omo/components/maho-omo-remote/src/account_cli.rs`: `run_account_verb` and `run_account_cli` serving `show [--account <id>]`, `adopt [--replace]`, `token`, `export`, `forget [--device <id>]`, `retire-sync --account <id]`, and `store status|enable|disable [--discard]`.
   - `crates/maho-cli/src/main.rs` and `crates/maho-cli/src/cli/help.txt`: dispatches `mhc account <verb> ...` in `omo` builds and reports it unavailable in the MIT profile.
2. **`#14711` / `#11065` single credential-store call on `logout` storage failure** (`6c9cae12ac`):
   - `crates/omo/components/maho-omo-remote/src/account_cli.rs` (`logout_account_session`): when the initial store read fails with `StoreUnavailable`, `KeychainLocked`, or `KeychainUnavailable`, `logout` makes no second read or `clear()` call, leaves credentials untouched, notifies `device_service.forget()`, and exits `9` (`store-unavailable`). An unreadable item (`InvalidState`) still writes the signed-out marker, retires/logs the sync pin, and clears the store.
3. **`#15025` / `#15111` display escaping for `mhc remote` human output and `--json` output boundary** (`944c86d6a4`, `9b24ac43c3`, `1a232eb8c9`, `9460c0633a`, `9fded163dd`):
   - `crates/omo/components/maho-omo-remote/src/render.rs`: `escape_for_display`, `escape_for_display_keep_lf`, `unsafe_for_display` (Unicode `Cc`, `Cf`, `Zl`, `Zp`), `refusal_lines`, `undelivered_prompt_lines`, `session_ended_line`, and display escaping in `render_table` (before column-width calculation), `status_lines`, and `session_lines`.
   - `crates/omo/components/maho-omo-remote/src/{context,device,verbs}.rs`: escapes all human stdout/stderr lines while keeping `--json` machine-readable output raw.
4. **`#13905` / `#14667` / `#14698` fail a `close` that closed nothing and read `send <target> -` from stdin** (`8f70de4861`, `bb2bde3fee`):
   - `crates/omo/components/maho-omo-remote/src/{args,usage,context,verbs}.rs`: adds `STDIN_TEXT = "-"`, `RemoteContext::set_stdin_reader` / `read_stdin` (empty stdin exits `2`), updates `send <session> <text|->` usage, and turns a `close` whose receipt reports `closed: false` into an exit-`1` `not_found` error instead of exit `0`.

## Per-PR verdicts

| upstream PR | verdict | mhc location or reason |
|---|---|---|
| `account-adopt` (`omo account adopt`, `account-cli.ts`, `account-show.ts`, `account-session-outcome.ts`, `account-store.ts`, `device-service-handoff.ts`, `hand-device-keys.ts`) | port | `crates/omo/components/maho-omo-remote/src/{account_cli,account_store,device_service_handoff}.rs`, `crates/maho-cli/src/{main.rs,cli/help.txt}`, and `crates/omo/components/maho-omo-remote/tests/account_cli.rs` (4 integration tests covering `adopt`, `--replace`, conflict, newer-token retention, `show --account`, `token` refresh and refusal codes, locked/unavailable store single-call exits, sync pin retirement, private file store, and device-service hand-off) |
| #14711 / #11065 `omo logout` single store call on storage failure (`6c9cae12ac`) | port | `crates/omo/components/maho-omo-remote/src/account_cli.rs` (`logout_account_session`) and `tests/account_cli.rs` (`storage_failures_report_exact_exit_codes_without_second_read_including_logout`: exits `9` after one `get` call without calling `delete`) |
| #15025 / #15111 display escaping for remote output writers and human lines, keeping `--json` unchanged (`944c86d6a4`, `9b24ac43c3`, `1a232eb8c9`, `9460c0633a`, `9fded163dd`) | port | `crates/omo/components/maho-omo-remote/src/{render,context,verbs,device}.rs`, `tests/display_escape.rs`, and `tests/verbs_table.rs`. (`remote-attach-presentation.ts` and `remote-background-enable.ts` in `1a232eb8c9` have no mhc counterpart: `mhc-relayd` carries no attach stream and mhc has no background post-login installer) |
| #13905 / #14667 / #14698 fail a `close` that closed nothing and read `send <target> -` from stdin (`8f70de4861`, `bb2bde3fee`) | port | `crates/omo/components/maho-omo-remote/src/{args,usage,context,verbs}.rs`, `tests/args_table.rs`, and `tests/verbs_table.rs` |
| #14698 / #11999 per-line bound on the attach endpoint line reader (`75fa73f750`, `f05b04676d`, `80e682a504`, `499aec3431`) | ported | `crates/omo/device-service-core/src/local_api/framing.rs` (`LineDecoder::push` splits incoming chunks on `\n` before enforcing `LOCAL_LINE_MAX` per line); `remote-attach.ts` has no standalone module in mhc because `mhc-relayd` serves no attach stream |
| #14258 / #14710 / #15100 / #15101 session-host unit binary path from the running executable (`a1d9844daf`, `5ebbbfbd9a`, `2b1ccaca1f`, `7a44a3a5b4`) | ported | `crates/omo/components/maho-omo-remote/src/context.rs` (`RemoteContext::for_process` resolves `mhc_bin` from `std::env::current_exe()`, never an inherited `OMO_BIN`/`MAHO_BIN` env var). The `bin/omo.js` npm launcher symlink branch (`5ebbbfbd9a`, `2b1ccaca1f`) is TS/npm-only; `device-service-core/src/cold-wake/*` in `a1d9844daf` belongs to `task-memory-device` |
| #14892 / #14909 / #14913 isolated agent dir in remote send tests (`24744f1f4e`, `6511ef02ef`) | ported | `crates/omo/components/maho-omo-remote/tests/verbs_table.rs` (`context()` places `agent_dir` under each test's `tempfile::tempdir()`, never `~/.omo/agent`) |
| #14319 / #14312 thread binding verb removal, #14426 `opening` state, #14992 / #15009 `omo thread close` flags, and #15025 `omo thread` display escaping in `bin/lib/thread*.js` (`4f6a56bbc0`, `7d36ea09ff`, `61d21904e1`, `a130d78c73`, `77d4bb28a1`, `64764b116b`, `c882861039`) | n/a | `mhc` has no `omo thread` CLI subcommand (`bin/lib/thread.js`, `thread-args.js`, `thread-output.js`; see `maho-omo-gateway/parity.d/5179.md`); thread operations are agent tools in `maho-omo-gateway`, owned by the `thread-core` and `thread-gateway` tracks |
| #14920 / #14923 telemetry destinations in `bin/lib/setup-summary.js` (`6de9001778`) | n/a | `mhc` has no `omo setup` wizard (`bin/lib/setup-*.js`); telemetry disclosures in `get-worker`, `telemetry-core` and `maho-omo-telemetry` belong to `misc-skills` |
| #13512 buffered send output test fixture in `test/handoff-reply-twice.test.ts` (`25dc4d8220`, `5530921f33`) | n/a | In `packages/omo-native` this only updates a TS handoff fixture (`5530921f33` regenerates `service-auth.generated.js`); `remote-ops.ts` (`followRemote`) belongs to `thread-remote` |
| #14930 pin closed `omo-relay-daemon` binary fixture to 0.1.45 (`1b60cd7b79`) | n/a | `mhc` ships native `mhc-relayd` and does not download signed `omo-relay-daemon` binary releases (`device-service-core/parity.d/5179.md`) |
| release, senpi dependency pins and generated `service-auth.generated.js` bundle regenerations (`8dbccd1284`, `7a98fc529d`, `855ca364df`, `e7cad484dc`, `fa3c0e61e7`, `915942aacb`, `5687a833eb`, `63be4a4ae3`, `7351756e1c`, `7d20fe0b70`, `4fda0171b4`, `b32ba1fd39`, `3b6061d97b`, `ee73f9884d`, `da01296dc5`, `bd8df618cb`, `ac17fd1633`, `30cea69415`, `abbca69523`, `7f068c4d32`, `877fe926ec`, `2172c87781`, `dc61f78b2b`) | n/a | JS package version, changelog, provider-map and generated bundle artifacts with no Rust counterpart |
