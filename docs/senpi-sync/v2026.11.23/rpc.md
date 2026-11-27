# senpi v2026.11.23 sync ledger — track `rpc`

Range: `f01af13491..47333c26cd` (`v2026.11.21-2` to `v2026.11.23`).

Host RPC (`packages/coding-agent/src/modes/rpc`). Rows are first-parent entries; test-only entries carry the reason they need no Rust change.

## Summary

- `port`: 2
- `gap`: 1
- `n/a`: 3

## Verdicts

| senpi PR | verdict | mhc location or reason |
| --- | --- | --- |
| #7365 fix(rpc): a predecessor that exits inside the route handshake is a gone route (senpi#7202; merge `c23a86599`, commits `67ec3af017`, `fca1eb0b9b`) | gap | `modes/rpc/predecessor-{attach,discovery,peer}.ts` (`routeEndpointGone` classifies `ENOENT`/`ECONNREFUSED`/`ECONNRESET`/`EPIPE` as `generation_route_gone`) belong to senpi's predecessor routing proxy: route table, route peers, routed attach and discovery. mhc carries none of it (`crates/maho-rpc` only has the handoff identity code in `host_handoff*.rs`), the same missing subsystem as the v2026.11.21-2 `#7146` row of `rpc-mode.md`. When that subsystem is ported, the change is the one shared classification plus the `7202-route-reset-during-handshake` regression. |
| #7220 fix(rpc): do not recreate a removed endpoint for a late crash record (senpi#5090; merge `b596b1527`, commits `9dfb9c19c8`, `4aa98e245f`) | port | `crates/maho-rpc/src/host_crash_record.rs` (`append_and_prune` returns without writing when the daemon dir is gone, so storage removal stays final; test `a_record_for_a_removed_endpoint_is_dropped_and_recreates_nothing`), `crates/maho-rpc/src/host_lifecycle_drain.rs` (the overdue-alarm fixture creates its daemon dir first, as the launch does). The terminal-record path (`host_ensure_stop.rs` `record_escalation`, senpi `recordTerminalHostRecord`) writes through `record_host_crash`, so it follows the same rule; mhc has no `crashes.lock`, so the lock-side existence check has nothing to port. |
| #7137 perf(rpc): find_session reads the files named for the id before every header (senpi#7136; merge `bc12a85c9`, commits `02ef515482`, `eb36d398c3`, `d5255f53c9`, `ab74b97340`) | port | `crates/maho-rpc/src/host_session_find.rs` (`carrying`: the `<timestamp>_<durable id>.jsonl` files are read first and the header decides, every other header only when none carries the id; `jsonl_names`: session folders are listed `READERS` at a time; five tests covering the root scan, the named file, a decoy named for the id, a caller-chosen name and a late folder batch). `docs/rpc.md` has no mhc counterpart. |
| #7159 test(rpc): 7020 restart waits end with their step and the case budget (senpi#7081; merge `5a01d1a6c`, commits `ec1e1d1cd6`, `94917e983a`, `9720b01f99`) | n/a | TS vitest harness only (`test/helpers/rpc-fake-model.ts`, `rpc-restart-retained-support.ts`, regression `7020-host-restart-resumes-engine-run`): case timeouts derived from measured host starts and the lint-ratchet baseline. No product code changed. |
| #6563 test(rpc): keep RPC host fixture key roots private under any umask (merge `c935bb81a`, commits `6c86a209be`, `603e73f6a8`, `ba630b1cac`, `a16388a57e`) | n/a | TS test fixtures only (`test/helpers/rpc-host-daemon-sandbox.ts`, `rpc-host-endpoint-scratch.ts`, benchmarks, manual-qa) plus the knip baseline. The product side, a 0o700 daemon dir whatever the umask, is `crates/maho-rpc/src/host_daemon_paths.rs` (`DirBuilder::mode(0o700)` followed by `set_permissions`) and predates this range. |
| #6329 fix(coding-agent): synchronize shutdown recovery regression (senpi#6216, #6582; merge `a6e753b53`, commits `9c67fbd385`, `03b41b7eca`, `a245da5a75`) | n/a | One TS regression file (`test/suite/regressions/3067-recovery-keeps-original.test.ts`) now waits on the exit-wait registration instead of a module mock; no product code changed. |
