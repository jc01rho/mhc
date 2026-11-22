# senpi v2026.11.21-2 sync ledger: `session`

Source range: senpi `59d58cee5` (v2026.10.27-2) to `f01af13491` (v2026.11.21-2).
Track input: `/tmp/omo-mig2/tracks/session.tsv` (154 rows).

Summary: **154** rows — `ported`: **58**, `port`: **21**, `n/a`: **75**, `golden`: **0**.

| senpi PR | merge_sha | verdict | mhc location or reason |
|---|---|---|---|
| direct | `89092fa33e` | `n/a` | Merge of senpi#6858 (`packages/coding-agent/test/suite/project-trust-linked-worktree.test.ts`): TS test-only generation-bump check; mhc `crates/maho-core/src/trust_manager.rs` already tests trust store atomic saves |
| direct | `3182dac607` | `ported` | `crates/maho-socket-secret/src/lib.rs`, `crates/maho-agent/src/proxy.rs`, `crates/maho-ai/src/utils/retry.rs`, `crates/maho-core/src/agent_session.rs` (branch sync of socket-secret, proxy argument reset, billing-scope 503 retry, and required-compaction wording already on main via round-1 PRs #32, #38, #39) |
| #4388 | `2ba8f3342b` | `ported` | `crates/maho-core/src/trust_manager.rs`, `crates/maho-core/src/project_trust.rs`, `crates/maho-core/src/agent_session_runtime.rs` (project trust resolution and Full-access trust sync) |
| #4418 | `81e426cb8b` | `ported` | `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/retry_fallback/controller.rs`, `crates/maho-core/src/retry_fallback/controller_types.rs` (fallback switch context fit and turn refusals) |
| #4429 | `ab24e8c96a` | `port` | `crates/maho-core/src/model_resolver.rs` (`restore_model_from_session`), `crates/maho-core/src/sdk.rs` (`create_agent_session`) — keep a saved session model when its provider is signed out |
| #4435 | `c64ecb129e` | `port` | `crates/maho-core/src/session_lifecycle.rs` (`SessionLifecycleController::start_run`) — settle whatever the previous run consumed even when cut short by retry or recovery |
| #4456 | `4179b9d009` | `port` | `crates/maho-core/src/skills_prompt.rs` (`COMPACT_SKILL_CATALOG_DESCRIPTION_BYTES`, `TIGHT_ONE_LINE_DESCRIPTION_MAX_CHARS`, `compact_skill_line`, trigger-clause preserving one-line descriptions) |
| #4472 | `fdf34950b0` | `n/a` | TS characterization test only (`packages/coding-agent/test/characterization/two-sided-closure.test.ts`); mhc extensions are statically compiled Rust crates |
| #4476 | `45ecad8df2` | `n/a` | TS characterization test only (`packages/coding-agent/test/characterization/tool-order-once.test.ts`) |
| #4478 | `5245692031` | `n/a` | TS characterization test only (`packages/coding-agent/test/characterization/crash-recovery-today.test.ts`) |
| #4493 | `89bebab7a6` | `n/a` | TS characterization test only (`packages/coding-agent/test/characterization/writer-claims.test.ts`); covered by `crates/maho-core/src/session_writer_claims.rs` tests |
| #4498 | `fdbb027402` | `n/a` | TS characterization test only (`packages/coding-agent/test/characterization/bash-wake.test.ts`) |
| #4499 | `9159af81c0` | `n/a` | TS characterization test only (`packages/coding-agent/test/characterization/permission-order.test.ts`) |
| #4544 | `2435447166` | `ported` | `crates/maho-ai/src/auth/pool/select.rs`, `crates/maho-core/src/provider_failure.rs`, `crates/maho-core/src/credential_pool/failover.rs` (provider failure and blocked-slot user guidance) |
| #4556 | `6bee32e1bf` | `n/a` | TS CLI help-fast-path test only (`packages/coding-agent/test/help-flags-cache.test.ts`) |
| #4567 | `46641be47f` | `port` | `crates/maho-core/src/skills_prompt.rs` (`EXPLICIT_TRIGGER_CLAUSE` matched ahead of `VERB_TRIGGER_CLAUSE` in `one_line_description`) |
| #4592 | `1d4ce1a4ef` | `n/a` | TS host-cli-presenter test harness update (`packages/coding-agent/test/suite/host-cli-presenter.test.ts`) |
| #4594 | `a781d06c87` | `n/a` | Bun runtime-modules static import test only (`packages/coding-agent/test/bun/runtime-modules.test.ts`) |
| #4603 | `817e1ffd9b` | `ported` | `crates/maho-core/src/credential_accounts.rs`, `crates/builtins/maho-ext-gpt-account/src/lib.rs` (pooled account unblock and status reporting) |
| #4637 | `54eca1da9d` | `n/a` | TSX in-process CLI fast-path test only (`packages/coding-agent/test/cli-inprocess-fast-path.test.ts`) |
| #4656 | `4f03cea0c5` | `n/a` | TS host-cli-presenter event wait test harness only (`packages/coding-agent/test/suite/host-cli-presenter-waits.test.ts`) |
| #4687 | `743fe234c6` | `ported` | `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/engine_turn_hold.rs` (`INTERRUPTED_RUN_HOLD_KEY` holds automatic wakes while previous run is interrupted) |
| #4702 | `67182da3fa` | `n/a` | TS test quarantine helper only (`packages/coding-agent/test/support/quarantine.ts`) |
| #4736 | `2cd829c9a3` | `ported` | `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/lifecycle_session_binding.rs` (`mark_interrupted_from_custody` holds interrupted runs during recovery) |
| #4772 | `08a57d9eb8` | `n/a` | TS prompt-preset test update only (`packages/coding-agent/test/suite/prompt-presets-deepseek-v4-1-flash.test.ts`) |
| #4796 | `c32b414805` | `n/a` | TS terminal-lifetime regression test budget tweak (`packages/coding-agent/test/suite/regressions/4272-terminal-lifetime.test.ts`) |
| #4804 | `989e3f60e3` | `n/a` | TS child fixture and knip config for permission shutdown (`packages/coding-agent/test/suite/regressions/permission-system-shutdown-unhandled-rejection.test.ts`) |
| #4805 | `08fa18b79d` | `n/a` | TS codemode npm package deduplication test only (`packages/coding-agent/test/suite/regressions/codemode-builtin-dedupe.test.ts`) |
| #4823 | `ff6c5557c5` | `n/a` | TS terminal-restore shutdown diagnostics test helper only (`packages/coding-agent/test/suite/terminal-restore-shutdown-diagnostics.ts`) |
| #4854 | `3017c757b5` | `n/a` | Prebuilt JS bundle regeneration only (`packages/coding-agent/dist/cli.js`) |
| #4866 | `cd9b3e1ce2` | `n/a` | TS host-cli test suite readiness wait updates (`packages/coding-agent/test/suite/host-cli*.test.ts`) |
| #4883 | `04719acd7c` | `ported` | `crates/maho-core/src/credential_pool/rotation_stream.rs`, `crates/maho-core/src/provider_account_events.rs` (account health change notification) |
| #4885 | `e6852efe35` | `ported` | `crates/maho-ai/src/auth/pool/select.rs` (`is_blocked`), `crates/maho-core/src/credential_accounts.rs`, `crates/maho-core/src/credential_pool/failover.rs` (`is_available`) |
| #4891 | `39ce4c12b0` | `n/a` | TS test spy re-entry fix in `packages/coding-agent/test/suite/host-session-support.ts` |
| #4911 | `aa4168f652` | `n/a` | TS test exit-listener race fix (`packages/coding-agent/test/suite/session-worker-data.test.ts`) |
| #5027 | `6bcbc2a084` | `ported` | `crates/maho-core/src/credential_pool/failover.rs`, `crates/maho-core/src/credential_pool/rotation_stream.rs` (`acquire_half_open_lease` and probe lease handling) |
| #5034 | `2e9d20cae0` | `port` | `crates/maho-core/src/credential_pool/classify.rs` (`GUESSED_BACKOFF_CAP_MS` 30-minute cap on guessed exponential backoff in `rate_limit_cooldown`) |
| #5039 | `f288cd1e96` | `gap` | Experimental TS `packages/coding-agent/src/subscription-egress/*` schema/executor for remote hand plane; not part of mhc workspace |
| #5040 | `4f936ae750` | `port` | `crates/maho-core/src/credential_pool/classify.rs` (`limit_from_provider_code` outranks message prose for structured `rate_limit_exceeded` vs `usage_limit_reached`/`usage_not_included`) |
| #5041 | `5c35281892` | `ported` | `crates/maho-core/src/retry_fallback/controller.rs`, `crates/maho-core/src/retry_fallback/unusable.rs`, `crates/maho-core/src/provider_account_events.rs` (primary restore honors active hold and account change events clear provider holds) |
| #5043 | `af11e3ac8e` | `ported` | `crates/maho-core/src/credential_pool/state_store.rs` (`StoredSlotRevisions`, `applicable_stored_state`), `crates/maho-core/src/credential_pool/rotation_stream.rs` (ported in round-1 PR #32) |
| #5050 | `54605777f3` | `port` | `crates/maho-core/src/retry_fallback/expansion.rs` (`FIRST_PARTY_PROVIDERS`, `in_bare_scope`), `crates/maho-core/src/retry_fallback/chains.rs` — bare selectors expand only to first-party vendor providers |
| #5053 | `cf320e5e22` | `ported` | `crates/maho-core/src/subscription_limits/limit_store.rs`, `crates/maho-core/src/credential_pool/rotation_stream.rs`, `crates/maho-core/src/model_runtime.rs` (subscription limit headroom tracking) |
| #5057 | `5457929685` | `ported` | `crates/maho-core/src/credential_pool/state_store.rs` (`CredentialSlotState` serde deserialization already ignores unknown fields for forward compatibility across engine versions) |
| #5061 | `151ee23f50` | `port` | `crates/maho-core/src/skill_discovery.rs` (`collect_skill_entries_with_options`), `crates/maho-core/src/skills.rs` (`load_skills_from_dir_internal`) — symlinked skill directories use their own ignore context |
| #5062 | `a7bd68f7af` | `ported` | `crates/maho-core/src/diagnostics.rs`, `crates/maho-core/src/package_manager.rs`, `crates/maho-core/src/resource_loader.rs`, `crates/maho-core/src/skills.rs` (skill discovery diagnostics) |
| #5068 | `9815495b8e` | `ported` | `crates/maho-core/src/credential_policy_schema.rs`, `crates/maho-core/src/credential_pool/rotation_stream.rs`, `crates/maho-core/src/model_runtime.rs` (credential pool policy & slot selection) |
| #5074 | `63bad86b43` | `n/a` | TS unused export removal in `packages/coding-agent/src/core/settings-manager.ts` (`knip` cleanup) |
| #5104 | `70a1ec6f89` | `n/a` | TS reverse-bridge capacity test only (`packages/coding-agent/test/reverse-bridge.test.ts`) |
| #5112 | `6d0ec57e2f` | `n/a` | Windows private-fd test fixture EBADF fix (`packages/coding-agent/test/fixtures/private-fd-inherit-failure.ts`) |
| #5125 | `b99e0c3440` | `n/a` | TS `packages/coding-agent/src/private-fds.ts` and `.github/workflows/ci.yml` Windows pair proof gate; mhc does not use TS `private-fds.ts` |
| #5156 | `3e291d5bcb` | `ported` | `crates/maho-core/src/skill_invocation.rs`, `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/settings_manager.rs` (skill invocation token expansion) |
| #5178 | `dd2c20b785` | `n/a` | TS ESM import-graph test helper only (`packages/coding-agent/test/helpers/esm-import-graph-probe.ts`) |
| #5190 | `1ad27bcfdf` | `n/a` | `packages/coding-agent/src/core/session-import/jsonl-import.ts` is an unshipped experimental store importer absent in mhc (`SessionManager` owns JSONL loading in `crates/maho-core/src/session_manager.rs`) |
| #5203 | `493ea29149` | `ported` | `crates/maho-core/src/compaction/compaction.rs` (`CJK_CHAR_WEIGHT`, `cjk_extra_chars`, `estimate_cut_point_tokens` — ported in round-1 PR #32) |
| #5210 | `59c0062a87` | `n/a` | TS `knip` unused export removal (`PROBE_IN_FLIGHT` in `rotation-slots.ts`) |
| #5240 | `a539cf19b9` | `ported` | `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/tool_dispatch/tool_dispatcher.rs`, `crates/maho-core/src/tool_child_host_calls.rs` (nested tool call provenance and tool child dispatch) |
| #5258 | `14a6a361c8` | `port` | `crates/maho-core/src/work_obligations.rs` (`SessionWorkObligations::settle_ended_runs`), `crates/maho-core/src/session_lifecycle.rs` (`SessionLifecycleController::new` settles stale consumed records from prior runs) |
| #5268 | `495734aaa1` | `n/a` | Windows private-fd child inheritance test fixture and upstream CI workflow only |
| #5282 | `acca944b5f` | `n/a` | TS `knip` export visibility tweak in `packages/coding-agent/src/core/credential-pool/login-health.ts` |
| #5286 | `b1bd63b8c7` | `n/a` | TS test-only change in `packages/coding-agent/test/session-import/jsonl-import.test.ts` |
| #5287 | `b9629c2ffb` | `n/a` | `packages/coding-agent/src/core/session-import/jsonl-import.ts` experimental store importer not present in mhc |
| #5329 | `9078d38ecd` | `n/a` | TS faux-session-worker test fixture race fix (`packages/coding-agent/test/experimental-remote-runtime.test.ts`) |
| #5500 | `b1cd67705a` | `n/a` | Prebuilt JS bundle regeneration only (`packages/coding-agent/dist/cli.js`) |
| #5503 | `1ff0b7f969` | `ported` | `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/sdk.rs` (`reasoning_baseline` preserved across requests during configuration updates) |
| #5520 | `b8f4e6105c` | `port` | `crates/maho-core/src/retry_fallback/expansion.rs` (`in_bare_scope`, `rank_family_models` keeps bare fallback rungs on `session_provider` whenever it serves the family) |
| #5584 | `39eb164be5` | `n/a` | TS regression test only (`packages/coding-agent/test/suite/regressions/5583-entry-before-tool-execution.test.ts`) |
| #5598 | `6f0aa9ebd6` | `ported` | `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/retry_fallback/controller.rs`, `crates/maho-core/src/retry_fallback/unusable.rs`, `crates/maho-core/src/retry_fallback/usage_limit.rs` (pooled account usage-limit hold management) |
| #5604 | `8ae5bf1059` | `port` | `crates/maho-core/src/credential_pool/reset_time.rs` (`ISO_RESET_RE`, `from_iso_reset_prose` for `Resets at <ISO-8601>` weekly limit reset prose) |
| #5613 | `1a199e8a5f` | `ported` | `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/retry_fallback/controller.rs`, `crates/maho-core/src/retry_fallback/unusable.rs`, `crates/maho-core/src/retry_fallback/usage_limit.rs` (usage limit scope handling) |
| #5614 | `bc85af2aad` | `n/a` | TS compile-cache test teardown try/finally (`packages/coding-agent/test/experimental-source-compile-cache.test.ts`) |
| #5620 | `d71e6341ef` | `n/a` | TS compile-cache test global setup (`packages/coding-agent/test/helpers/source-compile-cache-global-setup.ts`) |
| #5622 | `4f703a9608` | `ported` | `crates/maho-core/src/session_holders.rs`, `crates/maho-core/src/schedule/runner_lease.rs` (session holder lock directory creation and release) |
| #5627 | `a2623b351d` | `ported` | `crates/maho-core/src/subscription_limits/usage_accounts.rs`, `crates/maho-core/src/subscription_limits/usage_cache.rs`, `crates/maho-core/src/subscription_limits/usage_poller.rs` (Claude CLI account usage polling) |
| #5666 | `0d698abbc0` | `n/a` | Bun Chrome WebView Windows PowerShell process tree helper (`packages/coding-agent/src/core/webview/*`); mhc codemode runs Bun WebView inside the JS kernel (`crates/maho-codemode`) |
| #5672 | `3da134e67a` | `n/a` | TS test cleanup EBUSY retry (`packages/coding-agent/test/suite/host-private-fds-pair.test.ts`) |
| #5682 | `e6b7dab9ef` | `ported` | `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/structured_session_lane.rs` (ordered lifecycle and agent event delivery) |
| #5694 | `2d4af712b5` | `n/a` | TS compile-cache test helper only (`packages/coding-agent/test/helpers/source-compile-cache-global-setup.ts`) |
| #5714 | `83f9943fd7` | `n/a` | TS auth-lock test timing tweak (`packages/coding-agent/test/suite/regressions/1542-auth-lock-across-refresh.test.ts`) |
| #5717 | `dccfa02fc2` | `n/a` | TS hooks trust storage test fixture (`packages/coding-agent/test/suite/hooks-trust-storage-aba.test.ts`) |
| #5754 | `9c8c4996ee` | `ported` | `crates/maho-core/src/agent_session_runtime.rs`, `crates/maho-interactive/src/interactive_mode.rs` (abort active run before session shutdown) |
| #5798 | `2ce05709c8` | `n/a` | TS turn inactivity lease test only (`packages/coding-agent/test/suite/regressions/3227-turn-inactivity-lease.test.ts`) |
| #5819 | `6a77049a2e` | `n/a` | TS ESM import-graph probe test helper only (`packages/coding-agent/test/helpers/esm-import-graph-probe.ts`) |
| #5852 | `118f23bdf7` | `ported` | `crates/maho-core/src/agent_session.rs`, `crates/maho-agent/src/agent_loop.rs` (host tool execution and after-tool-call hooks) |
| #5865 | `185072a8f6` | `ported` | `crates/maho-core/src/auth_storage.rs`, `crates/maho-core/src/resolve_config_value.rs`, `crates/maho-core/src/config_reference_redaction.rs` (OAuth credential refresh and config value resolution) |
| #5877 | `45a866d200` | `ported` | `crates/maho-core/src/credential_pool/state_store.rs`, `crates/maho-core/src/credential_pool/rotation_stream.rs` (`CredentialSlotRepository` pool state operations) |
| #5908 | `88b3efd275` | `n/a` | TS kill-guard test only (`packages/coding-agent/test/suite/kill-guard-runtime-delivery.test.ts`) |
| #5953 | `cb2be3eb3a` | `n/a` | TS terminal extension test only (`packages/coding-agent/test/suite/terminal-extension.test.ts`) |
| #5974 | `f4631771df` | `ported` | `crates/maho-ai/src/auth/pool/slots.rs`, `crates/maho-ai/src/providers/chatgpt_subscription.rs`, `crates/maho-core/src/credential_accounts.rs`, `crates/maho-core/src/model_runtime.rs`, `crates/maho-rpc/src/connection_handler.rs` (ChatGPT SIWC account management and reauthorization) |
| #5976 | `cb29521dbe` | `ported` | `crates/maho-core/src/credential_pool/state_store.rs`, `crates/maho-core/src/credential_pool/rotation_stream.rs` (`applicable_stored_state` and lineage checks — ported in round-1 PR #32) |
| #5981 | `9a17de2777` | `ported` | `crates/maho-core/src/credential_accounts.rs`, `crates/builtins/maho-ext-gpt-account/src/lib.rs` (account block reason display) |
| #5997 | `195fba3c12` | `n/a` | TS kill-guard sleeper test fixture only (`packages/coding-agent/test/kill-guard/*`) |
| #6019 | `86fad10b8d` | `n/a` | TS runtime-snapshot inventory phase budget test only (`packages/coding-agent/test/runtime-snapshot/inventory.test.ts`) |
| #6028 | `d32a1a0f6f` | `n/a` | TS test fixture-ready helper only (`packages/coding-agent/test/helpers/fixture-ready.ts`) |
| #6082 | `9492f7151f` | `gap` | TS hand-mode socket carrier (`packages/coding-agent/src/modes/hand/*`); mhc does not ship the TS hand mode |
| #6084 | `3146bd275a` | `ported` | `crates/maho-core/src/session_manager.rs` (in-memory / non-persisting sessions never fail write-guard ownership checks) |
| #6091 | `c21189cff7` | `n/a` | TS runtime-snapshot inventory test only (`packages/coding-agent/test/runtime-snapshot/inventory.test.ts`) |
| #6130 | `6a98ceb476` | `gap` | TS hand-mode socket carrier (`packages/coding-agent/src/modes/hand/socket-carrier.ts`); not shipped in mhc |
| #6143 | `f6599da86d` | `n/a` | Windows private-fd test cleanup retry (`packages/coding-agent/test/suite/host-private-fds-proof-gate.test.ts`) |
| #6146 | `d295cca929` | `n/a` | TS shutdown call-sites regression test only (`packages/coding-agent/test/suite/regressions/3067-shutdown-call-sites.test.ts`) |
| #6162 | `595014205c` | `n/a` | TS host-cli-presenter EPIPE test helper only (`packages/coding-agent/test/suite/host-cli-presenter-support.ts`) |
| #6192 | `41686cc758` | `ported` | `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/session_log.rs`, `crates/maho-core/src/extension_handler_stall_report.rs` (tool/event queue stall logging) |
| #6197 | `1fa609b6cd` | `ported` | `crates/maho-core/src/context_files.rs`, `crates/maho-core/src/resource_loader.rs`, `crates/maho-core/src/settings_manager.rs` (context file loading and settings resolution) |
| #6230 | `e7b059887e` | `n/a` | TS z1 session-step-commit-error test only (`packages/coding-agent/test/z1-session-step-commit-error.test.ts`) |
| #6239 | `19033de30d` | `ported` | `crates/maho-core/src/agent_session_services.rs`, `crates/maho-core/src/sdk.rs` (agent session creation options) |
| #6254 | `2a12409d5c` | `ported` | `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/credential_accounts.rs`, `crates/maho-core/src/credential_pool/model_scope.rs` (model-scoped usage limit checks) |
| #6275 | `2de4af53c1` | `port` | `crates/maho-core/src/credential_pool/classify.rs` (`MODEL_SCOPED_REFUSAL_TEXT` fails the request without blocking the slot on model-scoped 403/429 plan entitlement refusals) |
| #6285 | `ebde7d0e16` | `n/a` | TS startup filler FD hop (`packages/coding-agent/src/private-fd-startup-fillers.ts`); Node/Bun launcher artifact not used by native mhc binary |
| #6334 | `5a59643871` | `ported` | `crates/maho-core/src/config_reference_redaction.rs`, `crates/maho-core/src/retry_fallback/log.rs`, `crates/maho-core/src/retry_fallback/model_config_error.rs`, `crates/maho-core/src/agent_session.rs` (resolved config reference secret redaction) |
| #6357 | `0d9dfddde0` | `n/a` | Prebuilt JS bundle regeneration only (`packages/coding-agent/dist/cli.js`) |
| #6368 | `6ae4b91d88` | `n/a` | TS hooks command runner test fixture only (`packages/coding-agent/test/suite/hooks-command-runner.test.ts`) |
| #6384 | `c26714aaa4` | `ported` | `crates/maho-core/src/retry_fallback/controller.rs`, `crates/maho-core/src/retry_fallback/controller_types.rs`, `crates/maho-core/src/retry_fallback/cooldown.rs` (cooldown and limit-reset release) |
| #6389 | `a2f9ffbb67` | `port` | `crates/maho-core/src/retry_fallback/settings.rs` (`default_fallback_chains` adds `claude-fable-5-2` and routes `claude-opus-5-5` through `claude-fable-5-2` per owner Fable feature-update policy) |
| #6432 | `62c39e6ae3` | `ported` | `crates/maho-core/src/resource_loader.rs` (read-only extensions directory seed handling) |
| #6463 | `25198cb082` | `ported` | `crates/maho-core/src/session_move/import.rs`, `crates/maho-core/src/session_move/move_landing.rs`, `crates/maho-core/src/session_move/move_ledger.rs`, `crates/maho-core/src/session_rebind.rs` (session move landing & ledger) |
| #6502 | `d3265cb1aa` | `ported` | `crates/maho-core/src/credential_pool/health_write.rs`, `crates/maho-core/src/credential_pool/state_store.rs` (synchronous sidecar health writes need no async block-log flush queue) |
| #6504 | `53f3de9a19` | `ported` | `crates/maho-core/src/experimental/server.rs` (experimental server lock cleanup and activation timeout) |
| #6540 | `430637653b` | `ported` | `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/credential_accounts.rs`, `crates/maho-core/src/retry_fallback/controller.rs`, `crates/maho-core/src/retry_fallback/unusable.rs`, `crates/maho-core/src/subscription_limits/usage_cache.rs`, `crates/maho-core/src/subscription_limits/usage_poller.rs` |
| #6549 | `a03ac0c78c` | `ported` | `crates/maho-core/src/agent_session.rs`, `crates/maho-core/src/credential_accounts.rs` (follow-up refinements to #6540) |
| #6630 | `da540dafe5` | `n/a` | TS test teardown awaited rm only (`packages/coding-agent/test/suite/kimi-account-pool.test.ts`) |
| #6658 | `63f64d68b4` | `ported` | `crates/maho-core/src/agent_session.rs` (`configuration_update` live thinking level projection) |
| #6660 | `d5153f141f` | `n/a` | TS test flake fix only (`packages/coding-agent/test/suite/regressions/3067-shutdown-call-sites.test.ts`) |
| #6701 | `1b89946197` | `port` | `crates/maho-core/src/retry_fallback/settings.rs` (`claude-opus-5-5` -> `claude-fable-5-2:medium`, `kimi-k3.1-pro:max`; Fable chains inherit session thinking effort), `crates/maho-core/src/retry_fallback/expansion.rs` (`family_ids` `k\d` -> `kimi-` alias and `exactness`), `crates/maho-core/src/retry_fallback/chains.rs` (deduplicate expanded rungs), `crates/maho-core/src/retry_fallback/controller.rs` (active episode `chain_key` precedence) |
| #6713 | `46f9757aab` | `gap` | Experimental `OMO_TOOLS_VIA_DAEMON` device bash routing (`packages/coding-agent/src/core/tools/bash.ts`); mhc executes bash natively via `maho-tools` / `maho-tool-child` |
| #6772 | `147a0df07e` | `ported` | `crates/maho-ext-api/src/types.rs` (`CompactionRejectionCause::TransientFailure`), `crates/maho-core/src/agent_session.rs` (ported in round-1 PR #32) |
| #6776 | `7e573378c4` | `n/a` | TS bundle import-order fix in `packages/coding-agent/src/private-fds.ts`; not applicable to native Rust binary |
| #6814 | `0d818b089b` | `port` | `crates/maho-core/src/work_obligations.rs` (`sending`, `is_delivery_in_flight`, `end_delivery` recompute on open unconsumed record), `crates/maho-core/src/session_lifecycle.rs` (`delivery_in_flight` excluded from background waits and keeps session running) |
| #6822 | `32802050b8` | `port` | `crates/maho-core/src/work_obligations.rs` (`is_consumed_by`), `crates/maho-core/src/session_lifecycle.rs` (exclude `is_consumed_by(record, live_run)` from background waits) |
| #6828 | `da0d839e0b` | `ported` | `crates/maho-core/src/credential_proxy/declarations.rs`, `crates/maho-core/src/credential_proxy/proxy.rs`, `crates/maho-core/src/credential_proxy/runtime.rs`, `crates/builtins/maho-ext-credential-proxy/src/lib.rs` (ported in round-1 PR #35) |
| #6829 | `6fcbaad0ef` | `n/a` | TS umask test fix only (`packages/coding-agent/test/suite/regressions/3858-reference-files.test.ts`) |
| #6833 | `6735e0285e` | `ported` | `crates/maho-core/src/agent_session.rs` (`is_required_compaction_error` excluded from provider fallback — ported in round-1 PR #32) |
| #6838 | `6b8378205a` | `n/a` | TS `knip` unused export removal in `active-tools.ts` and `hand-tool-catalog.ts` |
| #6840 | `c87f32d5ff` | `ported` | `crates/maho-core/src/session_move/{bundle,export,finish,import,move_landing,move_ledger,ownership}.rs`, `crates/maho-rpc/src/session_move_{commands,wire}.rs` (session move bundle, ownership, and RPC status) |
| #6853 | `2a203956ef` | `n/a` | TS prompt-preset test assertion tweak only (`packages/coding-agent/test/suite/prompt-presets-shared-sections.test.ts`) |
| #6894 | `30918021e9` | `ported` | `crates/maho-core/src/agent_session.rs` (model switch during active retry loop) |
| #6902 | `cc72c971d4` | `ported` | `crates/maho-core/src/session_move/import.rs`, `crates/maho-rpc/src/session_move_commands.rs` (session move target validation) |
| #6905 | `bbc22e31fc` | `port` | `crates/maho-core/src/engine_turn_hold.rs` (`announced_seq`, `is_current_pause_seq`, `add_hold` announces `SessionPaused` for every non-`RunFence` hold), `crates/maho-core/src/agent_session.rs` (`resume_session` checks `is_current_pause_seq`) |
| #6906 | `3d7f9fc0b8` | `ported` | `crates/maho-core/src/messages.rs`, `crates/maho-core/src/agent_session.rs`, `crates/maho-codemode/src/tool/tool_result_marshal.rs` (`tool_result_is_error` — ported in round-1 PR #32) |
| #6913 | `17fa913d82` | `ported` | `crates/maho-core/src/agent_session.rs` (follow-up to #6894) |
| #6916 | `054afefe5d` | `port` | `crates/maho-core/src/engine_turn_hold.rs` (`release` emits `SessionResumed` once when the pause ends, paired with #6905) |
| #6941 | `1bae1eb929` | `ported` | `crates/maho-core/src/session_discovery.rs`, `crates/maho-core/src/session_manager.rs`, `crates/maho-core/src/session_summary_index.rs` (session discovery and summary indexing) |
| #6957 | `69b64a519e` | `port` | `crates/maho-core/src/agent_session.rs` (`AgentSession::automatic_input_hold_live`), `crates/maho-rpc/src/automatic_input_gate.rs` (`automatic_input_would_be_held`) |
| #6965 | `2a8fa398a2` | `n/a` | TS `knip` unused export removal in `packages/coding-agent/src/core/shared-session-stores.ts` |
| #6981 | `2bf7bf4d16` | `port` | `crates/maho-core/src/nearest_parent_config.rs` (`ACCOUNT_HOME_DIR` real account home stop boundary in `find_nearest_parent_config_dir`) |
| #6987 | `1728d1e6df` | `ported` | `crates/maho-tools/src/kernel_process_identity.rs` (`parse_ps_identity` with `TZ=UTC` — ported in round-1 PR #35) |
| #6993 | `4586143b1a` | `ported` | `crates/maho-tools/src/kernel_process_identity.rs` (`read_kernel_process_identity` returns `Option` and never panics on `ps` spawn failure — ported in round-1 PR #35) |
| #7001 | `bf5e12704c` | `n/a` | Prebuilt JS bundle regeneration and knip config only (`packages/coding-agent/dist/cli.js`, `knip.jsonc`) |
| #7003 | `c5030158ac` | `n/a` | TS test suite browser-opener guard only (`packages/coding-agent/test/setup.ts`) |
| #7030 | `ce5fdac7e3` | `n/a` | TS test flake fix only (`packages/coding-agent/test/suite/regressions/6199-open-waits-for-warm.test.ts`) |
| #7050 | `d59069f1fb` | `n/a` | TS test race fix only (`packages/coding-agent/test/suite/regressions/6972-session-spawn-attestation.test.ts`) |
| #7053 | `a08517757c` | `port` | `crates/maho-core/src/session_lifecycle.rs` (`SessionLifecycleController::run_started_here`), `crates/maho-core/src/lifecycle_session_binding.rs` (`LifecycleBindingHost::run_started_here` skips active custody of a run started in this process during `restore`) |
| #7066 | `63d1680ed6` | `n/a` | Upstream markdown documentation only (`packages/coding-agent/docs/compaction*.md`, `docs/settings.md`) |
| #7078 | `3099ce9cb0` | `n/a` | Upstream markdown documentation nits only (`packages/coding-agent/docs/compaction*.md`, `docs/settings.md`) |
| #7135 | `0a1a4aa71e` | `ported` | `crates/maho-core/src/agent_session.rs` (retry wait drain emits `AutoRetryEnd` for both cancelled and continued outcomes) |

Lead audit (2026-11-22): 4 rows first recorded as `n/a` are re-labelled `gap`: the upstream change is a real capability that mhc lacks because the subsystem it lives in is not ported. The track's own summary line above predates this audit.
