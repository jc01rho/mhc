# omo v5.1.80..v5.1.83 sync ledger — `device-service` track

Source range: `822bd923038` (`v5.1.79`) .. `69a1863a457` (`v5.1.83`)
Upstream paths: `packages/device-service-core` (4 non-merge commits across 3 PRs/entries)
mhc counterpart: `crates/omo/device-service-core`

## Summary

| verdict | count |
|---|---:|
| port | 3 |
| ported | 0 |
| gap | 0 |
| n/a | 1 |
| **total** | **4** |

## Per-PR verdicts

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #14271 (`3ce9c69c30`, `fc57619930`) | port | `crates/omo/device-service-core/src/engine_endpoint.rs` (`ENDPOINT_PRIMARY_MIN_SERVICE_VERSION`, `EngineEndpoints`, `engine_endpoints_of`), `src/release/version.rs` (`is_semver`, `compare_versions`), `src/local_api/client.rs` (`LocalApiClient::engine_endpoints`), `src/lib.rs`, and `tests/engine_endpoint.rs`. The `DeviceService` wrapper (`device-service.ts`) is not ported in mhc (callers use `LocalApiClient` directly), and the `find_session` fallback caller (`packages/omo-senpi/src/components/thread/remote/find-session.ts`) belongs to `thread-remote`. |
| `bbab929dff` (`feat/14364-omo-sdk-entry` slice) | port | `crates/omo/device-service-core/src/hand_device_keys.rs` (`CancelController`, `CancelSignal`, `DeviceServiceStatus`, `StatusClient`, `device_service_endpoint_path`, `read_device_service_status`, `read_device_service_status_with`), `src/lib.rs` re-exports, and `tests/read_status.rs` (3 cases ported from `test/read-status.test.ts` plus live/dead/insecure-state-dir handshake cases). `handDeviceKeys` / `connectOrNotRunning` remain unported (mhc has no `DeviceService` wrapper or credential-adoption client here). |
| #14510 listing rows 6-7, policy, insecure cause (`17ad3cb68c`, client/protocol slice) | port | `crates/omo/device-service-core/src/listing_rows.rs` (`NewestRowsListing` asking `rows: 7` and stepping down `7 -> 6 -> 5 -> 4 -> 3 -> 2 -> None` on `invalid_request`), `src/local_api/policy.rs` (`Policy`, `PolicyBlocker`, `PolicyState`, `ThisDeviceState`, `PolicyWaitingState`, `PolicyWaitReason`), `src/local_api/protocol.rs` (`Status.policy`, `ListingRow` fields `archived`/`settled`/`settled_by`/`waiting`/`engine_version`, `ListingWait`, `ListingWaitKind`, `ListingSettledBy`, `validate_listing_row`, `validate_sessions_list_rows`), `src/local_api/errors.rs` (`InsecureStateCause`, `LocalApiError.insecure_cause`), `src/local_api/access.rs` (`SecureOrigin`, `make_dir`, POSIX `insecure_cause`), `src/local_api/vocabulary.rs` (`COMMAND_REFUSAL_REASONS` with `"unbound"`, `WAIT_KINDS`, `LISTING_SETTLED_BY_KINDS`, `LISTING_ROWS`), and tests `tests/device_service_rows.rs` + `tests/la23_listing_row_holder.rs`. |
| #14510 Windows DACL, PowerShell, and release acquire (`17ad3cb68c`, `dacl.ts`, `powershell.ts`, `secure-windows.ts`, `release/acquire.ts`, `release/windows-dacl.ts`, `PIN.json`) | n/a | `crates/omo/device-service-core` explicitly excludes the Windows PowerShell/DACL verifier and `omo-relay-daemon` binary release downloader (`parity.d/5179.md`): Windows uses the loopback TCP endpoint directly and mhc ships `mhc-relayd` rather than downloading upstream daemon binaries. |
