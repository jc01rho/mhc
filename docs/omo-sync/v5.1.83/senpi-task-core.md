# omo v5.1.80..v5.1.83 sync: senpi-task-core

Range: oh-my-openagent `822bd923038..69a1863a457` (v5.1.79 to v5.1.83; v5.1.80 was never released).
Upstream paths: `packages/senpi-task` (excluding `src/{runners,store,platform,senpi}`) and `packages/omo-senpi/src/components/task`, 30 non-merge commits.
mhc counterpart: `crates/omo/senpi-task` (`dag`, `category`, `manager`, `tools/task`), `crates/omo/components/maho-omo-task`.

> Status: draft created before implementation; verdicts will be verified and updated with final symbol/test locations after porting.

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
| #14427 (b9c468be2f, f9b841f85d, 7ca726e3f2) | port | crates/omo/senpi-task/src/category/{builtins,fallback_chains,resolver}.rs + category/task tests (GPT-6.2 exact defaults, gates, exact_served_rungs) |
| #14367 (dc314011c1, 320fee78d2) | port | crates/omo/senpi-task/src/tools/task/{validation,description,params}.rs, crates/omo/components/maho-omo-task/tests/golden/{workflow,dag}-schema.json + tests (agent_with_model refusal for builtin agents) |
| #14291 (e35927e759) | ported | crates/omo/senpi-task/src/tools/task/description.rs, description_mcp_cap_tests.rs (rules already precede category catalogue) |
| #14292 review (6a73675a8d, test) | port | crates/omo/senpi-task/src/tools/task/description_mcp_cap_tests.rs (assert catalogue starts <= 4096 chars and forms the tail) |
| #14292 review (6a73675a8d, EQ goldens) | n/a | TS fused-equivalence-golden darwin-arm64/linux-x64 E21-E24 fixtures only |
| #12107 / #14553 (6333851cde, 77b004b9c5 recovery slice) | port | crates/omo/senpi-task/src/dag/recovery.rs, crates/omo/senpi-task/src/manager/manager.rs + recovery tests (keep queued children live, re-admit orphaned unstarted pending nodes) |
| #14503 (0273d6b027, store.ts) | port | crates/omo/senpi-task/src/dag/store.rs (DagStorePaths::session_runs_lock used by write_checkpoint_within_session_limit) + store_tests.rs |
| #11632 / #14495 / #14503 test doubles (a335412bb8, 923105d30e, 9e1a91a9dc, 916c8b4937, e91b1516fd, 2e3773179c, 52e9574915, 4f00b2a657, 0273d6b027 in-memory-store) | n/a | TS Bun test-speed in-memory DagFileStore/TaskRecordStore doubles and knip.workspace.jsonc; mhc Rust tests run directly against DagFileStore/TaskRecordStore on TempDir |
| #14607 (8c87fd0438, 1a0471480b, a74db808d7) | n/a | TS test fixture isolation from shared ~/.omo/agent; mhc DagStoreConfig and TaskRecordStore default to project_dir/.senpi/tasks and tests use tempfile::tempdir() |
| #14441 / #14460 (db42f0a6a7) | n/a | TS test import rename SENPI_ASTRA_ULTRAWORK_DIRECTIVE -> SENPI_GPT_ULTRAWORK_DIRECTIVE in dag-completion-model-routing.test.ts (ultrawork-skills domain; no counterpart test in maho-omo-task) |
| 3d4af11b30 | n/a | In senpi-task, only touches src/senpi/* (owned by senpi-task-runners) and TS package.json |
| senpi pins & releases (0886a99a7f, ed9afe1f43, daaca92037, 28c8cc0449, a19011adbd, f23e7ad390, 5835a98e88, 77b004b9c5 packaging) | n/a | TS npm package.json, changes.md, and bun.lock version bumps; mhc pins in PINS.md (owned by lead) |
