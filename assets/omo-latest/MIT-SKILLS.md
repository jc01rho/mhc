# MIT-licensed skills (relicensed from lazycodex)

These skill directories were replaced with the MIT-licensed copies published in
`code-yeongyu/lazycodex` @ `75b017473c71d669dd9aed0a10e950e70724b283`
(`plugins/omo/skills/<name>`). License text: `LICENSES/lazycodex-MIT.txt`
(Copyright (c) 2026 Yeongyu Kim). `ast-grep` also carries its own `LICENSE`
(upstream `code-yeongyu/ast-grep-skill`, MIT).

| Skill | Changes made in mhc |
| --- | --- |
| coding-agent-sessions | Removed Codex-only `agents/` role files and the "Codex Harness Tool Compatibility" section |
| data-scientist | same |
| debugging | same |
| lsp-setup | same |
| remove-ai-slops | same; restored a top-level heading |
| git-master | same; frontmatter `description` rewritten by mhc |
| ast-grep | same |
| ultrawork | rewritten clean-room (todo 15, MIT) as the contract loaded on `/skill:ultrawork`, with on-demand `references/*.md`; the injected directive is `crates/maho-ultrawork/contract/core.md` |
| ulw-execute | same; Codex harness compatibility table replaced by mhc delegation notes |
| ulw-loop | same; driven through `mhc ulw-loop` (the `--codex-goal-json` flag keeps its historical name) |
| ulw-plan | same; planning delegated to `task` children |
| ulw-research | same; research lanes on `task`/teams |

The `ultrawork` and `ulw-*` rows were added on 2026-10-27 from the same
lazycodex commit; their manifest entries carry the source
`lazycodex@75b01747:plugins/omo/skills/<name>/<path> (MIT)`.

No content from the SUL-1.0 OmO upstream (`assets/omo-latest` previous bytes,
`oh-my-openagent`) remains in these directories.

## Clean-room skills (2026-10-26)

Written by implementers who were given only a functional spec and the MIT base
(lazycodex @ 75b01747) and never read the SUL-1.0 versions. Each directory has a
`CLEANROOM.md` with inputs and a requirement checklist.

| Skill | Base | Notes |
| --- | --- | --- |
| browser | lazycodex MIT | engine guard rewritten (connected/builtin/none policy, ask-before-irreversible) |
| ultimate-browsing | lazycodex MIT | broadened routing scope |
| frontend | lazycodex MIT / Apache-2.0 parts | capability additions only; base refinements kept |
| init-deep | lazycodex MIT (matrix/templates) | full rewrite; state in `.mhc/init-deep/` |
| refactor, review-work, visual-qa, programming | lazycodex MIT | Codex-only parts removed, base features kept |
| dag-library, visualize, adversarial-plan | none | written from spec only; MIT (c) 2026 Indo Yoon. `adversarial-plan` replaces `hyperplan` |
| give-me-tips (2026-10-27) | none | written from spec only; MIT (c) 2026 Indo Yoon; `tests/static-check.mjs` |
| onboarding (2026-10-27) | none | written from spec only; MIT (c) 2026 Indo Yoon; ships `scripts/savings-estimate.mjs`, `scripts/init-deep-gate.mjs` and `tests/` (replacing `qa-validator.sh` / `qa-savings-fixture.sh`) |

## Removed

- `mass-ulw` (2026-10-27): dropped from the bundle and listed under `retired` in
  `manifest.json`; graph work goes through the `workflow` tool directly.
- `hyperplan` (2026-10-26): replaced by `adversarial-plan`.

## Still SUL-1.0

No whole skill under `assets/omo-latest/skills` is SUL-1.0. One subtree is: `frontend/scripts/detect/**` and `frontend/references/perfection/detect.md` (the detect SDK) are copied unchanged from oh-my-openagent v5.1.52 (`354bcf9fc4`) and stay SUL-1.0 (`frontend/scripts/detect/LICENSE.md`, `frontend/ATTRIBUTION.md` section 7). The MIT profile (`package-native --mit`) stages no skills, so they never ship there. Out of scope
for this file: the runtime crates under `crates/omo/**` (for example
`maho-omo-skill-pointers`,
`maho-omo-onboarding`, `maho-omo-init-deep-advisor`) remain
`LicenseRef-SUL-1.0`, as does the pool-root developer doc
`assets/omo-latest/skills/AGENTS.md` (`poolDocs`), which is not a skill.

## Sync to omo v5.1.52 (2026-10-30)

Skills updated from `code-yeongyu/lazycodex` @ `e809c369053cd5eba7c6008c3dcdde8ac672f505`
(2026-10-28, "sync Codex marketplace v5.1.51"; before the v5.1.52 tag, and its
`plugins/omo/skills/<name>` files are byte-equal to the v5.1.52 contract). Changed
files carry the manifest source `lazycodex@e809c369:plugins/omo/skills/<name>/<path> (MIT)`;
mhc adaptations listed above are kept.

| Skill | lazycodex SHA | Notes |
| --- | --- | --- |
| ast-grep | e809c369 | downloaded `sg` goes to the user cache (`OMO_AST_GREP_BIN_DIR`, else XDG/LOCALAPPDATA `omo-ast-grep/bin`), never the skill dir |
| coding-agent-sessions | e809c369 | finder runs with `python3 -B` and sets `sys.dont_write_bytecode` before importing its package |
| ulw-research | e809c369 | scripts and references byte-equal to lazycodex@e809c369; `SKILL.md` follows the new router + `references/research-collection.md` / `research-delivery.md` split with the mhc tool notes (`task`/`team_create`/`workflow`, `mhc ulw-loop`) re-applied in those references; `AGENTS.md` keeps the mhc single-copy notes |

## Browser skill sync to omo v5.1.84 (2026-11-27)

Docs follow `code-yeongyu/lazycodex` @ `ebf7c57` (2026-11-26, "sync Codex marketplace v5.1.84"; MIT), which
carries omo `a160f55ea5`, `9ae70c3e1a`, `9bdd640262` and `3dda9ef757`. The engine runtime moved with them:
`manifest.json` `browserRuntime` pins omowright v0.3.4 (`d3b1a5d3c4`), the release omo v5.1.84 resolves.
Manifest sources keep their clean-room label; the bytes below are what changed.

| File | Notes |
| --- | --- |
| browser/SKILL.md, references/commands.md, references/install.md | three-way merge over lazycodex@75b01747 (the clean-room base), so only the mhc adaptations survive as differences: upstream's BrowserSkill 0.5 shared-control contract, wait and token-budget rules and the doctor state table are taken; the `OMO_BROWSER_ENGINE` engine policy (SKILL.md Step 0), the typed guard errors in `commands.md` and the install note are kept. The OmO Desktop built-in browser (`connectBuiltinBrowser()`, `references/desktop.md`) is not ported: the clean-room guard removed it (`browser/CLEANROOM.md` R1) and `OMO_BROWSER_ENGINE=builtin` hands the browser to the host's own tools |
| browser/references/owned-engine/*, references/recipes/1password.md, ATTRIBUTION.md | byte-equal to lazycodex@ebf7c57 (no mhc adaptation) |
| browser/scripts/omowright.mjs | after the staged runtime, a `node_modules/omowright` checkout resolves in the stager's layout order (`src/index.ts`, `src/index.js`, `dist/index.js`) |
| visual-qa/references/browser-setup.md, debugging/references/tools/browser-qa.md | byte-equal to lazycodex@ebf7c57 |
| ultimate-browsing/SKILL.md | the one-line `connectPipe` browser path from lazycodex@ebf7c57; the mhc routing text is kept |
