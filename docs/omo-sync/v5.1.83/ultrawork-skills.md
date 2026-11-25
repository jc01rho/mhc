# omo v5.1.80..v5.1.83 sync ledger — `ultrawork-skills` track

Source range: `822bd923038` (`v5.1.79`) .. `69a1863a457` (`v5.1.83`; `v5.1.80` was never released)
Upstream paths: `packages/omo-senpi/src/components/ultrawork`, `packages/omo-senpi/skills/ultrawork`, `packages/shared-skills`, `packages/omo-senpi/src/components/skill-pointers`, `packages/skills-loader-core`, `packages/omo-senpi/skills/AGENTS.md` (16 non-merge commits)
mhc counterparts: `crates/maho-ultrawork`, `crates/omo/components/maho-omo-skill-pointers`, `crates/omo/components/maho-omo-bundled-skills`, `crates/omo/skills-loader-core`, `assets/omo-latest/skills`

## Summary

| verdict | count |
|---|---:|
| port | 6 |
| ported | 2 |
| gap | 0 |
| n/a | 2 |
| **total** | **10** |

## Per-PR verdicts (draft — verifying against source)

| omo PR | verdict | mhc location or reason |
|---|---|---|
| `2393d144b6`, `62d085624f` (no PR; skill-pointers omo-relay fleet hint) | port | `crates/omo/components/maho-omo-skill-pointers/src/{index,pointer_text}.rs`, `tests/skill_pointers_bunshin.rs` |
| #14389 (`68f6df4838`, re-lands #14392) | port | `crates/omo/components/maho-omo-skill-pointers/src/strip_quoted_regions.rs`, `crates/maho-ultrawork/src/{classify,mask,mask_phrases,extension,lib}.rs`, `crates/omo/maho-omo/src/compose.rs` |
| #14388 (`e271f83646`) | port | `assets/omo-latest/skills/ulw-execute/SKILL.md`, `assets/omo-latest/manifest.json` |
| #14544 (`d5d1255be0`, `b346408286`, `f1ceae8317`, `f68964d8b4`) | port | `assets/omo-latest/skills/frontend/{SKILL.md,references/perfection/state-and-flow.md}`, `assets/omo-latest/skills/visual-qa/{AGENTS.md,SKILL.md,references/behaviour-evidence.md}`, `crates/omo/skills-loader-core` |
| #14602 (`eaef6a31fd`, `e9aa3b02e7`) | port | `assets/omo-latest/skills/programming/SKILL.md`, `assets/omo-latest/manifest.json` |
| #11711 (`fd3497e756`) | port | `assets/omo-latest/skills/browser/references/owned-engine/{README.md,frames-and-humans.md}`, `assets/omo-latest/manifest.json` |
| `ca7e2139e3` (no PR; omo-relay native skill) | ported | `assets/omo-latest/skills/omo-relay/**` and `crates/omo/components/maho-omo-bundled-skills/tests/all_skills_check.rs` (already in `main`; see `docs/omo-sync/v5.1.83/omo-sdk-relay.md`) |
| `b9c468be2f` (no PR; GPT-6.2 routing) | ported | `packages/omo-senpi/skills/AGENTS.md` doc table only in this track; model routing ported in `misc-core` and `senpi-task-core` |
| #14441 (`db42f0a6a7`) & #14442 (`f87b57c4b3`) | n/a | SUL `packages/omo-senpi/skills/ultrawork` and `src/components/ultrawork/directive-variants.ts` two-prompt directive split; `mhc` uses the clean-room MIT single-contract `crates/maho-ultrawork` (`PROVENANCE.md`), and `is_deepseek_v41_flash_model` / `is_gpt6_astra_model` were ported in `misc-core` |
| #14643 (`4ca98913ae`) | n/a | SUL `packages/omo-senpi/skills/ultrawork/SKILL.md` and `generated-directive.ts` wording edit; `mhc` uses clean-room MIT `crates/maho-ultrawork/contract/core.md` and `assets/omo-latest/skills/ultrawork/SKILL.md` |
