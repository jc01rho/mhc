# ai track ledger — senpi v2026.11.21-2

Round 2 of the mhc senpi port: every PR merged between `59d58cee5` (v2026.10.27-2) and
`f01af134919f0bbf24b2887000e2142dfd290567` (v2026.11.21-2) that touches `packages/ai`, split into
`/tmp/omo-mig2/tracks/ai.tsv` (83 rows: 43 catalog-only refreshes + 40 code PRs, including
`senpi#6422`).

| senpi PR | verdict | mhc location or reason |
|---|---|---|
| chore(ai): regenerate the model catalog for v2026.11.21 (`48f9564ac4`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.20 (`44eb4022d8`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.19-5 (`e86bfd141b`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.19-4 (`5f3bcb0a5d`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.19-3 (`2f2536d9fd`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.19-2 (`221396e543`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.19 (`27c379a24c`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.18-3 (`c6f1052d5b`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.18-2 (`378198bec0`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.18 (`83adaff917`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| Merge #6948: GPT-6.2 Astra and Sol catalog rows (thanks @dev (`7ecb2ca0a3`) | ported | `crates/maho-ai/src/models.rs` (`XHIGH_MODEL_IDS`, `OPENAI_MAX_MODEL_IDS`, no-`none` list), `src/api/openai_responses_shared.rs` (`ULTRAFAST_DOCUMENTED_MODEL_IDS`), `tests/gpt_6_2_catalog.rs` (round-1 #28 + regenerated catalog) |
| chore(ai): regenerate the model catalog for v2026.11.17-2 (`ba032498c7`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.17 (`333b9ebcd1`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.15-4 (`6a4cbb3f95`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.15-3 (`8f23bb14dd`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.15-2 (`8f63a25ad4`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.15 (`5799c801b2`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.13 (`145d913604`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.12 (`dc5cd0b833`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.11 (`dd75137ead`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.10-3 (`0139f38914`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.10-2 (`24b02dfab7`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.10-1 (`19e67ef4a7`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.10 (`a5ba567b1c`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| Merge remote-tracking branch 'origin/main' (`54e1935746`) | port | `crates/maho-ai/src/utils/validation.rs` `decode_json_encoded_structures` + 3 unit tests |
| chore(ai): regenerate the model catalog for v2026.11.9-2 (`a83678f4f5`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.9 (`c307f14533`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.6-3 (`be97a99e7b`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.6-2 (`52f274b486`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.6-1 (`ad0cdee102`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.5-1 (`d302c69f30`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.4-2 (`3401bbbaab`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.4-1 (`fbdd282ba8`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.3-3 (`2f02f13ba5`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.3-2 (`b3332c000e`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.3-1 (`4aecb8a99f`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.2-3 (`3e9ae7d60b`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.2-2 (`4dfeefdcfa`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.11.02-1 (`309dc8626d`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.10.31-2 (`eb09c02563`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.10.31-1 (`633d84a355`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.10.30-2 (`070b7b742b`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| chore(ai): regenerate the model catalog for v2026.10.30-1 (`bffe103c3d`) | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| #1248 | ported | `crates/maho-ai/src/utils/prompt_cache_ttl.rs` (`allows_anthropic_long_cache_retention`, `supports_anthropic_cache_warming`), `src/api/anthropic_messages.rs`, `src/api/warm_prompt_cache.rs` (round-1 #28) |
| #4341 | port | `crates/maho-ai/src/utils/provider_failure_description.rs` (`PROVIDER_NOT_CONFIGURED_PREFIX` guidance, third-party billing text without `/model`) + unit test |
| #4585 | port | `crates/maho-ai/src/legacy_provider_ids.rs` `upstream_provider_id_for` + unit test |
| #4952 | port | `crates/maho-ai/src/utils/provider_diagnostic_vocabulary.rs` (`OPENAI_COMPATIBLE_PRECISE_TYPES`, `precise_provider_token`), `src/types.rs` (`sanitize_provider_diagnostic`) + unit test |
| #5008 | port | `crates/maho-ai/src/auth/oauth/openai_chatgpt.rs` (`start_callback_server` `AddrInUse` fallback to ephemeral port and dynamic `redirect_uri`) |
| #5010 | port | `crates/maho-ai/src/utils/oauth_refresh_error.rs` (`OAuthTokenEndpointError::with_facts` carrying `oauth_error` and `credential_refused`), `src/auth/oauth/kimi_coding.rs` + unit test |
| #5012 | port | `crates/maho-ai/src/auth/oauth/openai_chatgpt.rs` (`id_token` validation, `DIRECT_TOKEN_SCOPE` & `clientId`/`scopes` metadata on `OAuthCredential`) |
| #5014 | port | `crates/maho-ai/src/cli.rs` (`get_or_create_device_id`, `get_or_create_device_id_in`), `src/auth/types.rs` (`with_app_name`, `app_name`), `src/auth/oauth/openai_chatgpt.rs` (`get_wire_identity` fallback) + unit test |
| #5022 | port | `crates/maho-ai/src/auth/pool/classify.rs` deleted; `src/auth/pool/failover.rs` reduced to `DEFAULT_SLOT_BLOCK_MS`, `MAX_SLOT_BLOCK_MS`, `TURN_RETRY_SUPPRESSION_PREFIX` |
| #5023 | n/a | TS-only `generate-models.ts` test-harness fixture refactor (`copyJsonModelGenerationFixture`); mhc embeds the generated JSON and keeps no TS generator tests |
| #5024 | ported | `crates/maho-ai/src/auth/oauth_refresh.rs` (`refresh_oauth_credential`, `exchange_and_store`, per-slot CAS outside store lock) |
| #5025 | ported | `crates/maho-ai/src/auth/credential_store.rs` (`modify_or_delete`), `src/auth/oauth_refresh.rs`, `src/auth/pool/slots.rs`, `src/auth/resolve.rs` |
| #5042 | port | `crates/maho-ai/src/utils/prompt_cache_ttl.rs` (`mixed_strict_tools`), `src/types.rs`, `src/api/openai_completions.rs` `convert_tools` + unit test |
| #5046 | port | `crates/maho-ai/src/utils/oauth_url.rs` (`redact_oauth_url`) + unit test |
| #5047 | ported | `crates/maho-ai/src/utils/retry_hint.rs` (`clock_reset_ms`, `CLOCK_RE`, `MONTHS`) (round-1 #28) |
| #5077 | port | `crates/maho-ai/src/utils/provider_diagnostic_carrier.rs` (`SIWC_ERRORS`, `siwc_error_for_code`, `siwc_recovery_from_diagnostic`), `src/utils/siwc_errors.rs` + unit test |
| #5078 | ported | `crates/maho-ai/src/auth/oauth/openai_chatgpt_model_catalog.rs` (`OpenAIChatGptModelCatalog`, `OpenAIChatGptCatalogModel`, `OpenAIChatGptModelCatalogError`) |
| #5081 | port | `crates/maho-ai/src/openai_responses_compat.rs` (`siwc`), `src/api/openai_responses.rs` (`ResponsesCompat.siwc`), `src/auth/pool/slots.rs` |
| #5135 | port | `crates/maho-ai/src/providers/google_vertex.rs` (`stored_env` empty-string fallback to ambient env) + unit test |
| #5136 | port | `crates/maho-ai/src/api/anthropic_messages.rs` (`CLAUDE_OAUTH_TOKEN_MARKER`, `is_claude_oauth_token_refused_message`, `assert_not_claude_oauth_token` in `drive` and `stream_simple`) |
| #5147 | port | `crates/maho-ai/src/utils/validation.rs` (`find_discriminated_union`, `select_branch`, `format_discriminated_union_error`) + unit test |
| #5310 | n/a | TS-only test-fixture hardening (credential-shaped literals built from parts for secret scanners); no Rust counterpart |
| #5316 | port | `crates/maho-ai/src/utils/tool_name_match.rs` (`DESTRUCTIVE_PREFIX_WORDS`, `stray_prefix_match`) + 6 table test cases |
| #5501 | port | `crates/maho-ai/src/api/openai_completions.rs` (`with_provider_thought_signature`, streaming `provider_specific_fields.thought_signature` capture) + unit test |
| #5586 | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| #5589 | port | `crates/maho-ai/src/api/anthropic_thinking_binding.rs` (`ThinkingBindingRecovery`, `bind_replayed_thinking`, `is_thinking_binding_mismatch_error`, `is_thinking_binding_control_refused_error`), `src/api/anthropic_request_shape_log.rs` (`log_anthropic_recovery`), `src/api/anthropic_messages.rs` + unit tests |
| #5821 | port | `crates/maho-ai/src/api/openai_images_params.rs` (`OPENAI_IMAGE_COMPLEXITIES`, `complexity`), `src/utils/provider_error.rs` (`/v1/images` allowlist pattern), `src/image_models.rs` (`gpt-image-2.6-sunburst`/`flare` tests) |
| #5944 | port | `crates/maho-ai/src/api/openai_responses_prompt_cache.rs` (`supports_openai_responses_explicit_prompt_cache_mode` checking `!siwc`) + `src/api/openai_responses.rs` + unit test |
| #5978 | ported | `crates/maho-ai/src/utils/retry_hint.rs` zone rule and unit tests (round-1 #28) |
| #6156 | port | `crates/maho-ai/src/api/openai_codex_responses.rs` (`resolve_codex_service_tier` `default`/`auto`/absent echo fallback, post-`onPayload` `body.service_tier`) + unit test |
| #6209 | n/a | Fireworks catalog data plus TS `generate-models` tests; the data side lands in the regenerated catalog |
| #6292 | ported | `crates/maho-ai/src/utils/openai_prompt_cache_lane.rs` (`OpenAIPromptCacheLane`, `classify_openai_prompt_cache_lane`), `src/utils/prompt_cache_ttl.rs` (round-1 #28) |
| #6309 | port | `crates/maho-ai/src/cursor/capability_table.rs` (`claude-fable-5-2` row), `src/cursor/model_capabilities.rs` + unit test |
| #6651 | port | `crates/maho-ai/src/api/openai_responses_shared.rs` (skip `configuration_update` on `siwc`), `src/api/openai_responses.rs` (`configured_effort` from latest `ConfigurationUpdate`) |
| #6653 | port | `crates/maho-ai/src/utils/siwc_errors.rs` (`SIWC_MISSING_RESPONSES_SCOPE_GUIDANCE`, `is_siwc_missing_responses_scope`), `src/api/openai_responses.rs` `format_openai_responses_error` + unit test |
| #6663 | port | `crates/maho-ai/src/utils/siwc_errors.rs` (`MISSING_RESPONSES_SCOPE` multi-scope regex running to end of line) + unit test |
| #6666 | golden | regenerated catalog data (`crates/maho-ai/data/{models,image-models}.json`, `src/providers/data/{meta,radius,model_data_manifest}.json`, `CLASSIFIER_MODELS_JSON`) |
| #6718 | port | `crates/maho-ai/src/api/subscription_sharing_items.rs` (`is_shared_subscription_item_limit_message`, `shared_subscription_item_limit_message`) wired into `openai_codex_responses.rs` and `openai_responses.rs` + unit test |
| #7122 | ported | `crates/maho-ai/data/models.json` regenerated Amazon Bedrock `anthropic.claude-haiku-5-5` vs `global.anthropic.claude-haiku-5-5` route costs |
| #6422 | n/a | Node/Bun realm registry bridging several `pi-ai` JS copies in one process (`@earendil-works/pi-realm-registry`); mhc links one Rust `maho-ai` crate, so `crates/maho-ai/src/node/provider_scope.rs` already holds the single-process invariant |
