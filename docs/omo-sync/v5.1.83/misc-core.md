# omo v5.1.80..v5.1.83 sync ledger — `misc-core` track

Source range: `822bd923038` (`v5.1.79`) .. `69a1863a457` (`v5.1.83`)
Upstream paths: `packages/omo-config-core`, `packages/model-core`, `packages/get-worker`, `packages/claude-code-compat-core`, `packages/omo-senpi/src/components/{agent-home,telemetry,native-badge,claim-receipt}` (21 non-merge commits across 13 PRs/commit groups)

## Summary

| verdict | count |
|---|---:|
| port | 10 |
| ported | 1 |
| gap | 0 |
| n/a | 2 |
| **total** | **13** |

## Per-PR verdicts

| omo PR | verdict | mhc location or reason |
|---|---|---|
| #14389 (`68f6df4838`, re-lands #14392) | port | `crates/omo/omo-config-core/src/schema/keyword_detector.rs` (`KeywordType`, `omo_keyword_detector_settings_schema`), `mod.rs`, `config.rs` (`keyword_detector` on all four schemas), `tests/schema_5183.rs` |
| #14430 (`293610e2a2`) | port | `crates/omo/omo-config-core/src/schema/operator_alerts.rs` (`omo_operator_alerts_settings_schema`, `ResolvedErrorStreakSettings`, `resolve_error_streak_settings`), `internal/validate.rs` (`Pattern::EnvVarName`, `env_var_name_string`), `schema_json.rs`, `mod.rs`, `config.rs`, `tests/schema_5183.rs` |
| `0031186084` (no PR) | port | `crates/omo/omo-config-core/tests/schema.rs`, `tests/schema_5179.rs` (update `openai/gpt-6-astra` test fixtures to `openai/gpt-6.2-astra`) |
| #14442 (`f87b57c4b3`) | port | `crates/omo/model-core/src/model_family_detectors.rs` (`is_deepseek_v41_flash_model`), `tests/parity/model_family_detectors.rs` |
| `db42f0a6a7` (no PR) | port | `crates/omo/model-core/src/model_family_detectors.rs` (`is_gpt6_astra_model` accepts `-ultrafast`), `tests/parity/gpt_6_2.rs`; the sibling `omo-native-prompt.test.ts` `ARMING_LEDGER_KEY` global-symbol fixture change is TS-only (`maho-omo-telemetry` calls `maho_ultrawork::classify_with_session_role` directly) |
| `b9c468be2f` (no PR) | port | `crates/omo/model-core/src/category_model_requirements.rs` (`ultrabrain`, `deep-low`, `deep-high` single-rung exact GPT-6.2 chains with `requires_any_model: Some(true)`), `tests/parity/{category_routing_policy,gpt_5_6_copilot_resolution,gpt_6_2,model_requirements,model_requirements_categories}.rs` |
| `70d3e049d9` (no PR) | port | `crates/omo/components/maho-omo-claim-receipt/tests/invocations.rs` (`mengmota` -> `studio-mac`; `crates/omo/model-core/src/context_overflow.rs` already uses the neutral senpi repository header comment) |
| `ca7e2139e3` (no PR) | port | `crates/omo/components/maho-omo-telemetry/src/product_identity.rs` (`BUILTIN_SKILL_NAMES` adds `"omo-relay"`, propagated into `event_schemas.rs` `omo_native_event_schemas()`) |
| #14344 (`edd8d0734e`, `1baf721b91`) | ported | `crates/omo/get-worker/assets/install.sh` (`refuse_sudo_install`) and `crates/omo/get-worker/tests/parity/install_sh_root_guard.rs` (8 tests, registered in `tests/parity/main.rs`) |
| #14622 (`5ad49782d7`, `0c56060059`, `27731e71d4`, `3d29c8e416`, `5546223cc6`, `30b3a2efd6`, `98657b14a4`; reviews M2-M9, round 3) | port | `crates/omo/components/maho-omo-agent-home/src/account_home.rs` (`PlatformFacts`, `user_record_home`: passwd by uid on Linux, `id -P` on macOS, System32 `whoami.exe`/`reg.exe` profile path on Windows, `None` when unreadable) and `src/dev_build_runtime.rs` (`is_dev_build_runtime`, `account_homes`, `is_shared_agent_dir`, `is_shared_store_file`: every account home counts, device+inode identity, dangling links resolved, case folded on macOS/Windows, unknown homes fail closed). Consumed by `maho-omo-gateway/src/thread/store_ops.rs` (`migrate_guarded`, `source_build_migration_refusal`, `StoreError::Migration`, code `gateway_migration_failed`). Unit tests live beside both modules; the gateway cases are in `store_ops.rs`. Differences: the dev-build marker is still the omob `OMO_PACKAGE_DIR` payload stamp (no mhc binary sets it, so the guard only fires for an omob-launched runtime sharing the store); child tools run without upstream's 5 s timeout and Windows `windowsHide`; Bun's `homedir()` quirk has no Rust counterpart. |
| #14382 (`5036654c32`) | port | `crates/omo/components/maho-omo-agent-home/src/dev_build_runtime.rs` (`is_dev_build_runtime` moved out of the native badge into agent-home, as upstream did), consumed by the gateway migrate guard (see #14622 above). The `native-badge` caller is n/a (mhc has no such component). The reader floor itself, which needs gateway schema v11-v14, stays a gap in `thread-gateway.md`. |
| `5b9d744f37`, `b812183af3` (no PR) | n/a | TS Bun tests wiring `createDefaultPostHogTransport` against `Bun.serve` to assert `flushAt === 20` isolation from the `flushAt === 1` legacy ping; `crates/omo/components/maho-omo-telemetry/src/omo_native_summary_wiring.rs` tests `NativeCaptureState::publish` via in-memory `CaptureFn` closures without constructing `PostHogTelemetryTransport` |
| `041a683066` (no PR) | n/a | TS Bun `beforeEach`/`afterEach` shared `TEST_DIR` cleanup fix in `claude-code-mcp-loader/loader.test.ts`; Rust tests in `crates/omo/claude-code-compat-core/src/features/claude_code_mcp_loader/loader.rs` already isolate every test with `tempfile::tempdir()` |
