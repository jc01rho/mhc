# omo v5.1.80..v5.1.83 sync: senpi-task-core

Range: oh-my-openagent `822bd923038..69a1863a457` (v5.1.79 to v5.1.83; v5.1.80 was never released).
Upstream paths: `packages/senpi-task` (excluding `src/{runners,store,platform,senpi}`) and `packages/omo-senpi/src/components/task`, 30 non-merge commits.
mhc counterpart: `crates/omo/senpi-task` (`dag`, `category`, `manager`, `tools/task`), `crates/omo/components/maho-omo-task`.

## Summary

| verdict | rows |
|---|---:|
| port | 5 |
| ported | 1 |
| n/a | 6 |
| gap | 0 |
| total | 12 |

## Per PR

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #14427 (b9c468be2f, f9b841f85d, 7ca726e3f2) | port | `crates/omo/senpi-task/src/category/builtins.rs` (`BuiltinCategoryDefinition::exact_models`, `GPT_ASTRA_GATE_MODELS`, `DEEP_LOW_GATE_MODELS`, `exact_served_rungs`, private `is_exact_model_category`), `fallback_chains.rs` (single-rung `ultrabrain`, `deep-low`, `deep-high`), `resolver.rs` (`exact_rungs`, exact `builtin_default_model`, `fallback_entry` restoration). Tests: `category_tests.rs`, `chains_tests.rs`, `gating_tests.rs`, `openai_categories_tests.rs`, `coverage_tests.rs`, `tools/task/gated_categories_tests.rs`. |
| #14367 (dc314011c1, 320fee78d2) | port | `crates/omo/senpi-task/src/tools/task/validation.rs` (`TaskTargetErrorCode::AgentWithModel`, `validate_task_target` checking `canonical_agent_name` against `curated_readonly_agent_names` and `ulw_reviewer_agent_names`, updated `CATEGORY_WITH_MODEL_MESSAGE`), `description.rs`, `params.rs`; `crates/omo/components/maho-omo-task/tests/golden/{workflow,dag}-schema.json`. Tests: `validation_tests.rs`, `execute_spawn_validation_tests.rs`, `maho-omo-task/tests/{dag_tool,adapters}.rs`. |
| #14291 (e35927e759) | ported | `crates/omo/senpi-task/src/tools/task/description.rs` (`build_task_tool_description` already places rules before category catalogue) and `description_mcp_cap_tests.rs`. |
| #14292 review (6a73675a8d, test) | port | `crates/omo/senpi-task/src/tools/task/description_mcp_cap_tests.rs` (`given_many_agents_and_a_long_custom_category_when_built_then_only_the_category_catalogue_lies_past_the_cap`). |
| #14292 review (6a73675a8d, EQ goldens) | n/a | TS `fused-equivalence-golden` darwin-arm64/linux-x64 E21-E24 JSON fixtures only. |
| #12107 / #14553 (6333851cde, 77b004b9c5 recovery slice) | port | `crates/omo/senpi-task/src/dag/recovery.rs` (`reconcile_nodes` keeps `queued_here` pending tasks live and re-admits orphaned unstarted pending nodes via `NodeRetried` after withdrawing/cancelling), `crates/omo/senpi-task/src/manager/manager.rs` (`TaskManager::withdraw_task_admissions`). Tests: `recovery_tests.rs` (`given_an_orphaned_unstarted_pending_task_below_the_readmission_limit_when_resumed_then_recovery_cancels_it_and_readmits_the_node`, `given_a_pending_task_still_queued_in_this_host_when_resumed_then_recovery_keeps_it_attached`). |
| #14503 (0273d6b027, store.ts) | port | `crates/omo/senpi-task/src/dag/store.rs` (`DagStorePaths::session_runs_lock`, used in `write_checkpoint_within_session_limit`). Test: `store_tests.rs` (`given_parent_session_id_when_building_session_runs_lock_then_places_hashed_lock_under_locks_dir`). |
| #11632 / #14495 / #14503 test doubles (a335412bb8, 923105d30e, 9e1a91a9dc, 916c8b4937, e91b1516fd, 2e3773179c, 52e9574915, 4f00b2a657, 0273d6b027 in-memory-store) | n/a | TS Bun test-speed in-memory `DagFileStore`/`TaskRecordStore` test doubles and `knip.workspace.jsonc`; mhc Rust tests run directly against `DagFileStore`/`TaskRecordStore` on `tempfile::tempdir()`. |
| #14607 (8c87fd0438, 1a0471480b, a74db808d7) | n/a | TS test fixture isolation from shared `~/.omo/agent`; mhc `resolve_dag_state_dir` and `TaskRecordStore` default to `project_dir/.senpi/tasks`, and Rust tests already isolate state in `TempDir`. |
| #14441 / #14460 (db42f0a6a7) | n/a | TS test import rename `SENPI_ASTRA_ULTRAWORK_DIRECTIVE` -> `SENPI_GPT_ULTRAWORK_DIRECTIVE` in `dag-completion-model-routing.test.ts` (ultrawork-skills domain; `maho-omo-task` has no test importing that symbol). |
| 3d4af11b30 | n/a | In `senpi-task`, only touches `src/senpi/*` (owned by `senpi-task-runners`) and TS `package.json`. |
| senpi pins & releases (0886a99a7f, ed9afe1f43, daaca92037, 28c8cc0449, a19011adbd, f23e7ad390, 5835a98e88, 77b004b9c5 packaging) | n/a | TS npm `package.json`, `changes.md`, and `bun.lock` version bumps; mhc pins in `PINS.md` (owned by lead). |
