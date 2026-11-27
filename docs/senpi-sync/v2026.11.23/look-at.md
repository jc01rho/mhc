# senpi v2026.11.23 sync ledger — track `senpi-engine`, slice `look-at`

Range: `f01af134919f..47333c26cdba` (`v2026.11.21-2` to `v2026.11.23`).
Upstream merge: `fe4fab3b7e` (`senpi#7260`, fixes `senpi#7233`).

## Summary

- `port`: 1 (`#7260` `97fa9b11ed`)
- `ported`: 1 (`#7260` `b1a6f71d03`)
- `golden`: 0
- `gap`: 1 (`#7260` `47143cac80`)
- `n/a`: 1 (`#7260` `b8311af79d`)

## Verdicts

| senpi PR | verdict | mhc location or reason |
| --- | --- | --- |
| `#7260` (`97fa9b11ed`) | `port` | `crates/maho-ai/src/api/transform_messages.rs` (`ImageDowngradeOptions`, `downgrade_unsupported_images` + 2 unit tests from `packages/ai/test/transform-messages-image-description-tool.test.ts`), `crates/maho-ai/src/api/openai_completions/transform_messages.rs` (`ImageDowngradeOptions`, `downgrade_unsupported_images`), `crates/builtins/maho-ext-look-at/src/image_placeholder.rs` (`replace_images_naming_tool`, `try_replace_images_naming_tool` + unit test from `packages/coding-agent/test/suite/look-at-image-placeholder.test.ts`), `crates/builtins/maho-ext-look-at/src/index.rs` (`register_image_placeholder_hook` on `EventKind::Context` with `mutates_messages: Some(false)` preserving index-aligned `entry_ids`, `offers_look_at`), `crates/builtins/maho-ext-look-at/tests/image_placeholder.rs` |
| `#7260` (`b1a6f71d03`) | `ported` | `jc01rho/mhc#101` (`9836ad421`): `crates/builtins/maho-ext-look-at/src/tool_result_images.rs` (`describe_tool_result_images`, `ImageDescription`, `DescribedToolResult`, `MAX_DESCRIBED_IMAGES_PER_RESULT`), `crates/builtins/maho-ext-look-at/src/prompts.rs` (`TOOL_IMAGE_DESCRIPTION_GOAL`, `build_image_description_block`), `crates/builtins/maho-ext-look-at/src/index.rs` (`register_tool_result_describer`, `describes_images_for`), `crates/builtins/maho-ext-look-at/tests/native.rs` (`tool_result_image_is_described_for_a_model_without_image_input`); extended in this slice with multi-turn once-only and 4-image gallery regressions in `crates/builtins/maho-ext-look-at/tests/tool_result_description.rs` (`"leaves the result of a call another tool made as that tool gets it"` skipped because `maho_ext_api::ToolResultEvent` has no `parent_tool_call_id` field) |
| `#7260` (`47143cac80`) | `gap` | `crates/builtins/maho-ext-look-at/src/index.rs` extracts `offers_look_at`, but dynamic registration/unregistration of the `EventKind::ToolResult` describer (`dropToolResultDescriber`) requires `maho_ext_api::ExtensionApi::on` (`crates/maho-ext-api/src/types.rs:5907`) to return an unsubscribe handle and `ExtensionRuntime` to support handler removal; the per-event `describes_images_for` guard is retained |
| `#7260` (`b8311af79d`) | `n/a` | upstream `changes/unreleased/7260.md` changelog fragment only |
