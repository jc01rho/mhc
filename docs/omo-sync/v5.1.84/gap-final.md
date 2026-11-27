# omo v5.1.83..v5.1.84 sync ledger — `gap-final` track

Source range: `69a1863a457` (`v5.1.83`) .. `8dbccd12842f` (`v5.1.84`), repo code-yeongyu/oh-my-openagent.
This track closes the gaps the per-track ledgers left open, one slice per missing subsystem. Each slice records its
own verdicts in the ledger of the track that owns the upstream paths; this file is the index and the slice the sdk
track's own gap closed here.

## Slices

| slice | upstream | verdict | where |
|---|---|---|---|
| sdk | #14948 `bb8b61313a`, `5d2e861f2f` — the root's typed call table | ported | `crates/omo/omo-sdk-contract` `src/calls.rs`, `src/shape.rs`, `src/results/root.rs`, `tests/root_calls.rs`; see `crates/omo/omo-sdk-contract/parity.d/5184s.md` and `docs/omo-sync/v5.1.84/omo-sdk.md` |
| consumer | #14623 `dde26d7b4c`, `1e5ecd91a9` — store intake write and the delivery-outcome consumer | see `thread-remote.md`, `thread-gateway.md` | `crates/omo/components/maho-omo-gateway` |
| reply | #14542 `8409382654`, `ca85fc8b51` — the reply allowance and its grant, local device | see `thread-remote.md`, `thread-gateway.md` | `crates/omo/components/maho-omo-gateway` |
| resolve | #14305 `a5f001273f` — `sessions.resolve` on the shared thread-part ladder | see `thread-core.md`, `thread-remote.md` | `crates/omo/components/maho-omo-remote` |

## The sdk slice

The omo SDK root's type is computed from one table, `ROOT_CALLS` (134 rows over the 14 namespaces of design 4.2). That
table, its per-row input schemas, its answer shapes and its wait flag are data, so this slice carries them as data in
`crates/omo/omo-sdk-contract`:

- `src/shape.rs` is the TypeBox shape vocabulary as data (`Shape`, `FieldSpec`, `accepts`, `sample`,
  `all_fields_optional`, `names_field`).
- `src/calls.rs` is `ROOT_CALLS`: each row's `key`, input `Shape`, `AnswerShape` (`value`/`thread`/`stream`) and
  `waits`; `input_accepts` is `validate.ts`'s `inputAccepts` over every row, and `takes_deadline_option` is design 4.4's
  wait-options rule ("`deadline_ms` only on waitable calls whose input does not already name it").
- `src/results/root.rs` restates the answer records the contract directory declares itself; a credential-bearing call's
  answer stays with `src/results/custody.rs`, a thread call's with the thread contract.
- `tests/root_calls.rs` ports the table cases of `contract.test.ts` and `contract-controls.test.ts`.

Not carried, with the reason each is not: `root.ts` (`Tree`/`Member`/`Answer`/`OptionsFor`, `Partial<T> extends T`) is
TypeScript type-level machinery, and the root object is slice P1b's; `root-options.ts`, `cancellation.ts`'s signal
plumbing and `failure.ts`'s stream classes are runtime JS host surfaces; `gated.ts`, `ports.ts` and
`descriptor-ahead.ts` are TS parity tooling; `results/threads.ts` re-exports the thread contract's records, which
`maho-omo-gateway` carries.

The table agrees with the pinned TypeBox schemas on 4,398 generated probes over all 134 rows (each row's minimal sample,
a sample with every optional field filled, and every single-field removal, retype and unknown-key addition, nested),
except the 19 that name a JS handle (`api_key` on the five launch calls, `signal` on `engine.schedule.run` and
`threads.runToCompletion`): TypeBox types those `{}`, which admits any JSON value, while this crate's earlier slice ruled
a handle has no JSON form, and that ruling stands. The answer records agree on their own samples and field mutations.
