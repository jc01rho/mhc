# maho-service-endpoints provenance

MIT, written for mhc (no upstream file is translated). It implements the behavior omo describes for
its hosted-service call sites and the local-only mark of omo#13440 (`device-service-core/src/local-only.ts`,
omo v5.1.71), with mhc's divergence that no hosted origin is assumed (docs/divergences.md SD-21).

## Hosted endpoints in the workspace (inventory at origin/main 31af3dd04)

| Site | Host | Now |
| --- | --- | --- |
| `crates/maho-feedback/src/reports.rs` | `api.omo.dev` (report intake) | resolver; local save when unset |
| `crates/omo/components/maho-omo-feedback/src/{index,runtime,submit}.rs` | `api.omo.dev` (report intake, sign-in token origin) | resolver; local save when unset |
| `crates/omo/components/maho-omo-memory/src/device_session/account_source.rs` | `api.omo.dev` (memory-sync account) | resolver; signed-out device when unset |
| `crates/omo/telemetry-core/src/otel_traceparent_fetch.rs` | `t.omo.dev`, `api.omo.dev` (trace context hosts) | only the configured collector and service hosts |
| `crates/omo/telemetry-core/src/otlp_config.rs` | `t.omo.dev` (OTLP collector default) | unchanged: telemetry is off unless the user opts in (SD-6) |
| `crates/omo/get-worker/src/*` | `get.omo.dev` | unchanged: the server side of the download worker, not a client |
| `crates/omo/get-worker/src/scripts.rs` | `omo.dev/docs` | unchanged: a documentation link |
| `crates/omo/memory-core/src/sync/genesis/vector_tests.rs` | `recovery.omo.dev` | unchanged: a test vector's relying-party id |

No `signin.omo.dev` reference exists in the workspace.

## Rules

* Resolution: an explicit setting, then `MAHO_SERVICE_API_URL`, then `OMO_SERVICE_API_URL`; empty
  counts as unset; none is local mode. A set value the guard refuses is an error naming the setting,
  never a fall back to another origin.
* Guard: HTTPS; plain HTTP only to loopback (`127.0.0.0/8`, `::1`, `localhost`) or a tailnet host
  (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`, `*.ts.net`); no credentials, path, query or fragment.
* Local-only mark: process-wide, never cleared; a marked process refuses every network call site
  with `local_only_network_refused` and the site's name, before any I/O.
