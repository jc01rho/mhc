# senpi agent, tools and extension-host delta (SNP-04)

Source range: senpi `59d58cee5` (v2026.10.27-2, the mhc pin) to `6d4426a28` (v2026.11.19-5). Every row was read from the senpi merge
commit's diff and checked against the mhc source before it was classified. The master plan named owner paths from PR titles
alone (`crates/maho-agent/src/execution/`, `crates/maho-tools/src/sanitize.rs`); the code those PRs change lives elsewhere, and the
table says where.

| senpi PR | merge | what it changes | mhc | where / why |
|---|---|---|---|---|
| #6675 | 08009f799 | `interrupt { background: "stop" }` stops the interrupted turn's background bash jobs and their exit notice no longer wakes the session | ported | `maho-ext-api/src/run_background_jobs.rs`, `maho-core/src/agent_session.rs`, `maho-ext-host/src/runner.rs`, `maho-ext-terminal/src/{run_jobs,notify,extension,extension_state}.rs`, `maho-ext-terminal/src/tools/context.rs`, `maho-rpc/src/{rpc_types,pause_command_validation,session_control_actions_data,connection_handler}.rs`, `maho-interactive/src/session_control_session_commands.rs` |
| #6669 | 1f40baebd | tool-result text never reaches Claude Code as a slash command: every non-empty text block of a tool result carries `[tool output] ` | ported | `maho-ext-anthropic-subscription/src/{content_blocks,prompt_bridge,session_sync}.rs` |
| #7069 | b3078f50e | a tool hook handler cannot hold the tool loop: `tool_result` has a 120 s deadline, an abort of the turn ends both waits | ported | `maho-ext-host/src/tool_hook_wait.rs`, `maho-ext-host/src/runner.rs` |
| #6210 | 26ed3ff8b | an extension graph is held strongly while an import it serves is in flight | n/a | Bun module-graph garbage collection; extensions are statically registered native crates, there is no import graph to hold |
| #5878 | 59905a97f | a tool result JSON cannot carry settles as a tool error on the prepared executor | n/a | the prepared executor and its ledger do not exist in mhc, and a typed Rust tool result cannot hold a value JSON cannot carry (the BigInt case) |
| #5739 | 288f0cc32 | epoch floor fences a moved session's old owner on the hand | not ported | belongs to the remote hand plane (`remote-epoch`, `remote-hand-ledger`, `remote-link`); mhc has no hand, no ledger and no remote link |
| #6767 | 616f6b86b | late results of a hand's work reach the hosted loop through a durable wake outbox | not ported | same hand plane (`hand-wake-route`, `state-file-store`); nothing in mhc to attach it to |
| #6740 | 165fb8032 | bash, read, write and edit split into worker-safe cores plus local extras | n/a | a bundling refactor so the tools load in a Worker host (senpi#6739): exported names, options and results are unchanged, and a native build has no Node-builtin bundling constraint |
| #6887 | 5eba336a7 | `tool_search` lists matching eval-only tools; `ToolInfo.evalOnly` | not ported here | the field lives on `maho_ext_api::ToolInfo`, which snp-codemode (SNP-05) also needs for `reachable_tools`; both would edit every `ToolInfo` literal. Follow-up once SNP-05 lands the field: `maho-ext-tool-search` `search_eval_only` and the result text |
| #5962 | c76409d43 | webfetch reads user-listed hosts through the signed-in browser | not ported | the signed-in route is the BrowserSkill connected-browser read path, which `maho-ext-webfetch` does not have; the host matcher alone would be dead code |

Behavior notes for the ported rows:

- #6675 keeps senpi's contract: only a turn that is running records a job, `stop` runs after the turn settled, a stop that fails is named in
  `background.failed` with its reason, and a session on the tool child fails typed (`this stop does not reach it`). The terminal
  tracking sits in `TerminalNotifier` because every background exit already passes through it and the session shutdown resets it.
- #7069 differs from senpi in one way: a native handler future the host stops waiting on is dropped (cancelled at its await point) instead of
  left running detached, as `handler_budget` already does. The session's abort signal is used only while the agent is running a turn, since
  the Rust session keeps an aborted signal after an Esc until the next turn builds a new one.
- #6669 marks tool-result text blocks only; a user's own text is left as it is.
