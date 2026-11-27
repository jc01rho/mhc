# senpi v2026.11.23 look-at slice (senpi#7260 / senpi#7233)

| TS path | Rust path | TS tests | Rust tests | status | N/A reason |
|---|---|---:|---:|---|---|
| image-placeholder.ts | src/image_placeholder.rs | 0 | 1 | done | |
| tool-result-images.ts | src/tool_result_images.rs | 0 | 2 | done | |
| prompts.ts | src/prompts.rs | 0 | 0 | done | |
| index.ts | src/index.rs | 0 | 2 | partial | `ExtensionApi::on` returns `()` without an unsubscribe handle (`crates/maho-ext-api/src/types.rs:5907`), so `register_tool_result_describer` keeps its per-event `describes_images_for` guard |
| ../../../../../test/suite/look-at-image-placeholder.test.ts | src/image_placeholder.rs | 1 | 2 | done | |
| ../../../../../test/suite/regressions/7233-non-vision-image-description.test.ts | tests/image_placeholder.rs, tests/tool_result_description.rs | 9 | 7 | partial | 2 of 9 cases not ported: `"leaves the result of a call another tool made as that tool gets it"` requires `parent_tool_call_id` on `maho_ext_api::ToolResultEvent`, and `"adds no hook run to a tool call while look_at is not offered"` requires an unsubscribe handle from `maho_ext_api::ExtensionApi::on` |
