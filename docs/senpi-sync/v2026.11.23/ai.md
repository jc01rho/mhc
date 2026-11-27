# senpi v2026.11.23 sync ledger — track `ai`

Range: `f01af13491..47333c26cd` (`v2026.11.21-2` to `v2026.11.23`).

Only one `packages/ai` source change falls in this range; the three bundled model catalog refreshes (#7204, #7379, #7179) are judged in [catalog.md](catalog.md).

## Summary

- `port`: 1

## Verdicts

| senpi PR | verdict | mhc location or reason |
| --- | --- | --- |
| #7392 fix(ai): send sk-ant-oat API keys as plain x-api-key instead of refusing them (senpi#7391; merge `36e330ca3`, commits `45510eb3fc`, `e30ba32d3c`, `0473557d92`) | port | `crates/maho-ai/src/api/anthropic_messages.rs` (the typed `ClaudeOAuthTokenRefusedError`, its `AnthropicStreamError` variant, `refuse_claude_oauth_token`, `assert_not_claude_oauth_token` and `is_claude_oauth_token_refused_message` are gone, so the value reaches `request_headers` as an ordinary `x-api-key`), tests `anthropic_audit_tests.rs` (`streams_send_a_claude_oauth_api_key_instead_of_refusing_it`, `a_claude_oauth_token_given_as_an_api_key_goes_out_as_a_plain_x_api_key`), `crates/maho-interactive/src/interactive_mode.rs` (the extra-usage warning no longer reads the API key: senpi's `maybeWarnAboutAnthropicSubscriptionAuth` checks the credential type only, and the mhc-only key branch would have mislabelled a plain key as subscription auth). `docs/providers.md` and the vitest files have no mhc counterpart. |
