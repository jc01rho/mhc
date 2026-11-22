# senpi v2026.11.21-2 sync ledger — track `codemode`

Range: `59d58cee5..f01af13491` (`v2026.10.27-2` to `v2026.11.21-2`).
Input specification: `tracks/codemode.tsv` (50 rows).

## Summary

- `port`: 7 (`#4797`, `#5150`, `#5155`, `#5496`, `#5945`, `#6393`, `#6830`)
- `ported`: 4 (`#6131`, `#6276`, `#6887`, `#6978`)
- `golden`: 2 (`#4639`, `#7054`)
- `n/a`: 37

## Verdicts

| senpi PR | verdict | mhc location or reason |
| --- | --- | --- |
| `direct` (`fe3b465e5b`) | `n/a` | upstream merge commit touching Python kernel `path_identity_windows.py`, Python canonical parity tests, TS `trust-manager.ts` and senpi `.github/workflows/ci.yml`; `maho-codemode` ships only the `bun/js` kernel |
| `#3197` (`9d602fa05a`) | `gap` | `packages/senpi-codemode/src/kernels/js/group-signal-notice.js` and `test/gate/allowlist.json`; `group-signal-notice.js` is recorded as `"status": "unported"` in `crates/maho-codemode/assets/UPSTREAM.json` (pre-range port gap) |
| `#4110` (`5bad49b549`) | `n/a` | vitest-only warm-up hook in `packages/senpi-codemode/test/js-kernel-webview-readiness.test.ts` for senpi Windows CI timing |
| `#4387` (`4298cc17cd`) | `gap` | JS eval kernel permission core (`interpose.js`, `permission-*`); recorded as `"status": "unported"` in `crates/maho-codemode/assets/UPSTREAM.json` and `crates/maho-codemode/parity.d/5179c.md` |
| `#4390` (`f6f6cf2114`) | `gap` | kernel tool invoker authority (`invocation-authority.ts`, `permission-host.ts`) paired with the unported `#4387`/`#4391` eval permission interposition stack (`crates/maho-codemode/parity.d/5179c.md`) |
| `#4391` (`71e556a373`) | `gap` | JS eval kernel permission interposition (`interpose-*.js`, `path-identity-*`); recorded as `"status": "unported"` in `crates/maho-codemode/assets/UPSTREAM.json` and `crates/maho-codemode/parity.d/5179c.md` |
| `#4392` (`b8735d6ed0`) | `gap` | Python eval kernel permission interposition (`kernels/py/interpose.py`, `path_identity*.py`, `permission_bridge.py`, `bridge/permission-check.ts`); `maho-codemode` ships only the `bun/js` kernel |
| `#4639` (`c61ea83a5d`) | `golden` | `.agents/skills/senpi-qa/scenarios/fused-equivalence/**` golden fixtures and senpi QA equivalence harness |
| `#4797` (`e0877ef6e1`) | `port` | `crates/maho-codemode/assets/kernels/js/worker-webview.js`, `crates/maho-codemode/assets/kernels/js/webview-host.js`, `crates/maho-codemode/assets/UPSTREAM.json`, `crates/maho-codemode/tests/webview/kernel-webview.test.js` (route all worker-mode WebViews including WebKit through the main-thread service; `goBack`/`goForward` wire methods; `WebViewNotReadyError` message) |
| `#4895` (`6eb5e3af13`) | `gap` | `packages/senpi-codemode/src/kernels/py/path_identity.py` and `test/py-canonical-parity.test.ts`; Python kernel is not shipped in `maho-codemode` (`bun/js` only) |
| `#4928` (`5bc094377e`) | `n/a` | `packages/senpi-codemode/test/gate/allowlist.json` only (senpi TS import-observer gate allowlist) |
| `#5054` (`8160a7e4a6`) | `n/a` | senpi `.github/workflows/ci.yml` and `test/js-kernel-webview.test.ts` test-only adjustment (`goBack`/`goForward` covered by `crates/maho-codemode/tests/webview/kernel-webview.test.js` in `#4797`) |
| `#5072` (`a7b162f80f`) | `n/a` | `packages/senpi-codemode/test/gate/allowlist.json` only (senpi TS gate baseline) |
| `#5150` (`14cbefa489`) | `port` | `crates/maho-codemode/src/tool/eval_argument_validator.rs`, `crates/maho-codemode/src/tool/eval_request.rs`, `crates/maho-codemode/src/tool/eval_tool.rs`, `crates/maho-codemode/tests/eval_request.rs`, `crates/maho-ai/src/utils/tool_schema_compat.rs` (teaching errors for missing `language`/`code`/`summary` run fields; `REQUIRED for <branch>` description hints on merged root object unions) |
| `#5155` (`2783d82aac`) | `port` | `crates/maho-codemode/src/tool/detached_cell_manager.rs`, `crates/maho-codemode/src/extension/eval_cells_rpc.rs`, `crates/maho-codemode/tests/detached_cell_manager.rs` (`closest_cell_id` edit-distance `did you mean "<id>"?` suggestion on unknown detached cell IDs) |
| `#5157` (`27d0c86beb`) | `gap` | `eval-language-inference.ts` (`py` vs `js` code-marker language classification and `WeakSet`-backed argument mutation during validation); `maho-codemode` supports only `"js"` and `maho-ai`'s `ToolArgumentValidator` takes `&Value` without mutating caller arguments |
| `#5158` (`cdef7f5d20`) | `n/a` | renames embedded `skill/bun-1-4` to `skill/bun`; mhc intentionally keeps `crates/maho-codemode/skill/bun-1-4/**` as pinned in `crates/maho-codemode/parity.d/9500.md`, `crates/maho-codemode/assets/UPSTREAM.json` and `crates/maho-codemode/tests/extension_bindings.rs` |
| `#5181` (`d18515862e`) | `n/a` | `SubprocessKernel`/`SubprocessProcess` Julia spawn failure classification (`neverStarted` -> `KernelStartupError`); `crates/maho-codemode/parity.d/9700.md` marks `subprocess-kernel.ts` `n/a` (Julia/Python/Ruby subprocess kernels not shipped), and Rust `Command::spawn` fails synchronously before `SubprocessProcess` is constructed |
| `#5183` (`fae7f34473`) | `n/a` | `packages/senpi-codemode/src/kernels/js/cell-ownership.js` and vitest timeout adjustments; `cell-ownership.js` is recorded as `"status": "unported"` in `crates/maho-codemode/assets/UPSTREAM.json` |
| `#5208` (`b45239dea0`) | `n/a` | `packages/senpi-codemode/test/gate/{allowlist,baseline}.json` only |
| `#5496` (`44a00451b3`) | `port` | `crates/maho-codemode/src/kernels/js/local-module-loader.js`, `crates/maho-codemode/tests/local_module_loader.rs` (`file://` module specifiers normalize through `new URL(specifier).href` and bypass `localRootUrls` and `resolvePackage`) |
| `#5658` (`cd621cf7fc`) | `n/a` | `packages/senpi-codemode/test/gate/allowlist.json` only |
| `#5854` (`4544bd1293`) | `n/a` | `packages/senpi-codemode/test/kill-guard-kernels.test.ts` only (tests Python/Ruby/Julia kill-guard kernels not shipped in mhc) |
| `#5918` (`29b3019df7`) | `n/a` | `packages/senpi-codemode/test/js-kernel-webview.test.ts` vitest-only Chrome warm-up helper |
| `#5945` (`770463bf84`) | `port` | `crates/maho-codemode/assets/kernels/js/webview-host.js`, `crates/maho-codemode/assets/UPSTREAM.json`, `crates/maho-codemode/tests/webview/webview-readiness.test.js` (`WebViewService` deduplicates per-client `#drop` promises in `#drops` WeakMap and deletes `#clients` in `finally` after in-flight launches settle and Chrome retires) |
| `#5950` (`e00707803f`) | `n/a` | `packages/senpi-codemode/test/js-kernel-webview.test.ts` vitest-only per-backend warm-up error deferral |
| `#6009` (`f894010388`) | `n/a` | prose edits under `packages/senpi-codemode/src/skill/bun/**` (follow-up to `#5158`; mhc pins `skill/bun-1-4/**` in `UPSTREAM.json`) |
| `#6095` (`1e726478e3`) | `n/a` | `defaultJsIsolation` in `feature-settings.ts` and `runtime-name.ts`: selects `"process"` only when the host executable itself is `"bun"` on POSIX (`host.executable === "bun" && host.platform !== "win32"`), while compiled binaries keep `"worker"`; mhc is always a compiled native binary, so `JsIsolation::Worker` is always the default |
| `#6131` (`b666f291d5`) | `ported` | Round-1 PR `jc01rho/mhc#35` (`a3c44520a`): `crates/maho-codemode/assets/kernels/js/power-throttling.js`, `power-throttling-startup.js`, and startup imports in `worker-entry.js`, `inline-worker-entry.js`, and `process-entry.js` (`crates/maho-codemode/parity.d/5179c.md`) |
| `#6168` (`fc42bed0a9`) | `gap` | live nested tool-update streaming (`cell-tool-updates.ts`, `bridge-cell-binding.ts`, `nested-tool-call.ts`, `nested-tool-output.ts`, `jl`/`rb` runner tokens); relies on `NestedToolOutput` live buffer and subprocess kernel bridge tokens not used by mhc's `EvalOutputCollector`-backed `CellResultBuilder` |
| `#6182` (`d2225ca510`) | `n/a` | `packages/senpi-codemode/test/gate/allowlist.json` only |
| `#6232` (`0bf16b0101`) | `n/a` | prose edits under `packages/senpi-codemode/src/skill/bun/**` (follow-up to `#5158`; mhc pins `skill/bun-1-4/**` in `UPSTREAM.json`) |
| `#6259` (`ad44857adc`) | `n/a` | follow-up to `#6168` in `nested-tool-output.ts` and `cell-runtime.ts` (`NestedToolOutput.releaseSettled`); `NestedToolOutput` is not used in mhc |
| `#6276` (`749ae14d9b`) | `ported` | Round-1 PR `jc01rho/mhc#35` (`a3c44520a`): `crates/maho-codemode/src/extension/reachable_tools.rs`, `src/extension/kernel_preludes.rs`, `src/extension/runtime_factory.rs`, `src/bridges/schema_bridge.rs` (`crates/maho-codemode/parity.d/5179c.md`) |
| `#6317` (`bd185cf4b1`) | `n/a` | `packages/senpi-codemode/test/gate/allowlist.json` only |
| `#6349` (`090ff49d3e`) | `n/a` | `packages/senpi-codemode/test/gate/allowlist.json` only |
| `#6377` (`b678422dd9`) | `gap` | `packages/senpi-codemode/src/kernels/py/{interpose.py,sqlite_uri.py}`; Python kernel is not shipped in `maho-codemode` (`bun/js` only) |
| `#6393` (`c330760b63`) | `port` | `crates/maho-codemode/src/extension/runtime_factory.rs`, `crates/maho-codemode/tests/extension_factory.rs` (`TOP_LEVEL_ONLY_TOOL_REFUSALS` rejects nested `show_html_page` calls inside `eval` with `ExecuteToolErrorCode::Blocked`) |
| `#6404` (`7883ad34ba`) | `n/a` | `packages/senpi-codemode/test/gate/allowlist.json` only |
| `#6406` (`7fa054dd67`) | `n/a` | `PlainTextComponent.#holdLiveHeight` in `render-blocks.ts` for variable-height live nested tool widgets from `#6168`; in mhc `EvalCard` is recreated per render and `render_live_cell_frame` already uses fixed `LIVE_BODY_ROWS = 6` (`crates/maho-codemode/src/tool/render.rs:1058`), plus `tests/render_golden.rs` pins the unpadded line arrays in `tests/golden/codemode-render.json` |
| `#6830` (`aaac4fc6cb`) | `port` | `crates/maho-codemode/assets/kernels/js/worker-shell-capture.js`, `crates/maho-codemode/assets/UPSTREAM.json` (`capturedSpawn` and `capturedSpawnSync` pin `process.env` when `spawnOptions.env == null` even after the cell becomes inactive; host credential-proxy lows were ported in Round-1 PR `jc01rho/mhc#35`) |
| `#6854` (`71691559ad`) | `n/a` | `packages/senpi-codemode/test/gate/{allowlist,baseline}.json` only |
| `#6887` (`5eba336a70`) | `ported` | Round-1 PR `jc01rho/mhc#35` (`a3c44520a`): `ToolInfo.eval_only`, `reachable_tools.rs`, `eval-prompt-template.txt`, `eval_prompt.rs`, `tests/attest_and_prelude.rs` (`crates/maho-codemode/parity.d/5179c.md`) |
| `#6892` (`465bf9d924`) | `n/a` | `packages/senpi-codemode/test/gate/{allowlist,baseline}.json` and senpi QA `E29.json` golden update |
| `#6903` (`9409fd4c73`) | `gap` | passes `ToolDefinition.renderCall`/`renderResult` through TS `ToolInfo` and `EvalSchemaToolInfo` to `tool-widgets.ts`; in mhc `maho_ext_api::ToolInfo` is a serializable metadata struct without renderer closures (`crates/maho-ext-api/src/types.rs:4005`) and `tool_widgets.rs` renders through `maho_interactive::tools::renderers::create_all_tool_renderers()` |
| `#6940` (`35de5b1a69`) | `n/a` | `packages/senpi-codemode/test/gate/{allowlist,baseline}.json` only |
| `#6978` (`e420af9aca`) | `ported` | Round-1 PR `jc01rho/mhc#35` (`a3c44520a`): `crates/maho-codemode/src/kernels/shared/session_spawn_attestation.rs`, `subprocess_process.rs`, `crates/maho-tools/src/session_spawn_attestation.rs`, `crates/maho-cli/src/main.rs` (`session-of-pid`), `crates/maho-codemode/tests/attest_and_prelude.rs` (`crates/maho-codemode/parity.d/5179c.md`) |
| `#6996` (`5a18c030e3`) | `n/a` | TS-only refactor injecting `AttestKernelSpawn` callback so TS kernel modules avoid importing `@code-yeongyu/senpi` at module load time; mhc links `maho_tools::session_spawn_attestation::attest_session_spawn_in_store` statically in `crates/maho-codemode/src/kernels/shared/session_spawn_attestation.rs` |
| `#7037` (`c6e8cd2acb`) | `n/a` | RPC host supervisor hand-off env scrubbing (`childEnvironmentOf`, `inheritedEnvironmentDelta`) for in-process worker threads and Python/Ruby/Julia kernels; mhc spawns every JS kernel in a child process and scrubs the environment at spawn via `with_credential_proxy` and `scrub_host_env` (`crates/maho-codemode/src/kernels/js/{worker_host,process_worker}.rs`) |
| `#7054` (`52a289f06e`) | `golden` | `.agents/skills/senpi-qa/scenarios/fused-equivalence/golden/**` re-recorded QA equivalence goldens and gate allowlist |

Lead audit (2026-11-22): 10 rows first recorded as `n/a` are re-labelled `gap`: the upstream change is a real capability that mhc lacks because the subsystem it lives in is not ported. The track's own summary line above predates this audit.
