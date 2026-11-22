# ai track ledger — senpi v2026.11.21-2

Round 2 of the mhc senpi port: every PR merged between `59d58cee5` (v2026.10.27-2) and
`f01af134919f0bbf24b2887000e2142dfd290567` (v2026.11.21-2) that touches `packages/ai`, split into
`/tmp/omo-mig2/tracks/ai.tsv` (83 rows: 43 catalog-only refreshes + 40 code PRs, including the
lead's late addition `senpi#6422`).

Verdicts: `ported` = already in mhc (with the location), `port` = implemented in this PR,
`golden` = regenerated catalog data, `n/a` = no meaningful Rust counterpart.

## Catalog (golden)

`crates/maho-ai/data/models.json` (42 providers, 1991 models), `data/image-models.json` (2 providers,
80 models), `src/providers/data/{meta,radius,model_data_manifest}.json` and the embedded
`CLASSIFIER_MODELS_JSON` (5 providers, 75 models) are regenerated from senpi `f01af13491` with
`tools/golden/gen-models.mjs` semantics: `MODELS` minus the shard-embedded `meta`, the fork-owned
`radius` and the classifier-only `typesafe`, key order preserved.

| senpi PR | verdict | mhc location or reason |
|---|---|---|
| chore(ai): regenerate the model catalog for v2026.11.21 (`48f9564ac4`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.20 (`44eb4022d8`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.19-5 (`e86bfd141b`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.19-4 (`5f3bcb0a5d`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.19-3 (`2f2536d9fd`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.19-2 (`221396e543`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.19 (`27c379a24c`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.18-3 (`c6f1052d5b`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.18-2 (`378198bec0`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.18 (`83adaff917`) | golden | regenerated catalog data |
| Merge #6948: GPT-6.2 Astra and Sol catalog rows (thanks @dev (`7ecb2ca0a3`) | ported | `crates/maho-ai/src/models.rs` (`XHIGH_MODEL_IDS`, `OPENAI_MAX_MODEL_IDS`, no-`none` list), `src/api/openai_responses_shared.rs` (`ULTRAFAST_DOCUMENTED_MODEL_IDS`), `tests/gpt_6_2_catalog.rs` — round-1 #28 + the regenerated catalog |
| chore(ai): regenerate the model catalog for v2026.11.17-2 (`ba032498c7`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.17 (`333b9ebcd1`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.15-4 (`6a4cbb3f95`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.15-3 (`8f23bb14dd`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.15-2 (`8f63a25ad4`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.15 (`5799c801b2`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.13 (`145d913604`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.12 (`dc5cd0b833`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.11 (`dd75137ead`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.10-3 (`0139f38914`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.10-2 (`24b02dfab7`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.10-1 (`19e67ef4a7`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.10 (`a5ba567b1c`) | golden | regenerated catalog data |
| Merge remote-tracking branch 'origin/main' (`54e1935746`) | ported | `crates/maho-ai/src/utils/validation.rs` `decode_json_encoded_structures` + 3 unit tests |
| chore(ai): regenerate the model catalog for v2026.11.9-2 (`a83678f4f5`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.9 (`c307f14533`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.6-3 (`be97a99e7b`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.6-2 (`52f274b486`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.6-1 (`ad0cdee102`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.5-1 (`d302c69f30`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.4-2 (`3401bbbaab`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.4-1 (`fbdd282ba8`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.3-3 (`2f02f13ba5`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.3-2 (`b3332c000e`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.3-1 (`4aecb8a99f`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.2-3 (`3e9ae7d60b`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.2-2 (`4dfeefdcfa`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.11.02-1 (`309dc8626d`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.10.31-2 (`eb09c02563`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.10.31-1 (`633d84a355`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.10.30-2 (`070b7b742b`) | golden | regenerated catalog data |
| chore(ai): regenerate the model catalog for v2026.10.30-1 (`bffe103c3d`) | golden | regenerated catalog data |
| #1248 | ported | `crates/maho-ai/src/utils/prompt_cache_ttl.rs` (`allows_anthropic_long_cache_retention`, `supports_anthropic_cache_warming`), `src/api/anthropic_messages.rs`, `src/api/warm_prompt_cache.rs` — round-1 #28 |
| #4341 | ported | `crates/maho-ai/src/utils/provider_failure_description.rs` (`PROVIDER_NOT_CONFIGURED_PREFIX` guidance, billing text without the `/model` hint) + unit test |
| #4585 | ported | `crates/maho-ai/src/legacy_provider_ids.rs` `upstream_provider_id_for` + unit test |
| #4952 | ported | `crates/maho-ai/src/utils/provider_diagnostic_vocabulary.rs` (`OPENAI_COMPATIBLE_PRECISE_TYPES`, `precise_provider_token`), `src/types.rs` (`sanitize_provider_diagnostic` accepts the token) + unit test |
| #5008 | port | **pending** — ChatGPT OAuth callback: preferred port with EADDRINUSE fallback to an ephemeral port, dynamic `redirect_uri` |
| #5010 | port | **pending** — `OAuthTokenEndpointError` gains `oauthError`/`credentialRefused`; `kimi-coding`/`radius` adapters pass them |
| #5012 | port | **pending** — ChatGPT OAuth ID-token verification (`openai-id-token.ts`), `planUsageEnabled`, consent client id |
| #5014 | port | **pending** — ChatGPT OAuth host/installation id (`getOrCreateDeviceId`) and `appName` |
| #5022 | ported | `crates/maho-ai/src/auth/pool/classify.rs` removed; `src/auth/pool/failover.rs` keeps `DEFAULT_SLOT_BLOCK_MS`/`MAX_SLOT_BLOCK_MS` for `maho-core`'s `credential_pool` |
| #5023 | n/a | TS-only `generate-models.ts` test-harness fixture refactor (`copyJsonModelGenerationFixture`); mhc embeds the generated JSON and keeps no TS generator tests |
| #5024 | port | **pending** — Cross-process OAuth refresh serialization (`serializeExchange`, `refreshSlotName`) |
| #5025 | port | **pending** — OAuth revocation (`revokeStoredOAuth`, `clearTokens`, refresh observers) |
| #5042 | ported | `crates/maho-ai/src/utils/prompt_cache_ttl.rs` (`mixed_strict_tools`), `src/types.rs`, `src/api/openai_completions.rs` `convert_tools` + unit test |
| #5046 | port | **pending** — ChatGPT re-authorization (client-id pinning, `redactOAuthUrl`) |
| #5047 | ported | `crates/maho-ai/src/utils/retry_hint.rs` (`clock_reset_ms`, `CLOCK_RE`, `MONTHS`) — round-1 #28 |
| #5077 | port | **pending** — SIWC error stacking (`siwc-errors.ts`, provider diagnostic carrier/vocabulary/retry) |
| #5078 | ported | `crates/maho-ai/src/auth/oauth/openai_chatgpt_model_catalog.rs` (`[OI]ChatGptModelCatalog` etc.) |
| #5081 | port | **pending** — SIWC surfaces (slot routing, `shapeSiwcRequest`, OAuth account fields, chatgpt-subscription provider) |
| #5135 | ported | `crates/maho-ai/src/providers/google_vertex.rs` — an empty stored env value now falls back to the ambient env + unit test |
| #5136 | port | **pending** — Remove Anthropic OAuth impersonation: `ClaudeOAuthTokenRefusedError`, drop the `claude-cli` identity, CC tool-name mapping and the version-retry loop |
| #5147 | ported | `crates/maho-ai/src/utils/validation.rs` `find_discriminated_union`/`select_branch`/`format_discriminated_union_error` + unit test |
| #5310 | n/a | TS-only test-fixture hardening (credential-shaped literals built from parts for secret scanners); no Rust counterpart |
| #5316 | ported | `crates/maho-ai/src/utils/tool_name_match.rs` `stray_prefix_match` + `DESTRUCTIVE_PREFIX_WORDS` + 6 table rows |
| #5501 | ported | `crates/maho-ai/src/api/openai_completions.rs` `with_provider_thought_signature`, streaming `provider_specific_fields.thought_signature` capture + unit test |
| #5586 | golden | regenerated catalog data |
| #5589 | port | **pending** — Anthropic thinking-block binding recovery (`anthropic-thinking-binding.ts` + request integration) |
| #5821 | ported | `crates/maho-ai/src/api/openai_images_params.rs` (`complexity`), `src/utils/provider_error.rs` (`/v1/images` allowlist prose), `src/image_models.rs` (2.6 rows) + tests |
| #5944 | ported | `crates/maho-ai/src/api/openai_responses_prompt_cache.rs` `supports_openai_responses_explicit_prompt_cache_mode` + `src/api/openai_responses.rs` + unit test |
| #5978 | ported | `crates/maho-ai/src/utils/retry_hint.rs` zone rule and tests — round-1 #28 |
| #6156 | ported | `crates/maho-ai/src/api/openai_codex_responses.rs` `resolve_codex_service_tier` (`default`/`auto`/absent echo → sent tier), priced against the body's `service_tier` + unit test |
| #6209 | n/a | Fireworks catalog data plus TS `generate-models` tests; the data side lands in the regenerated catalog |
| #6292 | ported | `crates/maho-ai/src/utils/openai_prompt_cache_lane.rs` (`[OI]PromptCacheLane`, `classify_openai_prompt_cache_lane`), `src/utils/prompt_cache_ttl.rs` — round-1 #28 |
| #6309 | ported | `crates/maho-ai/src/cursor/capability_table.rs` (`` row), `src/cursor/model_capabilities.rs` + unit test |
| #6651 | port | **pending** — SIWC input items: configured effort vs requested reasoning effort |
| #6653 | port | **pending** — SIWC missing Responses scope guidance |
| #6663 | port | **pending** — SIWC multi-scope list |
| #6666 | golden | regenerated catalog data |
| #6718 | ported | `crates/maho-ai/src/api/subscription_sharing_items.rs` (new) + call sites in `openai_codex_responses.rs`/`openai_responses.rs` + unit test |
| #7122 | ported | `packages/ai/test` only (Bedrock Haiku 5.5 route prices); the data side lands in the regenerated `data/models.json` |
| #6422 | n/a | Node/Bun realm registry bridging several `pi-ai` JS copies in one process (`@earendil-works/pi-realm-registry`); mhc links one Rust `maho-ai` crate, so `crates/maho-ai/src/node/provider_scope.rs` already holds the single-process invariant |

## Divergences

- `crates/maho-ai/src/auth/pool/failover.rs` keeps `DEFAULT_SLOT_BLOCK_MS`/`MAX_SLOT_BLOCK_MS` and the
  turn-retry prefix: senpi deleted the whole engine, but `crates/maho-core/src/credential_pool/` reads those
  two constants, so only the unused classification/failover machinery was removed.
- `senpi#6422` is `n/a`: it exists because one Node process can hold several `pi-ai` copies. mhc links a
  single `maho-ai` crate, so there is no second copy to bridge.
