# omo v5.1.80..v5.1.83 sync ledger — `misc-core` track

Source range: `822bd923038` (`v5.1.79`) .. `69a1863a457` (`v5.1.83`)
Upstream paths: `packages/omo-config-core`, `packages/model-core`, `packages/get-worker`, `packages/claude-code-compat-core`, `packages/omo-senpi/src/components/{agent-home,telemetry,native-badge,claim-receipt}` (21 non-merge commits across 13 PRs/commit groups)

## Summary

| verdict | count |
|---|---:|
| port | 8 |
| ported | 1 |
| gap | 2 |
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
| #14622 (`5ad49782d7`, `0c56060059`, `27731e71d4`, `3d29c8e416`, `5546223cc6`, `30b3a2efd6`, `98657b14a4`; reviews M2-M9, round 3) | gap | Source-build guard (`isDevBuildRuntime`, `isSharedAgentDir`, `isSharedStoreFile`) and `@oh-my-opencode/omo-opencode/account-home` OS user-record reader (`userRecordHome`) require the `omob` `OMO_PACKAGE_DIR`/`omoBuild` dev-build runtime and gateway schema v11-v14 reader floor (`#14382`), which `mhc` lacks (`maho-omo-agent-home` holds only `resolve_agent_home` and `install_plugin_artifacts`; `mhc` has no `omo-opencode` crate; `maho-omo-gateway` already ported the snapshot/write-lock schema re-check in PR #70) |
| #14382 (`5036654c32`) | gap | Creates `agent-home/dev-build-runtime.ts` and moves `isDevBuildRuntime` out of `native-badge/footer-badge.ts` for the gateway store reader floor; `mhc` lacks the `native-badge` component, the `omob` `OMO_PACKAGE_DIR` dev-build launcher payload, and gateway schema v11-v14 (`docs/omo-sync/v5.1.83/thread-gateway.md`) |
| `5b9d744f37`, `b812183af3` (no PR) | n/a | TS Bun tests wiring `createDefaultPostHogTransport` against `Bun.serve` to assert `flushAt === 20` isolation from the `flushAt === 1` legacy ping; `crates/omo/components/maho-omo-telemetry/src/omo_native_summary_wiring.rs` tests `NativeCaptureState::publish` via in-memory `CaptureFn` closures without constructing `PostHogTelemetryTransport` |
| `041a683066` (no PR) | n/a | TS Bun `beforeEach`/`afterEach` shared `TEST_DIR` cleanup fix in `claude-code-mcp-loader/loader.test.ts`; Rust tests in `crates/omo/claude-code-compat-core/src/features/claude_code_mcp_loader/loader.rs` already isolate every test with `tempfile::tempdir()` |
