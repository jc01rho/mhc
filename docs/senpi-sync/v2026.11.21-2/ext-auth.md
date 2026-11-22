# ext-auth track ledger — senpi v2026.11.21-2 (pin f01af13491)

Range `59d58cee5` (v2026.10.27-2) .. `f01af13491` (v2026.11.21-2). Verdicts: `ported` = already in mhc main (file/symbol or a round-1 PR), `port` = implemented in this slice, `deferred` = missing and NOT implemented here (needs a follow-up slice), `n/a` = no meaningful Rust counterpart, `golden` = fixture regeneration only.

## Implemented in this slice

| senpi PR | verdict | mhc location | test |
|---|---|---|---|
| 4060 | port | `maho-ext-anthropic-subscription/src/stream_events.rs` (`begin_attempt`), `src/stream.rs` (called per attempt) | `begin_attempt_resets_response_id_and_usage` |
| 4427 | port | `maho-ext-anthropic-subscription/src/oauth_login.rs` (`recovery_target`) | `lone_healthy_slot_is_not_a_recovery_target` |
| 4566 | port | `maho-ext-anthropic-subscription/src/prompt_append.rs` (`claude_skills_path`, `<skill_roots>` table) | `skill_roots_table_rewrites_only_when_target_is_not_a_divergent_real_file` |
| 4835 | port (partial) | `maho-ext-vertex-model-garden/src/{lib,transport}.rs` (`VERTEX_MODEL_GARDEN_REGISTRATION_BASE_URL`, baseUrl honored), new `maho-core/src/provider_base_url_notice.rs`, `maho-core/src/model_config.rs` | `only_fixed_address_providers_with_a_base_url_get_a_notice` |
| 5019 | port | `maho-ext-anthropic-subscription/src/{stream_guidance,failover}.rs` (model-limit sentence, retry-after marker, model-scoped pool refusal reaches the chain) | `model_scoped_pool_block_emits_model_limit_sentence_and_retry_after_marker` |
| 5029 | port | `maho-ext-api/src/types.rs` (`ProviderConfig.local_only`), `maho-core/src/{provider_composer,agent_session_runtime}.rs`, both provider `extension.rs` | covered by provider-composer tests |
| 5631 | port | `maho-ext-anthropic-subscription/src/{errors,failover,stream_guidance,account_events}.rs` (`SdkErrorKind::LoginUnreadable`, rotate without persisting) | `exhaustion_and_unknown_cases`, `sdk_error_kind_name` |
| 5947 | port | `maho-ext-cursor-cli-oauth/src/stream.rs` (`cursor_cli_diagnostic`, error payload) | `cursor_cli_diagnostic_classifies_auth_and_rate_limit_pool_refusals` |
| 6087 | port | `maho-ext-anthropic-subscription/src/prompt_append.rs` (`claude_skills_path` realpath identity) | same test as 4566 |
| 6270 | port | `maho-ext-vertex-model-garden/src/models.rs` (`thinking_levels`, `thinking_toggle`, per-model `default_thinking_level`) | `thinking_toggles_expose_off_and_medium_with_per_model_chat_template_flags` |
| 5195 | port | `maho-ext-prompt-preset/src/deepseek_v4_1_flash.rs` (DeepSeek V4.1 Flash core rewrite, superseded in-tree by #7109 standalone core) | `v41_flash_renders_each_rule_of_each_surface_once_under_its_own_section` |
| 5686 | port | `maho-core/src/dynamic_prompt/{types,build,handoff,mod}.rs` (`SharedSections`, `FullToolPrompt`, `FULL_HANDOFF_LANGUAGE_RULE`), new `maho-ext-prompt-preset/src/active_preset.rs`, `contract.rs` (`FULL_SHARED_SECTION_PRESETS`, `preset_shared_sections`), `presets.rs`, `index.rs`, `maho-ext-todotools/src/prompt.rs` (`FULL_TASK_MANAGEMENT_SECTION`) | `declares_full_shared_sections_for_exactly_the_opus_and_fable_presets` |
| 6264 | port | `maho-core/src/dynamic_prompt/build.rs` (`native_search_available` on `DynamicPromptCoreContext` and `BuildDynamicSystemPromptOptions`), `maho-ext-prompt-preset/src/{claude_haiku_5_5,claude_sonnet_5_5,index}.rs` | `adds_the_search_grounding_sentence_when_only_anthropic_native_search_is_available`, `omits_the_search_grounding_sentence_when_neither_builtin_nor_native_search_is_available`, `keeps_the_sonnet_prompt_unchanged_when_only_anthropic_native_search_is_available` |
| 6314 | port | new `maho-ext-prompt-preset/src/claude_fable_5_2.rs`, `contract.rs` (`ClaudeFable52`), `matchers.rs` (`is_claude_fable_52_model`), `presets.rs`, `settings.rs`, `lib.rs`, `tests/dispatch.rs` | `fable52_core_renders_codebase_ownership_and_ask_user_question` |
| 6962 / 6980 | port | `maho-ext-prompt-preset/src/{contract,gpt_6_2,gpt_6_astra,matchers,presets,settings}.rs`, `tests/gpt_6_2_preset.rs`, `maho-core/src/high_reasoning_warning.rs` (GPT-6.2 preset contract, standalone core, `gpt-6-astra` surface anchor `swap` guard, high-reasoning warning matchers) | `every_gpt_6_2_tier_renders_the_standalone_core`, `surface_variants_swap_anchors_for_terminal_app_and_chat`, `gpt_six_sol_warns_at_extended_levels`, `astra_warns_only_at_max` |
| 6976 | port | `maho-ext-goal/src/prompt.rs` (Astra-tier regex keying) | `refusal_is_final_in_both_variants` |
| 7032 | port | `maho-ext-anthropic-subscription/src/options.rs` (`CLAUDE_CODE_MAX_MCP_DESCRIPTION_LENGTH=16384` in `claude_code_query_environment`), `maho-codemode/src/prompt/eval-prompt-template.txt` (front-loaded `Eval-only tools` line), `maho-codemode/tests/attest_and_prelude.rs` | `lifts_claude_code_mcp_description_cap_and_respects_host_override`, `names_the_eval_only_tools_within_claude_code_mcp_description_cap` |
| 7109 / 7110 | port | new `maho-ext-prompt-preset/src/{deepseek_v4_1_flash_rules,gpt_6_2_rules}.rs`, `src/{deepseek_v4_1_flash,gpt_6_2,contract}.rs` (`PLAN_AFTER_READING_PRESETS`, `preset_first_turn_plan_default`, `TODO_RULE_OWNER_PRESETS`, `preset_owns_todo_rules`), `maho-core/src/settings_manager.rs` (`get_todo_first_turn_plan_with_fallback`), `maho-ext-todotools/src/index.rs`, `maho-ext-prompt-preset/tests/{deepseek_v4,gpt_6_2_preset}.rs` | `defaults_the_first_turn_plan_to_off_under_every_gpt_preset_and_deepseek_v4_1_flash`, `v41_flash_renders_each_rule_of_each_surface_once_under_its_own_section`, `the_core_renders_every_section_in_table_order` |

## Already ported in mhc main (round 1, PRs #24-#44)

| senpi PR | verdict | evidence |
|---|---|---|
| 508789a525 (direct, #6729) | ported | `stream_guidance.rs` pool-refusal retry pacing |
| dec021a22c (direct, #6360/#6284) | ported | prompt-preset Fable 5.2 ownership + engine turn pause |
| 877bf73ab8 (direct, #5472/#5595/#5508/#4066) | ported | long-context budget, retry fallback, seat digest, opaque signatures, install salt |
| bd7248b233 (direct, #5095/#5075/#4838/#5020/#4522) | ported | Claude CLI auth driver, credential accounts, legacy sign-out, auth block recovery |
| 6669 | ported | `neutralize_tool_result_text` in `content_blocks.rs`, `prompt_bridge.rs`, `session_sync.rs` (#38) |
| 5352 | ported | `current_turn_tool_images_*` are private consts in `prompt_bridge.rs` |

## Deferred (missing; follow-up slice)

| senpi PR | verdict | why deferred |
|---|---|---|
| 4437 | deferred | `claude-cli-auth` / `claude-cli-process` / `claude-cli-browser-capture` modules do not exist in mhc; CLI-store auth surface needs its own slice |
| 5006 | deferred | needs new `credential_account_status` module plus call sites in account/cursor/gpt account commands |
| 5019 | ported | see implemented table |
| 5032 | deferred | goal park on pool block: `maho-ai` diagnostic vocabulary, credential-pool failover, goal continuation |
| 5195 | port | see implemented table |
| 5276 | deferred | cold-seed `ReplayedHistoryImages` split (`history` + `currentTurn`) |
| 5353 | deferred | `login_expired` sign-in state across auth storage, accounts, auth block recovery |
| 5357 | deferred | depends on 4437 (claude-cli-auth) |
| 5616 | deferred | duplicate `accounts_changed` emission on account add |
| 5625 | deferred | `is_foreign_auth_block` across `credential_accounts`, accounts, affinity, failover |
| 5631 | ported | see implemented table |
| 5947 | ported | see implemented table |
| 6112 | deferred | `is_servable_account_slot` + unusable-dir sign-in state |
| 6116 | deferred | nameless-slot seat digest guard |
| 6264 | port | see implemented table |
| 6270 | ported | see implemented table |
| 6289 | deferred | account-list view module + `limit_summary` module |
| 6303 | deferred | `claude_code_max_context_tokens` window math in auth lane/query env |
| 6333 | deferred | `claude_settings_env` resolver + prompt-cache TTL precedence |
| 6380 / 6398 | deferred | Claude connection-test diagnostics bound to the tested credential |
| 6730 | deferred | limit recheck on person turns (agent-session input attribution) |
| 6962 / 6980 | port | see implemented table |
| 6976 | ported | see implemented table |
| 7032 | port | see implemented table |
| 7109 / 7110 | port | see implemented table |

## n/a

| senpi PR | reason |
|---|---|
| 57aa8ee833 (direct, #6359) | TS-only prompt-ownership guide + fused-equivalence QA goldens |
| 72526b0cf0 (direct, #5415/#5573/#5559/#5436) | omo QA harness, CI yaml, knip, AGENTS/CONTRIBUTING — tooling mhc does not ship |
| 4657 | Windows-only teardown fix in a module mhc has not ported |
| 4705 | browser-sink dispose fix in the same unported module |
| 5686 | ported in `preset-followup` slice — see implemented table (`port`) |
| 6279 | `windowsHide` on `std::process::Command`; mhc spawn wrappers do not expose the flag |
| 6295 | abort/writable race; `session_registry_pump` has no `writable()` queue hook |
| 6396 | revert of 6380; 6380's later state is the one to port |

Totals: port 18, ported 6, deferred 12, n/a 7 = 43 rows (49 tsv rows, 5 direct-merge rows counted once each, 1 overlap between 5019/5631 counted in the implemented table; #5195, #5686, #6264, #6962/#6980, #7032, #7109/#7110 ported in `preset-followup`).

New deps / Cargo.lock / ci-shard / workflow edits: none.
Conflict risk with other tracks: `maho-ext-prompt-preset/src/{contract,matchers,presets,settings}.rs` (also `ai` track), `maho-core/src/dynamic_prompt/*` (also `routing` track), `maho-core/src/model_config.rs`.
