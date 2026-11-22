# ext-auth track ledger — senpi v2026.11.21-2 (pin f01af13491)

Range `59d58cee5` (v2026.10.27-2) .. `f01af13491` (v2026.11.21-2). Verdicts: `ported` = already in mhc main (file/symbol or a round-1 PR), `port` = implemented in this slice, `deferred` = missing and NOT implemented here (needs a follow-up slice), `n/a` = no meaningful Rust counterpart, `gap` = a real upstream capability mhc lacks because the subsystem it lives in is not ported (the reason names the subsystem), `golden` = fixture regeneration only.

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
| 4437 / 5357 | port | new `maho-ext-anthropic-subscription/src/{claude_cli_auth,claude_cli_auth_status,claude_cli_browser_capture,claude_cli_environment,claude_cli_process}.rs` (`claude auth login --claudeai` driver with the copy-code URL allowlist, paste normalization and typed failure reasons; `auth status` and `auth logout`; sanitized spawn environment; `BROWSER` capture helper). A cancelled or timed-out child is killed outright: the workspace forbids `unsafe`, so there is no process-group SIGTERM then SIGKILL | `a_pasted_code_is_reduced_to_the_line_the_cli_reads`, `only_claudes_own_sign_in_hosts_are_passed_on`, `the_paste_fallback_writes_the_code_only_to_stdin`, `cancelling_kills_the_child` (fake `claude` shell-script fixture, unix) |
| 4657 / 4705 | port | `maho-ext-anthropic-subscription/src/claude_cli_browser_capture.rs` (`helper_script` leaves the account dir before it writes on Windows; `dispose` revokes the sink path by rename before deleting) | `the_windows_helper_leaves_the_account_dir_before_it_writes`, `disposing_removes_the_helper_and_its_sink` |
| 5006 | port | new `maho-core/src/credential_account_status.rs`, `maho-ext-api/src/types.rs` (`CredentialAccountSummary` `blocked_reason` / `blocked_until`), `maho-ext-account`, `maho-ext-cursor-cli-oauth/src/account_command.rs`, `maho-ext-builtin-loose/src/gpt_account.rs`, `maho-ext-anthropic-subscription/src/{account_command,account_management}.rs` (account lists say why an account is blocked and until when) | `a_blocked_account_says_why_it_sits_out_and_until_when`, `an_account_row_carries_label_source_health_model_blocks_and_pin` |
| 5032 | port | `maho-core/src/credential_pool/{failover,rotation_stream}.rs` (`limited_until`, `with_pool_retry_at`, `pool_limited`), `maho-ai/src/utils/provider_diagnostic_vocabulary.rs` (`credential_pool_blocked_diagnostic`), `maho-ext-anthropic-subscription/src/{affinity,provider_diagnostic,stream,stream_guidance}.rs`, `maho-ext-goal/src/{monitor_continuation,runtime}.rs` (a goal parks while every slot is rate limited; `provider_pause_notice`) | `a_pool_waiting_only_on_rate_limits_reports_the_soonest_reset`, `a_turn_the_provider_refused_never_counts_toward_the_toolless_streak`, `provider_failure_stages_runtime_recovery_only_without_core_retry` |
| 5276 | port | `maho-ext-anthropic-subscription/src/{cold_seed_images,prompt_bridge}.rs` (`ReplayedHistoryImages { history, current_turn }`) | `a_turn_that_ends_after_tool_screenshots_rewrites_none_of_the_history_the_last_request_cached`, `a_user_upload_keeps_its_history_label_whether_or_not_the_turn_has_ended` |
| 5616 | port | no code change: the native `/claude-account add` runs the OAuth flow inside the extension and emits `accounts_changed` once itself; it never goes through a core login that already emitted, which is the duplicate senpi removed. The one-event invariant is pinned through the real command handler | `one_account_add_emits_accounts_changed_exactly_once` (`maho-ext-anthropic-subscription/tests/account_add_event.rs`) |
| 6116 | port | `maho-ext-anthropic-subscription/src/accounts.rs` (`is_listed_stored_slot`, `unusable_stored_slot_count`), `src/auth_lane.rs` (`claude_account_unusable`) | `a_pool_with_only_nameless_stored_accounts_refuses_instead_of_falling_back_to_ambient`, `a_sentinel_slot_is_hidden_on_purpose_not_counted_as_unusable` |
| 6289 | port | new `maho-core/src/account_list_view.rs`, `maho-core/src/subscription_limits/limit_summary.rs`, `maho-ext-anthropic-subscription/src/account_list.rs` (`/claude-account` lists one readable block per account) | `each_account_is_a_readable_block_with_status_limits_pin_and_next_turn_tags`, `a_window_that_reset_since_the_reading_and_a_failed_check_read_plainly` |
| 6303 | port | `maho-ext-anthropic-subscription/src/{context_window,options}.rs` (`claude_code_max_context_tokens`, used by `claude_code_query_environment`) | `claude_code_refuses_a_request_only_past_the_window_that_is_planned`, `a_request_budget_below_the_cap_reserves_only_what_it_asks_for` |
| 6333 | port | new `maho-ext-anthropic-subscription/src/claude_settings_env.rs`, `src/{prompt_cache_ttl,stream}.rs` (a loaded settings file's cache lifetime wins the one-shot pin; the files are merged natively instead of through the SDK resolver) | `a_later_source_overrides_an_earlier_one_key_by_key`, `a_lifetime_set_in_a_loaded_settings_file_is_visible_to_the_pin` |

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
| 4437 | port | see implemented table |
| 5006 | port | see implemented table |
| 5019 | ported | see implemented table |
| 5032 | port | see implemented table |
| 5195 | port | see implemented table |
| 5276 | port | see implemented table |
| 5353 | gap | `login_expired` is a state of a `store: claude-cli` account slot (`signInRequired`, `authVerdict`, the `claude auth status` probe in `auth-block-recovery`, `sign-in-again`). mhc has no CLI-store slot: `AccountSlot` has no `store` / `dir_id` / `sign_in_required`, there is no `sign_in_again` module and `auth_block_recovery.rs` has no CLI probe, so nothing could carry or clear the state. Needs the Claude CLI-store account subsystem (account dirs, login slots, token source, turn-lane wiring) |
| 5357 | port | see implemented table |
| 5616 | port | see implemented table |
| 5625 | gap | `isForeignAuthBlock` and `authVerdict` apply only to `store: claude-cli` account slots (`credential-accounts` `storedSlotBlock`, `listAccounts`, `blockedAccount`, `persistBlock`). mhc `AccountSlot` has no `store` / `dir_id` / `auth_verdict` and nothing writes a CLI verdict. Same missing CLI-store account subsystem as #5353 |
| 5631 | ported | see implemented table |
| 5947 | ported | see implemented table |
| 6112 | gap | `isServableAccountSlot` and the unusable-dir sign-in state key on `store: claude-cli` and `dirId` (`isValidAccountDirId` from `claude-cli-account-dir`), none of which exist in mhc. Same missing CLI-store account subsystem as #5353 |
| 6116 | port | see implemented table |
| 6264 | port | see implemented table |
| 6270 | ported | see implemented table |
| 6289 | port | see implemented table |
| 6303 | port | see implemented table |
| 6333 | port | see implemented table |
| 6380 / 6398 | gap | the connection-test diagnostics (`message` / `status` on `ClaudeCliAccountCheck`, the dirId-bound `withCheck`, `runClaudeCliConnectionTest`) live in `claude-cli-account-test`, `claude-cli-connection-test` and `claude-cli-login-slots`, which mhc does not have; the sign-in driver ported with #4437 has no login slot to record a check on. Needs the CLI-store account subsystem. #6396 is the revert between them |
| 6730 | gap | the person-turn identity needs agent-session admission plumbing mhc lacks: `TurnOriginTracker` is not driven by input admission (`note_user_message_start` has no caller outside a test) and `retry_fallback` has no `release_account_hold` / `release_guessed_account_hold`, which #6730 defers to after admission. The extension half (`limit-recheck`, the half-open `select` in failover) is inert without that identity. Needs the agent-session input-attribution and account-hold release path |
| 6962 / 6980 | port | see implemented table |
| 6976 | ported | see implemented table |
| 7032 | port | see implemented table |
| 7109 / 7110 | port | see implemented table |

## n/a

| senpi PR | reason |
|---|---|
| 57aa8ee833 (direct, #6359) | TS-only prompt-ownership guide + fused-equivalence QA goldens |
| 72526b0cf0 (direct, #5415/#5573/#5559/#5436) | omo QA harness, CI yaml, knip, AGENTS/CONTRIBUTING — tooling mhc does not ship |
| 4657 | ported in `auth-followup` slice — see implemented table (`port`) |
| 4705 | ported in `auth-followup` slice — see implemented table (`port`) |
| 5686 | ported in `preset-followup` slice — see implemented table (`port`) |
| 6279 | `windowsHide` on `std::process::Command`; mhc spawn wrappers do not expose the flag |
| 6295 | abort/writable race; `session_registry_pump` has no `writable()` queue hook |
| 6396 | revert of 6380; 6380's later state is the one to port |

Totals: port 28, ported 6, gap 5, n/a 5, deferred 0 = 44 rows (recounted from the tables: the earlier line's deferred tally of 12 undercounted the 15 deferred rows it listed; 49 tsv rows, 5 direct-merge rows counted once each, 1 overlap between 5019/5631 counted in the implemented table; #5195, #5686, #6264, #6962/#6980, #7032, #7109/#7110 ported in `preset-followup`; #4437/#5357, #4657/#4705, #5006, #5032, #5276, #5616, #6116, #6289, #6303, #6333 ported in `auth-followup`, #5353, #5625, #6112, #6380/#6398, #6730 left `gap`).

New deps / Cargo.lock / ci-shard / workflow edits: none (`maho-core` moved from dev-dependencies to dependencies in `maho-ext-account`; it was already in Cargo.lock).
Conflict risk with other tracks: `maho-ext-prompt-preset/src/{contract,matchers,presets,settings}.rs` (also `ai` track), `maho-core/src/dynamic_prompt/*` (also `routing` track), `maho-core/src/model_config.rs`.
