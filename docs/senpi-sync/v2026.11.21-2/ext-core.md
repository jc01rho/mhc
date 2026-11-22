# senpi v2026.11.21-2 sync ledger: `ext-core`

Source range: senpi `59d58cee5` (v2026.10.27-2) to `f01af13491` (v2026.11.21-2).
Track input: `/tmp/omo-mig2/tracks/ext-core.tsv` (92 rows).

Summary: **92** rows — `ported`: **40**, `port`: **7**, `n/a`: **42**, `golden`: **3**.

| senpi PR | merge_sha | verdict | mhc location or reason |
|---|---|---|---|
| direct | `9d9af3dab3` | `ported` | `crates/builtins/maho-ext-mcp/src/auth/{oauth_provider,oauth_issuer,token_store}.rs`, `crates/maho-core/src/input_origin.rs` (branch sync of OAuth redirect and turn-origin changes already on main) |
| direct | `265fe85426` | `ported` | `crates/builtins/maho-ext-goal/src/{index,stop_pause,elapsed_ticker}.rs` (paused goal elapsed freeze on a held session) |
| #3281 | `be7383091e` | `n/a` | TUI footer watching-indicator animation pulse (`WATCH_PULSE_FRAMES`, `WATCH_GLYPH`); `maho-ext-terminal/src/monitor_status.rs` renders a static `watching ...` status without decorative glyph pulsing |
| #4384 | `931f007cde` | `n/a` | TS-only `decideCellOperation` host entry for the senpi JS cell authorizer (`permission-system/cell-decision.ts`); mhc codemode dispatches tool calls through `maho-codemode` and `PermissionService::ask_with` |
| #4438 | `72cff1101a` | `n/a` | Extension-registered OAuth `provider_login_start` hook for dynamic JS extensions that pin external CLI binaries; mhc extensions are statically compiled native crates |
| #4450 | `8145cf913d` | `ported` | `crates/maho-ext-host/src/runner.rs` and `crates/builtins/maho-ext-mcp/src/index.rs` (builtin registration order keeps `<mcp_instructions>` at the tail of `before_agent_start`) |
| #4454 | `5b008dce17` | `ported` | `crates/builtins/maho-ext-terminal/src/shared.rs` (`TERMINAL_COMPANION_TOOLS`), `crates/builtins/maho-ext-goal/src/tool_registration.rs`, `crates/builtins/maho-ext-html-render/src/tool.rs` |
| #4455 | `8b078d2ffe` | `ported` | `crates/maho-ai/src/api/anthropic_messages.rs`, `crates/builtins/maho-ext-cache-keepalive/src/lib.rs`, `crates/builtins/maho-ext-compaction/src/speculative_summary.rs` (cross-session stable system prefix cache boundary) |
| #4482 | `1c0e77a3f1` | `ported` | `crates/maho-core/src/input_origin.rs`, `crates/maho-core/src/agent_session.rs`, `crates/maho-rpc/src/connection_handler.rs` (input attribution and queued input dispositions) |
| #4560 | `5769117562` | `n/a` | Upstream markdown documentation only (`packages/coding-agent/docs/terminal-tools.md`, `AGENTS.md`, `changes/unreleased/4560.md`) |
| #4604 | `40fc7a0c1d` | `ported` | `crates/maho-ext-api/src/types.rs` (`RegisteredMcpServerDeclaration`), `crates/builtins/maho-ext-mcp/src/{service,service_types,host_registry}.rs` |
| #4625 | `5f7fc83312` | `n/a` | Upstream JSDoc and documentation only (`packages/coding-agent/docs/extensions.md`, `src/core/extensions/types.ts`) |
| #4681 | `9d55fe2f86` | `n/a` | `builtin/job-widgets` P1 spec/validator: a model-authored live background-job widget subsystem (needs `re2js` plus background LLM authoring) that mhc does not ship |
| #4682 | `443ed3c7f4` | `n/a` | `builtin/job-widgets` P1b `re2js` line matcher (`bun.lock`/`package.json` JS dependency); `job-widgets` is not shipped in mhc |
| #4685 | `72010b941a` | `n/a` | `builtin/job-widgets` P2 model spec author and sample redactor; `job-widgets` is not shipped in mhc |
| #4686 | `a2c41837f2` | `n/a` | `builtin/job-widgets` P3 background-session wiring and the `job_widget_state` event; `job-widgets` is not shipped in mhc |
| #4691 | `55d1b64d45` | `n/a` | `builtin/job-widgets` P4 TUI sparkline/meter widget rendering; `job-widgets` is not shipped in mhc |
| #4695 | `b7be6744c8` | `n/a` | `builtin/job-widgets` P4b review polish and `NOTICE.md`; `job-widgets` is not shipped in mhc |
| #4718 | `f4e7038edb` | `ported` | `crates/builtins/maho-ext-terminal/src/{extension_state,monitor_notify,notify,restore}.rs` (a retired extension context stops the monitor notifier instead of throwing) |
| #4774 | `696aa36a6b` | `ported` | `crates/builtins/maho-ext-terminal/src/{extension,prompt}.rs` (`build_terminal_prompt_section` is the single prompt owner for monitor guidance) |
| #4782 | `e6c0f4fc24` | `n/a` | Deletes the duplicate upstream TS test `test/mcp/native-anthropic.test.ts` and its `knip.jsonc` entry |
| #4809 | `7e3cc2f173` | `n/a` | Bun dynamic TS extension loader `import.meta.filename`/`dirname` shim (`bun-extension-registry.ts`); mhc has no dynamic TS extension loader |
| #4812 | `fb431365a5` | `ported` | `crates/maho-core/src/model_registry.rs` and `crates/maho-ai/src/models.rs` (`models.json` `baseUrl` precedence over extension-registered providers) |
| #4833 | `4d5bdc0a26` | `ported` | `crates/builtins/maho-ext-ask-user/src/{extension,resume}.rs`, `crates/maho-ext-host/src/runner.rs` (a recovered question is published after `session_start` preparation) |
| #4850 | `090cf665c8` | `ported` | `crates/builtins/maho-ext-mcp/src/{control_inventory,index,service,startup_race}.rs`, `crates/maho-rpc/src/connection_handler.rs` (deterministic startup surfaces once MCP startup settles) |
| #4927 | `5ace14addf` | `ported` | `crates/builtins/maho-ext-mcp/src/auth/{oauth,oauth_issuer,oauth_provider}.rs` and `tests/auth_discovery.rs` (`trusted_discovery_state` never persists or contacts an unbound discovery URL from `tokens.json`) |
| #4936 | `59b5053720` | `n/a` | TS knip dead-export cleanup (unexporting `discoveryNamesItsServer`) plus an `.agents/skills/senpi-qa` script tweak |
| #5092 | `d8836b4fbc` | `ported` | `crates/builtins/maho-ext-goal/src/{index,monitor_continuation,provider_retry}.rs` (an exhausted fallback ladder resumes the goal at the soonest limit reset) |
| #5146 | `68a2b735fc` | `ported` | `crates/builtins/maho-ext-bash-timeout/src/{lib,timeout}.rs` (`build_bash_timeout_prompt` uses the `eval_only` callable wording when the `eval` tool is active) |
| #5152 | `df54e75c59` | `port` | `crates/builtins/maho-ext-todotools/src/normalize.rs` (`unwrap_todo_list`, `prepare_todo_arguments`, and unit tests for nested-array and object `list` wrappers) |
| #5224 | `85d909448e` | `n/a` | Sessionless runtime split `core/runtime/credential-resolver.ts` (RT4) for loopless hand execution; mhc runs `maho-ext-imagegen` and `maho-ext-websearch` against `ExtensionContext::model_registry` |
| #5225 | `b4c4e17087` | `n/a` | Sessionless runtime split (RT5a) that boots `tool_search` on a context without a `SessionManager`; mhc `ExtensionContext` always carries `Arc<dyn SessionManager>` |
| #5245 | `7be1a53fc5` | `n/a` | Sessionless runtime split (RT5) `attachWithoutSession` in `permission-system` when `ctx.sessionManager === undefined`; mhc `ExtensionContext` always carries `session_manager` |
| #5247 | `b07ac902cb` | `n/a` | Sessionless runtime split (RT5b) attaching MCP on a context without `sessionManager`; mhc `ExtensionContext` always carries `session_manager` |
| #5250 | `bfef6eb446` | `n/a` | Sessionless runtime split (RT11) `core/runtime/ui-broker.ts` and runtime-versus-loop event placement for loopless hands; mhc has no split loopless hand runtime |
| #5290 | `9b360172cc` | `port` | `crates/builtins/maho-ext-mcp/src/expose/tier_b.rs` and `crates/builtins/maho-ext-mcp/tests/registration.rs` (direct-mode MCP tools sort deterministically whatever order servers connect in) |
| #5317 | `eb772af773` | `port` | `crates/builtins/maho-ext-mcp/src/expose/tier_b.rs` and `crates/builtins/maho-ext-mcp/tests/registration.rs` (`server_gateway_names` keeps the fixed resource utility tools ahead of the per-server tools) |
| #5324 | `e7b8c78214` | `n/a` | Follow-up to #5225 (RT5a) that drops the sessionless first-use `tool_call` boot hook at `session_start`; mhc never registers the sessionless RT5a `tool_call` hook |
| #5343 | `cd8330be57` | `ported` | `crates/maho-tui/src/program_status.rs`, `crates/builtins/maho-ext-herdr/src/{state,reporter}.rs`, `crates/builtins/maho-ext-ask-user/src/tool.rs` (OSC 7501 program status reporting) |
| #5478 | `0d9aeb017c` | `n/a` | Split-session remote hand execution plane (S28 `packages/agent/src/execution/remote-*`, `modes/rpc/loopless-*`, `runs: "loop" | "tools"`); mhc has no split loopless hand plane (`docs/fork/senpi-agent-tools-delta.md`) |
| #5497 | `5e60c2a0a3` | `ported` | `crates/builtins/maho-ext-handoff-hint/src/index.rs` (`Shared` state survives the replacement session in `HandoffHint` and emits `HANDOFF_HINT_EVENT` with `HandoffHintAction::Accepted`) |
| #5498 | `5ab4b41341` | `ported` | `crates/builtins/maho-ext-websearch/src/websearch/{config,providers,search,tool,types}.rs` (public fan-out search, provider endpoints and result constraints; commit `91b72d7bb`) |
| #5504 | `3a84844a2e` | `ported` | `crates/builtins/maho-ext-compaction/src/{speculative,speculative_job,orchestration,rejected_recovery,log}.rs` (speculative compaction warm summary candidate reuse) |
| #5646 | `0ee68da72f` | `n/a` | Split-session loop-side `todotools` placement and hand mirror (S39); mhc runs `maho-ext-todotools` in process with the session loop |
| #5664 | `3f173a9949` | `n/a` | Split-session two-sided extension tool-event forwarding between remote hand and loop (S29 `two-sided-forwarding.ts`, `loopless-tool-handlers.ts`); mhc has no remote hand plane |
| #5677 | `7c1990ec17` | `ported` | `crates/builtins/maho-ext-imagegen/src/{auth,params,state,tool}.rs` (gateway candidate filtering and the 2.5 route fallback; commit `53f467922`) |
| #5740 | `d43ef3b1b0` | `n/a` | Split-session remote hand `ui-presentation.ts` port to the attached client (S30 `runtime-factory-loopless.ts`); mhc has no loopless hand runtime |
| #5817 | `e886871ee7` | `n/a` | Herdr pane metadata `tokens.app` brand token for the external Herdr multiplexer; Herdr is retired on the mhc fleet and `maho-ext-herdr/tests/reporter.rs` pins the three-method quit sequence |
| #5884 | `f7773b08b5` | `ported` | `crates/builtins/maho-ext-relocate-session/src/lib.rs` and `crates/maho-rpc/src/connection_handler.rs` (a relocation refusal reaches its tool before the session close teardown) |
| #5909 | `682ec80711` | `ported` | `crates/builtins/maho-ext-compaction/src/{speculative,speculative_summary}.rs` (the summarization budget accounts for the summary request tokens it actually sends) |
| #5948 | `38e81d7893` | `ported` | `crates/builtins/maho-ext-compaction/src/{speculative,speculative_summary}.rs` (the summary request token estimate comes from uncut usage and activated tools) |
| #5957 | `7d0fa0cdb3` | `n/a` | `webfetch` connected BrowserSkill local-socket reader (`browser-skill.ts`); `maho-ext-webfetch` is a standalone HTTP/HTML fetcher with no BrowserSkill daemon bridge (`docs/fork/senpi-agent-tools-delta.md`) |
| #5962 | `c76409d43a` | `n/a` | `webfetch` `signed-in-hosts.ts` routing through `browser-skill.ts`; excluded in `docs/fork/senpi-agent-tools-delta.md` because `maho-ext-webfetch` has no connected-browser read path |
| #5964 | `76bece884c` | `ported` | `crates/builtins/maho-ext-goal/src/{index,stop_pause,lifecycle_helpers}.rs` (`is_resume_of_stopped_goal` prompts when an interrupted active goal is reopened) |
| #6149 | `665e63b5cb` | `port` | `crates/maho-core/src/retained_message_safety.rs` and `crates/builtins/maho-ext-compaction/tests/retained_message_safety.rs` (`safe_configuration_update` accepts bounded `configurationUpdate` messages in the retained suffix) |
| #6151 | `2ceae959d4` | `ported` | `crates/builtins/maho-ext-terminal/src/{rpc_monitor_targets,rpc_monitor_source,rpc_monitor_watch,rpc_monitor_render,terminal_manifest_model}.rs` (remote `<device>/<session>` RPC monitor target support) |
| #6153 | `2f70890db8` | `n/a` | Herdr pane open-ended wake-source idle message split; Herdr is retired and `maho-ext-herdr` keeps its pinned four-event state machine (`Turn`, `Children`, `Monitors`, `Blocked`) |
| #6169 | `2edad22260` | `ported` | `crates/builtins/maho-ext-mcp/src/service.rs` (`wire_status_snapshot` derives wire status from the bound `McpService` instance instead of a shared cross-session cache) |
| #6198 | `35defecefa` | `n/a` | Heuristic escape-residue rejection in `ask_user_question` (`ask-user/escape-residue.ts`); depends on JS UTF-16 surrogate decoding (`String.fromCharCode`) and regex lookbehind |
| #6200 | `0a8ef4dced` | `n/a` | Bun dynamic extension importer event-loop yielding during `host-warm.ts` (`bun-extension-load-turns.ts`); mhc has no dynamic Bun extension importer |
| #6210 | `26ed3ff8bb` | `n/a` | Bun dynamic extension module-graph strong reference during an in-flight `import()` (`docs/fork/senpi-agent-tools-delta.md`: mhc extensions are statically compiled native crates) |
| #6212 | `f4f54aae66` | `n/a` | Terminal background task age telemetry events and opt-in auto-reap (`background-age.ts`, `background-auto-reap.ts`); mhc manages background PTY sessions through `maho-ext-terminal` `run_jobs.rs` and `orphan_reaper.rs` |
| #6260 | `5f3e39a14d` | `n/a` | Dynamic TS extension module SHA-256 cache (`extension-module-cache.ts`) and `scripts/bench-open-session.ts`; mhc has no dynamic TS extension loader |
| #6272 | `7769efce34` | `golden` | Golden fixture regeneration for eval-only `generate_image` (`.agents/skills/senpi-qa/scenarios/fused-equivalence/golden/**`); codemode eval-only reachability was ported in PR #35 (`a3c44520a`) |
| #6301 | `3705da617b` | `ported` | `crates/builtins/maho-ext-goal/src/{continuation_recovery,lifecycle_helpers,persistence,store,types}.rs` (recover a crashed goal-continuation stop on session start) |
| #6331 | `999646e6ce` | `n/a` | Changes the `show_html_page` tool description and output string for the OmO desktop app surface; deferred because the `SENPI_PIN` golden fixtures pin the existing tool description |
| #6366 | `b8fe5e0e91` | `ported` | `crates/builtins/maho-ext-mcp/src/auth/{oauth_provider,oauth_refresh,oauth,token_store}.rs` and `crates/builtins/maho-ext-mcp/src/{catalog_cache,service_register,service_tools_changed,startup_race}.rs` (OAuth grant continuity across token refresh) |
| #6388 | `f3768f7049` | `ported` | `crates/builtins/maho-ext-goal/src/{continuation,lifecycle_helpers,monitor_continuation,parked_wait,provider_retry,store}.rs` (a goal parked on a rate limit outlasts the limited accounts) |
| #6401 | `d82dae866f` | `n/a` | TS knip unused-export removal (`GOAL_LIMIT_WAIT_PARKED_EVENT` in `monitor-continuation.ts`) |
| #6403 | `f2e0d538db` | `ported` | `crates/builtins/maho-ext-goal/src/{continuation,index,lifecycle_helpers,monitor_continuation,reload_reengagement,parked_wait}.rs` (a goal waiting out a provider limit resumes when its session reopens) |
| #6676 | `629e90a3a7` | `ported` | `crates/builtins/maho-ext-permission-system/src/lib.rs` (`PendingRequest`'s `Drop` rejects an unanswered permission prompt when `runner.rs` drops the `tool_call` future on turn abort) |
| #6679 | `c0a3e77680` | `ported` | `crates/maho-ext-host/src/{runner,tool_hook_wait}.rs` and `crates/maho-core/src/agent_session.rs` (nested `execute_tool` call signal and turn-abort handling; PR #38 `f515c537b`) |
| #6681 | `742c01da4d` | `golden` | Monitor `<device>/<session>` prompt wording and golden fixture updates (`.agents/skills/senpi-qa/scenarios/fused-equivalence/golden/**`); handled with golden regeneration after the pin bump |
| #6720 | `6dc38d3b0c` | `n/a` | `pi.registerDeviceMachine` for routing `bash` over `OMO_TOOLS_VIA_DAEMON` in the TS extension loader; `maho-machine-ops` was ported in PR #39 (`a3e15575c`) and mhc runs native tools directly |
| #6728 | `848ae79456` | `n/a` | Follow-up to #6720 (`registerDeviceMachine` per-load staging guard in the TS `loader.ts`) |
| #6750 | `c16d064f13` | `port` | `crates/builtins/maho-ext-hooks/src/prompt_adapter.rs` (`build_user_prompt_hook_input` now carries `hook_event_name` = `UserPromptSubmit`, with a unit test) |
| #6757 | `690326b4a9` | `n/a` | Loopless remote hand `describe_tools` catalog (`modes/hand/tool-catalog.ts`, `modes/rpc/hand-tool-catalog.ts`); mhc has no loopless hand mode |
| #6764 | `29c7bae28a` | `ported` | Ported in round-1 PR #32 (`4b7f7e387`): `crates/builtins/maho-ext-compaction/src/{speculative,extension_wiring}.rs`, `crates/maho-core/src/compaction/compaction_execution.rs` |
| #6795 | `dc3bcf08f3` | `ported` | `crates/builtins/maho-ext-hooks/src/{stop_lifecycle,index}.rs` (`dispatch_stop` reads `StopTurnTracker::turn_key` at dispatch time instead of caching a stale turn key at turn end) |
| #6805 | `86fb89ddfa` | `ported` | `crates/builtins/maho-ext-hooks/src/stop_adapter.rs` (`StopTurnTracker::turn_key(leaf_id, session_id)` falls back to `session_id` when `leaf_id` is `None`) |
| #6898 | `20ea959d1d` | `ported` | `crates/builtins/maho-ext-goal/src/{index,stop_pause}.rs` (the goal stays paused and keeps its footer hold label when the stop hold flaps) |
| #6899 | `ac23875742` | `ported` | `crates/builtins/maho-ext-html-render/src/{images,tool}.rs` (`inline_local_images` magic-byte check, 10 MiB image and 25 MiB page limits, and data-URI inlining) |
| #6912 | `af6f06f455` | `ported` | `crates/builtins/maho-ext-html-render/src/{images,tool}.rs` (local image path extraction and size-limit checks) |
| #6926 | `4acf6f7341` | `n/a` | `PendingHandedPages` in-memory ledger for the loopless-hand and desktop-app transcript hydration race; mhc writes the prepared HTML file directly in `maho-ext-html-render/src/tool.rs` |
| #6930 | `ad99b0243d` | `n/a` | Follow-up to #6926 that makes `pending: PendingHandedPages` a required parameter of `createShowHtmlPageTool` |
| #6950 | `1dcc99c8c9` | `port` | `crates/builtins/maho-ext-compaction/src/context_pipeline.rs` (`build_compaction_context` skips `engaged_emergency_prune` when `append_only_transcript` is true, with a unit test) |
| #6982 | `5899433b27` | `n/a` | GPT-block todo handoff cue wording (`GPT_HANDOFF_CUES`) coupled to the #7114 prompt-preset rewrite; deferred with the golden prompt-preset regeneration |
| #6992 | `309ced6868` | `n/a` | Follow-up to #6982 interpolating `GPT_HANDOFF_LABELS` in the `all-closed` handoff cue |
| #7069 | `b3078f50e0` | `ported` | Ported in round-1 PR #38 (`f515c537b`): `crates/maho-ext-host/src/{tool_hook_wait,runner}.rs` (120s `tool_result` handler deadline and turn-abort cancellation) |
| #7073 | `589f951e18` | `ported` | Ported in round-1 PR #38 (`f515c537b`): `crates/maho-ext-host/src/runner.rs` (only a live turn's abort signal cancels the `tool_result` hook wait; `docs/fork/senpi-agent-tools-delta.md`) |
| #7114 | `b3bc2fc840` | `golden` | Prompt-preset wording and `token-budget-reminder.ts` removal across the `claude_opus_5_5` and `claude_fable_5_2` presets (`prompt-presets-native-baseline.json`); coupled to golden fixture regeneration after the pin bump |
| #7151 | `48d3377fe2` | `port` | `crates/builtins/maho-ext-permission-system/src/external_pattern.rs` (`swap_case` and `may_have_other_case` avoid `read_dir` when a path component's case spelling cannot differ, with unit tests) |
