# senpi v2026.11.23 sync ledger — track `prompt-preset`

Range: `f01af13491..47333c26cd` (`v2026.11.21-2` to `v2026.11.23`).

`crates/builtins/maho-ext-prompt-preset` carries one wording per dialect for the execution-tooling rules: the text senpi calls `fullDirective`. The `directive` (diet, senpi#5335) variants and the `sharedSections` switch of `execution-tooling.ts` are not carried, an existing divergence this range does not change; senpi#7275 is applied to the wording mhc renders.

## Summary

- `port`: 1

## Verdicts

| senpi PR | verdict | mhc location or reason |
| --- | --- | --- |
| #7275 fix(prompt-preset): the routing rule names only the editing tools the session offers (senpi#7232; merge `182b56f3d`, commits `585f305618`, `e8204f3394`, `0d6527eaf2`, `0cfdc766f7`) | port | `src/execution_tooling.rs` (`render_execution_tooling_directive`; the \[\[...\]\] clause of `eval-routing-decision` and `eval-stay-direct` is filled from `resolve_file_mutation_routing` or dropped when the session offers no editing tool; the two rules' wording equals senpi's v2026.11.23 `fullDirective`), `src/file_operations.rs` (`format_tool_list`, shared with the file-operations tuning), tests `tests/execution_tooling.rs` (`the_section_names_exactly_the_offered_editing_tools`, `editing_tools_alone_render_no_section_and_no_tool_name`, `clause_markers_never_reach_a_rendered_section`) and `tests/file_operations.rs`. The TS native baseline JSON and shared-sections tests pin the diet wording and have no counterpart. |
