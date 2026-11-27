# senpi v2026.11.21-2..v2026.11.23 sync ledgers

Round 3 of the senpi sync: every entry on senpi's first-parent history from `f01af134919f` (v2026.11.21-2) to `47333c26cdba` (v2026.11.23) is judged in one ledger below, and each of the 83 non-merge commits of the range is named in exactly the row of the entry that carried it. This is the engine omo v5.1.84 bundles (`@code-yeongyu/senpi` 2026.11.23). The Rust work is the slice PRs and the lead PR of track senpi-engine of the omo v5.1.84 campaign.

Verdicts: `port` ported in this round, `ported` already in mhc, `golden` regenerated data only, `gap` a real capability mhc lacks because the subsystem it lives in is not ported, `n/a` no meaningful Rust counterpart (release, CI, docs, TS test harness). An entry judged in several rows of one ledger is counted once per row.

| ledger | port | ported | golden | gap | n/a | total |
|---|---|---|---|---|---|---|
| [ai](ai.md) | 1 | 0 | 0 | 0 | 0 | 1 |
| [catalog](catalog.md) | 0 | 0 | 3 | 0 | 0 | 3 |
| [codemode](codemode.md) | 1 | 0 | 0 | 0 | 1 | 2 |
| [lead](lead.md) | 0 | 0 | 0 | 0 | 25 | 25 |
| [prompt-preset](prompt-preset.md) | 1 | 0 | 0 | 0 | 0 | 1 |
| [rpc](rpc.md) | 2 | 0 | 0 | 1 | 3 | 6 |
| **total** | **5** | **0** | **3** | **1** | **29** | **38** |

Pins: this round changes neither `PINS.md` nor the CI `SENPI_PIN` (lead-owned). The CI `SENPI_PIN`, which must equal `tools/golden/pin.mjs` `SENPI_PIN`, needs to become `47333c26cdba96631d64317284a06ca6e0aab68e` (v2026.11.23) when the lead bumps the parity pin.
