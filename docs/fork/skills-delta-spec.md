# SKL-01 Skills Delta Specification (omo v5.1.53..v5.1.79 -> mhc)

## 1. Inventory Comparison (Upstream Shared & Native Skills vs mhc Bundled Overlay)

| Skill Name | Upstream Pool (v5.1.79 / dev) | mhc Status Before SKL-01 | SKL-01 Action & Source PRs |
|---|---|---|---|
| `adversarial-plan` | mhc clean-room (MIT) | Shipped in `assets/omo-latest` | Retained; coexists with `hyperplan` |
| `ast-grep` | `shared-skills` | Shipped | Verified references clean (`#12528`, `#12614`) |
| `browser` | `shared-skills` | Shipped | Added 0.4.0 `outdated-extension` doctor report (`scripts/browser-doctor-report.mjs`, `#12663`) |
| `coding-agent-sessions` | `shared-skills` | Shipped | Retained (`python3 -B` finder contract) |
| `dag-library` | `omo-senpi/skills` | Shipped | Verified references clean (`#12528`, `#12614`) |
| `data-scientist` | `shared-skills` | Shipped | Verified references clean (`#12528`, `#12614`) |
| `debugging` | `shared-skills` | Shipped | Verified references clean (`#12528`, `#12615`) |
| `eval-integrity` | `shared-skills` (new) | Missing | Added clean-room `eval-integrity/SKILL.md`, bundled opt-in off by default per `POL-03` (`#7593`) |
| `frontend` | `shared-skills` | Shipped | Added prolog-walking error probe placement (`render/probe-placement.ts`) and `severityFixed` (`#11837`) |
| `git-master` | `shared-skills` | Shipped | Verified references clean (`#12528`, `#12614`) |
| `give-me-tips` | `omo-senpi/skills` | Shipped | Verified references clean (`#12528`, `#12614`) |
| `hyperplan` | `omo-senpi/skills` + `skills-loader-core` | Retired in overlay | Restored as active bundled skill (`hyperplan/SKILL.md`) with `deep-low` degraded-roster rule (`#12484`, `#12528`) |
| `init-deep` | `omo-senpi/skills` + `shared-skills` | Shipped | Verified references clean (`#12528`, `#12614`) |
| `lsp-setup` | `shared-skills` | Shipped | Verified references clean (`#12528`) |
| `omo-relay` | `omo-senpi/skills` (new) | Missing | Added clean-room `omo-relay/SKILL.md`, `references/{errors,machines,parallel}.md`, `scripts/sdk/*`, and `maho-omo-skill-pointers` target (`#13446`, `#13467`, `#14404`) |
| `onboarding` | `omo-senpi/skills` | Shipped | Added `resolve_bundled_skill_provenance` in `maho-omo-bundled-skills` (`#12544`) |
| `programming` | `shared-skills` | Shipped | Code-ownership opening (`#13091`), Bun scaffolders with CRLF->LF normalization (`#11991`), testing references & junk detector + mutation proof (`#11967`, `#11972`) |
| `refactor` | `shared-skills` | Shipped | Verified references clean (`#12528`, `#12614`, `#12615`) |
| `remove-ai-slops` | `shared-skills` | Shipped | Wired `programming` test-junk detector (`check-test-junk.ts`) and serial `mutation-proof.ts` Phase 4b (`#12017`) |
| `review-work` | `shared-skills` | Shipped | Verified references clean (`#12528`) |
| `ultimate-browsing` | `shared-skills` | Shipped | Verified references clean (`#12528`, `#12614`) |
| `ultrawork` | `omo-senpi/skills` / `maho-ultrawork` | Shipped | Preserved byte-for-byte parity with `crates/maho-ultrawork/contract/` |
| `ulw-execute` | `shared-skills` | Shipped | Added `--light` profile (single worktree, no per-step ceremony, single end verification, `#14395`) |
| `ulw-loop` | `omo-senpi/skills` | Shipped | Added doctrine edge-reason wave rule (`#14083`) |
| `ulw-plan` | `omo-senpi/skills` + `shared-skills` | Shipped | Added doctrine edge-reason wave rule and dependency matrix columns (`#14083`) |
| `ulw-research` | `omo-senpi/skills` + `shared-skills` | Shipped | Added SVG `<foreignObject>` HTML parser and chart fact verification (`scripts/html-foreign-object.mjs`, `#12040`) |
| `visual-qa` | `shared-skills` | Shipped | Verified references clean (`#12528`, `#12614`) |
| `visualize` | `omo-senpi/skills` | Shipped | Added `:root { color-scheme: light dark; }`, HTML bar guidance, and 4-view chart measurement checks (`#12612`) |

## 2. Rust Component & Tooling Changes

1. `crates/omo/components/maho-omo-bundled-skills`:
   - `DEFAULT_OPT_IN_SKILLS = &["eval-integrity"]` and `is_opt_in_skill_enabled` (`POL-03`, `omo#7593`).
   - `BundledSkillProvenance` and `resolve_bundled_skill_provenance` (`omo#12544`).
2. `crates/omo/components/maho-omo-skill-pointers`:
   - `src/pointer_text.rs` module extracted (`skill_pointer`, `companion_pointer`, `ignore_when_only_discussed`, `omo#13467`).
   - `OMO_RELAY_CUSTOM_TYPE`, `BUNSHIN_TS_IMPORT_SAFETY`, `TARGET_OMO_RELAY` (`omo#13446`, `omo#13467`, `omo#14404`).
   - `PROHIBITED_ITEM` / `PROHIBITED_KEYWORD` negation masking in `src/strip_quoted_regions.rs` (`omo#14389`).
3. `tools/check-skill-refs.mjs`:
   - 3-pass reference validator scanning all shipped skills for dangling skill names, agent/category names (with Damerau-Levenshtein suggestion), and skill/repo-relative file paths (`omo#12484`, `omo#12528`, `omo#12614`, `omo#12615`).
