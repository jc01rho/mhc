# senpi v2026.11.23 sync ledger — slice `codemode`

Range: `f01af134919f..47333c26cdba` (`v2026.11.21-2` to `v2026.11.23`).
Input specification: `tracks/senpi-engine.md` / `slices/codemode.md` (2 items: `senpi#7201` via merge `d8c9366aa` [`6eb4dcefc`, `a5f989862`, `8cd001c88`] and `senpi#7206` via merge `22fb7dc79` [`8a7b63e03`]).

## Summary

- `port`: 1 (`#7201`)
- `ported`: 0
- `golden`: 0
- `gap`: 0
- `n/a`: 1 (`#7206`)

## Verdicts

| senpi PR | verdict | mhc location or reason |
| --- | --- | --- |
| `#7201` (`d8c9366aa`: `6eb4dcefc`, `a5f989862`, `8cd001c88`) | `port` | `crates/maho-codemode/assets/kernels/js/worker-runtime.js`, `crates/maho-codemode/assets/UPSTREAM.json`, `crates/maho-codemode/src/tool/js_cell_parse_message.rs`, `crates/maho-codemode/src/tool/mod.rs`, `crates/maho-codemode/src/tool/run_eval_cell.rs`, `crates/maho-codemode/tests/eval_js_parse_diagnostic.rs`, `crates/maho-codemode/tests/main.rs`, `crates/maho-codemode/parity.d/20261123c.md` (compile-time `SyntaxError` tagged `js_cell_parse_error` in the JS worker; `js_cell_parse_message` locates parse errors in the submitted cell source via `Bun.Transpiler` with 1-based line and column, ±60 UTF-16 code-unit excerpt, caret line, and quoted-delimiter shell heredoc advice) |
| `#7206` (`22fb7dc79`: `8a7b63e03`) | `n/a` | `packages/senpi-codemode/test/gate/allowlist.json` only (senpi TS import-observer and legacy-contract gate allowlist for `#7201`; `mhc` has no `test/gate/allowlist.json` counterpart) |
