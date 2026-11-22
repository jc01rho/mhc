# senpi 59d58cee5..v2026.11.21-2 sync ledgers

Round 2 of the senpi sync: every commit on senpi's first-parent history from `59d58cee5` (v2026.10.27-2) to `f01af134919f` (v2026.11.21-2) is judged in one ledger below. `routing.md` covers the senpi-task categories at omo v5.1.79 and the Fable execution gate (`model_policy.block_fable`, on by default).

Verdicts: `port` ported in round 2, `ported` already in mhc, `golden` regenerated data only, `gap` a real capability mhc lacks because the subsystem it lives in is not ported, `n/a` no meaningful Rust counterpart. Rows listed under several ids in one ledger are counted once.

| ledger | port | ported | golden | gap | n/a | total |
|---|---|---|---|---|---|---|
| [ai](ai.md) | 27 | 9 | 43 | 0 | 4 | 83 |
| [agent](agent.md) | 4 | 2 | 0 | 23 | 11 | 40 |
| [session](session.md) | 21 | 57 | 0 | 4 | 71 | 153 |
| [rpc-host](rpc-host.md) | 4 | 2 | 0 | 3 | 50 | 59 |
| [rpc-mode](rpc-mode.md) | 29 | 48 | 0 | 49 | 50 | 176 |
| [ext-auth](ext-auth.md) | 30 | 6 | 0 | 5 | 0 | 41 |
| [ext-core](ext-core.md) | 11 | 39 | 2 | 12 | 27 | 91 |
| [tools](tools.md) | 7 | 8 | 0 | 29 | 6 | 50 |
| [codemode](codemode.md) | 7 | 4 | 2 | 10 | 27 | 50 |
| [tui](tui.md) | 11 | 7 | 2 | 0 | 5 | 25 |
| [lead](lead.md) | 0 | 0 | 0 | 0 | 126 | 126 |
| **total** | **151** | **182** | **49** | **135** | **377** | **894** |
