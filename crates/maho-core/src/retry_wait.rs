use maho_agent::types::AgentEvent;
use maho_ext_api::AgentSessionEvent;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

const ERROR_CHARS: usize = 200;

static RATE_LIMITED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)rate.?limit|usage.?limit|quota|too many requests|resource.?exhausted|(?:^|\b(?:HTTP/1\.[01]|HTTP|status(?: code)?|error|code)\s+)429\b",
    )
    .expect("valid rate limit regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryWaitKind {
    RateLimit,
    Retry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryWait {
    pub kind: RetryWaitKind,
    pub attempt: u32,
    pub max_attempts: u32,
    pub delay_ms: u64,
    pub until: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn next_retry_wait(
    previous: Option<RetryWait>,
    event: &AgentSessionEvent,
    now_ms: i64,
) -> Option<RetryWait> {
    match event {
        AgentSessionEvent::AutoRetryStart {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => {
            let error: String = error_message.chars().take(ERROR_CHARS).collect();
            let kind = if RATE_LIMITED.is_match(error_message) {
                RetryWaitKind::RateLimit
            } else {
                RetryWaitKind::Retry
            };
            let until_ms = now_ms.saturating_add(i64::try_from(*delay_ms).unwrap_or(i64::MAX));
            let until = chrono::DateTime::from_timestamp_millis(until_ms)
                .unwrap_or_default()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            Some(RetryWait {
                kind,
                attempt: *attempt,
                max_attempts: *max_attempts,
                delay_ms: *delay_ms,
                until,
                error: (!error.is_empty()).then_some(error),
            })
        }
        AgentSessionEvent::Agent(AgentEvent::AgentStart)
        | AgentSessionEvent::AutoRetryEnd { .. } => None,
        _ => previous,
    }
}

pub fn redacted_retry_wait(wait: &RetryWait) -> RetryWait {
    RetryWait {
        kind: wait.kind,
        attempt: wait.attempt,
        max_attempts: wait.max_attempts,
        delay_ms: wait.delay_ms,
        until: wait.until.clone(),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_rate_limit_and_transient_retry_and_clears_on_start_or_end() {
        let start = AgentSessionEvent::AutoRetryStart {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 1_000,
            error_message: "429 rate_limit_exceeded: slow down".into(),
        };
        let wait = next_retry_wait(None, &start, 1_700_000_000_000).expect("retry wait");
        assert_eq!(wait.kind, RetryWaitKind::RateLimit);
        assert_eq!(wait.attempt, 1);
        assert_eq!(wait.max_attempts, 3);
        assert_eq!(wait.delay_ms, 1_000);
        assert_eq!(
            wait.error.as_deref(),
            Some("429 rate_limit_exceeded: slow down")
        );
        let redacted = redacted_retry_wait(&wait);
        assert!(redacted.error.is_none());
        assert_eq!(redacted.kind, RetryWaitKind::RateLimit);

        let cleared_on_start = next_retry_wait(
            Some(wait.clone()),
            &AgentSessionEvent::Agent(AgentEvent::AgentStart),
            1_700_000_001_000,
        );
        assert!(cleared_on_start.is_none());

        let transient = AgentSessionEvent::AutoRetryStart {
            attempt: 2,
            max_attempts: 3,
            delay_ms: 500,
            error_message: "503 service unavailable".into(),
        };
        let wait2 = next_retry_wait(None, &transient, 1_700_000_002_000).expect("transient wait");
        assert_eq!(wait2.kind, RetryWaitKind::Retry);

        let cleared_on_end = next_retry_wait(
            Some(wait2),
            &AgentSessionEvent::AutoRetryEnd {
                success: true,
                attempt: 2,
                final_error: None,
            },
            1_700_000_002_500,
        );
        assert!(cleared_on_end.is_none());
    }
}
