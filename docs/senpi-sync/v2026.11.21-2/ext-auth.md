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
| 6314 | port | new `maho-ext-prompt-preset/src/claude_fable_5_2.rs`, `contract.rs` (`ClaudeFable52`), `matchers.rs` (`is_claude_fable_52_model`), `presets.rs`, `settings.rs`, `lib.rs`, `tests/dispatch.rs` | `fable52_core_renders_codebase_ownership_and_ask_user_question` |
| 6976 | port | `maho-ext-goal/src/prompt.rs` (Astra-tier regex keying) | `refusal_is_final_in_both_variants` |

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
| 5195 | deferred | DeepSeek V4.1 Flash core rewrite (202-line preset) |
| 5276 | deferred | cold-seed `ReplayedHistoryImages` split (`history` + `currentTurn`) |
| 5353 | deferred | `login_expired` sign-in state across auth storage, accounts, auth block recovery |
| 5357 | deferred | depends on 4437 (claude-cli-auth) |
| 5616 | deferred | duplicate `accounts_changed` emission on account add |
| 5625 | deferred | `is_foreign_auth_block` across `credential_accounts`, accounts, affinity, failover |
| 5631 | ported | see implemented table |
| 5947 | ported | see implemented table |
| 6112 | deferred | `is_servable_account_slot` + unusable-dir sign-in state |
| 6116 | deferred | nameless-slot seat digest guard |
| 6264 | deferred | needs `native_search_available` on `DynamicPromptCoreContext` (dynamic-prompt plumbing) |
| 6270 | ported | see implemented table |
| 6289 | deferred | account-list view module + `limit_summary` module |
| 6303 | deferred | `claude_code_max_context_tokens` window math in auth lane/query env |
| 6333 | deferred | `claude_settings_env` resolver + prompt-cache TTL precedence |
| 6380 / 6398 | deferred | Claude connection-test diagnostics bound to the tested credential |
| 6730 | deferred | limit recheck on person turns (agent-session input attribution) |
| 6962 / 6980 | deferred | GPT-6.2 preset contract/`gpt-6.2` core rewrite |
| 6976 | ported | see implemented table |
| 7032 | deferred | MCP description-length cap (env + codemode template) |
| 7109 / 7110 | deferred | deepseek-v4-1-flash standalone core and gpt-6.2 core v2 |

## n/a

| senpi PR | reason |
|---|---|
| 57aa8ee833 (direct, #6359) | TS-only prompt-ownership guide + fused-equivalence QA goldens |
| 72526b0cf0 (direct, #5415/#5573/#5559/#5436) | omo QA harness, CI yaml, knip, AGENTS/CONTRIBUTING — tooling mhc does not ship |
| 4657 | Windows-only teardown fix in a module mhc has not ported |
| 4705 | browser-sink dispose fix in the same unported module |
| 5686 | full shared-sections (`sharedSections`) revert needs a dynamic-prompt rewrite; deferred, not n/a — listed here only because it blocks 6264/6314's shared-sections variant |
| 6279 | `windowsHide` on `std::process::Command`; mhc spawn wrappers do not expose the flag |
| 6295 | abort/writable race; `session_registry_pump` has no `writable()` queue hook |
| 6396 | revert of 6380; 6380's later state is the one to port |

Totals: port 12, ported 6, deferred 17, n/a 8 = 43 rows (49 tsv rows, 5 direct-merge rows counted once each, 1 overlap between 5019/5631 counted in the implemented table).

New deps / Cargo.lock / ci-shard / workflow edits: none.
Conflict risk with other tracks: `maho-ext-prompt-preset/src/{contract,matchers,presets,settings}.rs` (also `ai` track), `maho-core/src/dynamic_prompt/*` (also `routing` track), `maho-core/src/model_config.rs`.
