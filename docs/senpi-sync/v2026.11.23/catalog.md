# senpi v2026.11.23 sync ledger — slice `catalog`

Range: `f01af134919f..47333c26cdba` (`v2026.11.21-2` to `v2026.11.23`).
Input specification: `tracks/senpi-engine.md` / `slices/catalog.md` (3 bundled model catalog refresh PRs).

## Summary

- `port`: 0
- `ported`: 0
- `golden`: 3 (`#7179`, `#7204`, `#7379`)
- `gap`: 0
- `n/a`: 0

## Catalog Diff Summary (`v2026.11.21-2` -> `v2026.11.23`)

- `tools/golden/pin.mjs`: `SENPI_CATALOG_PIN` updated to `47333c26cdba96631d64317284a06ca6e0aab68e` (`v2026.11.23`); `SENPI_PIN` (`f01af134919f0bbf24b2887000e2142dfd290567`) unchanged.
- `crates/maho-ai/data/models.json`: regenerated via `SENPI_SRC=/tmp/senpi-v2026.11.23 bun tools/golden/gen-models.mjs` (42 providers in `models.json` + `meta` injected from `src/providers/data/meta.json` = 43 builtin chat providers; 1,992 chat models in `models.json` + 7 `meta` models = 1,999 builtin chat models, net +1 from 1,998).
  - `opencode-go` (38 -> 39): +1 added (`longcat-2.5`), 0 removed, 2 modified (`longcat-2.0` and `longcat-2.5-free` marked `deprecated: true`).
  - `opencode` (99 -> 99): 0 added, 0 removed, 3 modified (`deepseek-v4.1-flash` and `glm-5.3` token pricing lowered; `longcat-2.5-free` marked `deprecated: true`).
  - `opengateway` (84 -> 84): 0 added, 0 removed, 2 modified (`deepseek/deepseek-v4.1-flash-ultrafast` and `z-ai/glm-5.3-flash-ultrafast` base and `>256k` tier token pricing updated).
  - `openrouter` (416 -> 416): 0 added, 0 removed, 15 net modified (`~deepseek/deepseek-pro-latest`, `~moonshotai/kimi-latest`, `deepseek/deepseek-v4.1-flash-1017`, `google/gemma-3-27b-it`, `moonshotai/kimi-k3.1-pro`, `openai/gpt-oss-120b`, `openai/gpt-oss-20b`, `particle-ai/particle-1`, `qwen/qwen3.5-27b`, `qwen/qwen3.6-35b-a3b`, `qwen/qwen3.8-2.4t-a95b`, `qwen/qwen3.8-27b`, `tencent/hy3`, `thinkingmachines/inkling-2-mini`, `z-ai/glm-4.7`; `z-ai/glm-5.1` was touched in `#7179` and reverted to its prior values in `#7204`).
- `crates/maho-ai/src/providers/data/model_data_manifest.json`: updated to `v2026.11.23` `packages/ai/src/providers/data/.manifest.json` (`generatedAt: "2026-11-23T02:28:43.265Z"`, epoch `1_795_400_923_265`, `structureHash: "3b550860e840fc3c0f8441b5deda5d4c6a99f3fc1706d2dc96d2235742e60f1e"`).
- `crates/maho-ai/data/image-models.json` (2 providers, 80 models), `crates/maho-ai/src/models_generated.rs` (`CLASSIFIER_MODELS_JSON`: 5 providers, 75 models), `crates/maho-ai/src/providers/data/meta.json` (7 models), and `crates/maho-ai/src/providers/data/radius.json` (23 models): verified identical at `v2026.11.23`.

## Verdicts

| senpi PR | verdict | mhc location or reason |
| --- | --- | --- |
| `#7179` (`3db1d1604` merge; commits `a2e903de9`, `e7a86c281`) | `golden` | regenerated catalog data in `crates/maho-ai/data/models.json` and `crates/maho-ai/src/providers/data/model_data_manifest.json` (`openrouter` refresh; `z-ai/glm-5.1` subsequently re-updated in `#7204`) |
| `#7204` (`9b7bbfc2f` merge; commit `6d468cce8`) | `golden` | regenerated catalog data in `crates/maho-ai/data/models.json` and `crates/maho-ai/src/providers/data/model_data_manifest.json` (`opencode` 3 modified, `opencode-go` +1 added `longcat-2.5` and 2 deprecated, `openrouter` 16 modified), `crates/maho-ai/src/models/tests.rs` (`catalog_matches_senpi_generated_counts_and_order`: 1,999 models + `longcat-2.5` / deprecation assertions), `crates/maho-ai/src/providers/tests.rs` (`get_builtin_model_data_generated_at` = `1_795_400_923_265`) |
| `#7379` (`e2e48025c` merge; commits `27db87655`, `db4c54f6a`) | `golden` | regenerated catalog data in `crates/maho-ai/data/models.json` and `crates/maho-ai/src/providers/data/model_data_manifest.json` (`opengateway` `deepseek/deepseek-v4.1-flash-ultrafast` and `z-ai/glm-5.3-flash-ultrafast` pricing and tier updates) |
