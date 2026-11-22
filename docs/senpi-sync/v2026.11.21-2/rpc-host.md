# rpc-host track: senpi v2026.11.21-2 (59 rows)

Round 2 of the mhc port, senpi `59d58cee5` (v2026.10.27-2) -> `f01af13491` (v2026.11.21-2). This track owns the senpi
host, server and protocol changes: `packages/coding-agent/src/modes/rpc`, `packages/machine-ops` and the
engine-socket surfaces those touch. Every row of `tracks/rpc-host.tsv` appears below with one verdict against the
mhc `main` this branch started from (`ed795da8d`).

Verdicts: 53 n/a, 2 ported, 4 port.

The four `port` rows are one commit each, with the upstream number in the subject. The parity fragments these
changes touch are `crates/maho-rpc/parity.d/5180r.md` (the new `session_drain_hold.rs`) and the existing rows of
`session_worker_requests.rs`, `session_worker_client.rs`, `worker_session_registry.rs`, `multi_session_host.rs`,
`rpc_types.rs` and `connection_handler.rs`.

| senpi PR | Subject | verdict | mhc location or reason |
|---|---|---|---|
| `direct` f01af13491 | release: v2026.11.21-2 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` cb7ba5904b | release: v2026.11.21 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` a76b6e98bc | release: v2026.11.20 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 99a26322f4 | release: v2026.11.19-5 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` e4de6cfe97 | release: v2026.11.19-4 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 173188f34e | release: v2026.11.19-3 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 1975499844 | release: v2026.11.19-2 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 1f335acde8 | release: v2026.11.19 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 82a79a55ac | release: v2026.11.18-3 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 2c28cd8c16 | release: v2026.11.18-2 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` e5c70922e8 | release: v2026.11.18 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` c35449798e | release: v2026.11.17-2 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` b793d2c46a | release: v2026.11.17 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 9ce169751e | release: v2026.11.15-4 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 0569033759 | release: v2026.11.15-3 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` bdfd45a31e | release: v2026.11.15-2 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 43bff8deab | release: v2026.11.15 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` a4d67dfd67 | release: v2026.11.13 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 9ab66273c5 | release: v2026.11.12 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 40b9289107 | release: v2026.11.11 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 1f9b266f7c | release: v2026.11.10-3 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` f6cd305b0f | release: v2026.11.10-2 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 5d424044fa | release: v2026.11.10-1 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 30af8b0fee | release: v2026.11.10 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` b177e57a96 | release: v2026.11.9-2 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` ecd78e432c | release: v2026.11.9 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 40e2655f0d | release: v2026.11.6-3 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` f388cc2f2e | release: v2026.11.6-2 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 3f80cb1e71 | release: v2026.11.6-1 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` df310d1e87 | release: v2026.11.5-1 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` b8bcde3bc3 | release: v2026.11.4-2 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 0733419aa1 | release: v2026.11.4-1 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 9d29f1f413 | release: v2026.11.3-3 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 1d12d0dc4b | release: v2026.11.3-2 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 15c5462aea | release: v2026.11.3-1 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 92314cc4be | release: v2026.11.2-3 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 7f9449d221 | release: v2026.11.2-2 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` cf0e3c13db | release: v2026.11.02-1 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` e005a4e122 | release: v2026.10.31-2 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` dfd9653174 | release: v2026.10.31-1 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 12c216e964 | release: v2026.10.30-2 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| `direct` 7d27baf893 | release: v2026.10.30-1 | n/a | a release commit: package versions, CHANGELOG/`changes` notes and lockfiles. mhc ports behavior, not senpi's package versions |
| #4641 |  | n/a | engine-socket recovery-page/catalog-node goldens for the desktop decoder; mhc ships no engine-socket decoder fixtures |
| #5603 |  | ported | round 1 (PR #39): `crates/maho-machine-ops` files/policy/realpath layer |
| #5655 |  | n/a | generated per-method inbound sample fixture (`docs/engine-socket/fixtures/rpc-inbound-samples.json`, its generator and test) for the desktop engine socket |
| #5684 |  | n/a | senpi-qa fused-equivalence goldens under `.agents/skills/senpi-qa/`; mhc ships no senpi QA harness |
| #5804 |  | n/a | admission-route golden mask (a decoder fixture) plus its regression test |
| #6726 |  | ported | round 1 (PR #39): `crates/maho-machine-ops/src/files/local_files.rs` `split_glob_base` with the `..`-after-wildcard refusal |
| #6994 |  | n/a | admission-route golden refresh (`method_classes` digest); test-only fixture |
| #4668 |  | n/a | engine-socket worker payload permits and fragmented transfers (`session-worker-permits.ts`, `compact-transfer-staging.ts`, `accepted-transcript-tail.ts`, the split `session-event-writer-*` lanes). mhc's worker runtime carries no engine-socket permit/transfer/intake lane: the acceptance lane (`session_acceptance_lane.rs`, `accepted_transcript.rs`) journals transcripts only, so there is nothing to bound or fragment |
| #4758 |  | port | `crates/maho-rpc/src/session_drain_hold.rs` + `HostCore::drain_sweep` holds `AgentSession::hold_automatic_turns` once per unattached open session |
| #5197 |  | n/a | bounded worker secret frames (`WorkerSecretIds`). The worker wire (`SessionWorkerToHost`/`HostToSessionWorker`) has no secret-input lane, so there is no side channel to bound; the bound alone would be unreachable code |
| #5807 |  | n/a | test-support fix: `peekSession` on the in-process host core plus regression tests for an order-based registry lookup. mhc's rigs read the entry by handle (`registry.get`), so the divergence does not exist |
| #5809 |  | gap | delegated connections: every `open_session` on a `declare_delegation` connection is retained. mhc has no delegated connection (`same_account_delegation_v1` unported, `declare_delegation` absent), so every open here is local |
| #6075 |  | gap | sealed secret prompt lifecycle follow-ups (`SecretInputPresentations.useSealed`, `WorkerHostSecrets.exited`). The sealed-holder lane does not exist in mhc: `secret_input.rs` covers the extension API's timeout and value rules only |
| #6281 |  | gap | `withReceipt` on an applied close reply. `command-authority.ts` (relayExec receipts) was never ported; no mhc response carries an `authority` field |
| #6449 |  | port | `HostCore::tear_down_released` (and `close_session`, both runtimes) clears every handover-parked record key of the session |
| #6686 |  | port | `session_cwd_not_directory` refusal: `maho-core` `session_cwd.rs` (`SessionCwdNotDirectoryError`, `session_cwd_not_directory`), `SessionSwitchFailure.not_directory`, `AgentSessionRuntimeError::SessionCwdNotDirectory`, and the `open_session` pre-check in `multi_session_host.rs` |
| #6815 |  | port | `SessionWorkerRequests::note_progress` re-arms the no-progress deadline on every worker message, `progress_budget_ms` starts a fresh budget after a quiet spell, and `WorkerSessionRegistry::open_session` refuses a quarantined session `session_path_in_use` with `{ reason: session_quarantined, action: retry }` |

## What was left out, and why

The engine-socket worker lane (`#4668`), the worker secret lane (`#5197`, `#6075`), the delegated connection
(`#5809`, `#6281`) and the desktop decoder's fixtures (`#4641`, `#5655`, `#5804`, `#5684`, `#6994`) are
one family: none of those layers exists in mhc. Porting a bound, a receipt or a fixture without the lane it protects
would add unreachable code and a ledger row that claims coverage mhc does not have, so each is recorded `n/a` with
the layer it needs named. The two machine-ops rows (`#5603`, `#6726`) were already ported by round 1's PR #39.

## Follow-ups found while porting

- `multi_session_host.rs` does not resolve `open_session.cwd` from the session file's header the way senpi's
  `openSessionFolder` does (`#6427`, ported in round 1 only as far as the profile), so the `session_cwd_not_directory`
  refusal judges the named or launch cwd. A header cwd that is a file is refused later, by the runtime, as
  `open_failed`.

Lead audit (2026-11-22): 3 rows first recorded as `n/a` are re-labelled `gap`: the upstream change is a real capability that mhc lacks because the subsystem it lives in is not ported. The track's own summary line above predates this audit.
