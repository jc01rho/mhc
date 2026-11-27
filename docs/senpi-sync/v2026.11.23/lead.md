# senpi v2026.11.23 sync ledger — track `lead`

Range: `f01af13491..47333c26cd` (`v2026.11.21-2` to `v2026.11.23`).

Release, CI, docs and test-runner entries that have no Rust counterpart. The sixteen commits of the `Merge remote-tracking branch 'origin/main'` entry are split: the three codemode commits are judged in [codemode.md](codemode.md), the other thirteen are listed here.

## Summary

- `n/a`: 25

## Verdicts

| senpi PR | verdict | mhc location or reason |
| --- | --- | --- |
| release: v2026.11.23 (`47333c26cd`) | n/a | Version bumps, CHANGELOGs, `changes.md` fragments and `install-lock/package-lock.json` only. mhc pins senpi by commit (`PINS.md`, lead-owned): the `.github/workflows/ci.yml` `SENPI_PIN` this release needs is `47333c26cdba96631d64317284a06ca6e0aab68e`, which equals `tools/golden/pin.mjs` `SENPI_PIN` once goldens are regenerated; this track changes neither. |
| release: v2026.11.22-2 (`3a1d8d79d7`) | n/a | Release metadata only (versions, CHANGELOGs, install lock). |
| release: v2026.11.22 (`e0f42c175b`) | n/a | Release metadata only (versions, CHANGELOGs, install lock). |
| Add [Unreleased] section for next cycle (`6d91e370cf`) | n/a | CHANGELOG headings only. |
| Add [Unreleased] section for next cycle (`d6d24e00df`) | n/a | CHANGELOG headings only. |
| Add [Unreleased] section for next cycle (`7f9e81f045`) | n/a | CHANGELOG headings only. |
| #7194 fix(ci): pilot exact npm tree caching for coding-agent shards (merge `f6c460808`, commits `74f525b991`) | n/a | senpi GitHub Actions workflow (npm tree cache); mhc caches with `Swatinem/rust-cache`. |
| #7184 fix(ci): balance coding-agent shards by duration (senpi#7183) (merge `6238cf996`, commits `9d8caaef3e`, `5f970a8519`, `e20d079e5a`) | n/a | senpi CI shard scripts (`scripts/ci-coding-agent-shards*.mjs`) and the knip entry; mhc shards by crate with `scripts/ci-shard.py`. |
| #7175 fix(release): ship the committed model catalog instead of regenerating it in the release (merge `fdd7be3c7`, commits `4ef64f0f95`, `4cdae45376`, `919c10f9f6`, `fc7692116c`, `ee25b0040a`) | n/a | senpi release workflow and scripts (`scripts/release*.mjs`, `publish*.mjs`, `model-catalog-refresh.mjs`); mhc has no npm release. The catalog data these PRs refresh is judged in [catalog.md](catalog.md). |
| #7185 test(telemetry): run the workspace suite with native Bun (merge `b62a7cd7a`, commits `3f7ff9f23f`, `d7c29b9252`) | n/a | TS test-runner selection and docs for `packages/telemetry`; no Rust counterpart change. |
| #7187 test(sqlite-bun): run durable store tests with native Bun (merge `92806cf13`, commits `d8225420da`) | n/a | TS test-runner selection for the session-backend packages. |
| #7174 fix(release): publish-only treats a version a concurrent run already published as done (senpi#1831) (merge `bfba6f310`, commits `2216ff1608`, `29618766d0`) | n/a | npm publish script and its change fragment. |
| fix(ci): preserve cold-build and macOS timeout budgets (inside `d8c9366aa` Merge remote-tracking branch 'origin/main', `c7e3bcf308`) | n/a | senpi CI workflow timeouts. |
| fix(test): observe actual resident failover generations (senpi#7197) (inside `d8c9366aa` Merge remote-tracking branch 'origin/main', `62019b09ca`) | n/a | One TS test (`test/anthropic-subscription-auth-lane.test.ts`); no product code. |
| docs(ai): no release entry point regenerates the catalog any more (inside `d8c9366aa` Merge remote-tracking branch 'origin/main', `f0097b8913`) | n/a | `packages/ai/scripts/AGENTS.md` prose. |
| fix(ci): set job timeouts from measured runner p95 (inside `d8c9366aa` Merge remote-tracking branch 'origin/main', `2cee459a64`) | n/a | senpi CI workflow timeouts. |
| fix(release): the preflight names what the release job does with a superseded CI run (inside `d8c9366aa` Merge remote-tracking branch 'origin/main', `3eb9fe89b7`) | n/a | senpi release preflight script. |
| fix(release): keep preflight helpers module-private where nothing imports them (knip) (inside `d8c9366aa` Merge remote-tracking branch 'origin/main', `31db4a4e3c`) | n/a | senpi release preflight script. |
| test(mcp): await reconnect events without a second deadline (inside `d8c9366aa` Merge remote-tracking branch 'origin/main', `a74d82202e`) | n/a | TS MCP reconnect fixtures and tests only; no product code. |
| ci: initialize timeout tuning draft (inside `d8c9366aa` Merge remote-tracking branch 'origin/main', `a92419f250`) | n/a | Empty commit. |
| fix(release): the preflight can read refresh PRs and logs the CI gate it leaves to the release job (inside `d8c9366aa` Merge remote-tracking branch 'origin/main', `9ced8b70a0`) | n/a | senpi release preflight and publish workflow. |
| test(server): run server tests with native Bun (inside `d8c9366aa` Merge remote-tracking branch 'origin/main', `321fe1055e`) | n/a | TS test-runner selection for `packages/server`. |
| test(ci): pin auto-merge on both catalog refresh PRs (inside `d8c9366aa` Merge remote-tracking branch 'origin/main', `aa4c52e013`) | n/a | senpi workflow test for the catalog refresh automation. |
| fix(release): the preflight warns about a stale catalog refresh PR and leaves the publish-only guard offline (inside `d8c9366aa` Merge remote-tracking branch 'origin/main', `047b37408b`) | n/a | senpi release preflight, publish workflow and release skill. |
| fix(release): check everything knowable at dispatch in a first-minute preflight job (inside `d8c9366aa` Merge remote-tracking branch 'origin/main', `52adf111a6`) | n/a | senpi release preflight job and scripts. |
