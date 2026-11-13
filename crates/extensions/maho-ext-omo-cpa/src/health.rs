//! Port of omo-cpa `src/health.ts`. Circuit breaker for the CPA endpoint.
//!
//! Attribution caveat: omo's `after_provider_response` event carries only a status and headers,
//! with no provider identity. The extension therefore records the provider of the most recent
//! `before_provider_request` and attributes the next response to it.

use crate::types::{HealthSnapshot, HealthState};
use std::collections::BTreeMap;

pub const DEFAULT_DOWN_THRESHOLD: u32 = 3;

#[derive(Clone, Debug)]
pub struct HealthTracker {
    down_threshold: u32,
    consecutive_failures: u32,
    last_status: Option<u16>,
    retry_at: Option<i64>,
    updated_at: i64,
    last_detail: String,
    state: HealthState,
}

impl Default for HealthTracker {
    fn default() -> Self {
        Self::new(DEFAULT_DOWN_THRESHOLD)
    }
}

impl HealthTracker {
    pub fn new(down_threshold: u32) -> Self {
        Self {
            down_threshold,
            consecutive_failures: 0,
            last_status: None,
            retry_at: None,
            updated_at: 0,
            last_detail: "아직 요청 없음".to_owned(),
            state: HealthState::Unknown,
        }
    }

    /// Record a completed provider response attributed to CPA.
    pub fn record(
        &mut self,
        status: u16,
        headers: &BTreeMap<String, String>,
        now: i64,
    ) -> HealthSnapshot {
        self.last_status = Some(status);
        self.updated_at = now;

        if status == 429 {
            self.consecutive_failures += 1;
            self.retry_at = parse_retry_after(headers, now);
            self.state = HealthState::RateLimited;
            self.last_detail = match self.retry_at {
                Some(retry_at) => {
                    format!("요청 제한 (HTTP 429) · {} 재시도", retry_clock(retry_at))
                }
                None => "요청 제한 (HTTP 429)".to_owned(),
            };
            return self.snapshot();
        }

        if status >= 500 {
            self.consecutive_failures += 1;
            self.state = if self.consecutive_failures >= self.down_threshold {
                HealthState::Down
            } else {
                HealthState::Degraded
            };
            self.last_detail = format!(
                "서버 오류 (HTTP {status}) · 연속 {}회",
                self.consecutive_failures
            );
            return self.snapshot();
        }

        if status >= 400 {
            // A 4xx is usually the request's fault, not the server's: do not trip the breaker.
            if self.consecutive_failures == 0 {
                self.state = HealthState::Ok;
            }
            self.last_detail = format!("요청 거부 (HTTP {status}) — 서버 상태와 무관할 수 있음");
            return self.snapshot();
        }

        self.consecutive_failures = 0;
        self.retry_at = None;
        self.state = HealthState::Ok;
        self.last_detail = format!("정상 (HTTP {status})");
        self.snapshot()
    }

    /// Record a transport-level failure (no HTTP status at all).
    pub fn record_failure(&mut self, message: &str, now: i64) -> HealthSnapshot {
        self.consecutive_failures += 1;
        self.last_status = None;
        self.updated_at = now;
        self.state = if self.consecutive_failures >= self.down_threshold {
            HealthState::Down
        } else {
            HealthState::Degraded
        };
        self.last_detail = format!(
            "연결 실패 · 연속 {}회 · {message}",
            self.consecutive_failures
        );
        self.snapshot()
    }

    pub fn snapshot(&self) -> HealthSnapshot {
        HealthSnapshot {
            state: self.state,
            detail: self.last_detail.clone(),
            consecutive_failures: self.consecutive_failures,
            last_status: self.last_status,
            retry_at: self.retry_at,
            updated_at: self.updated_at,
        }
    }
}

/// Parse Retry-After (seconds or HTTP date). `None` when absent or unparseable.
pub fn parse_retry_after(headers: &BTreeMap<String, String>, now: i64) -> Option<i64> {
    let raw = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
        .map(|(_, value)| value)?;
    if let Ok(seconds) = raw.trim().parse::<f64>()
        && seconds.is_finite()
        && seconds >= 0.0
    {
        return Some(now + (seconds * 1000.0) as i64);
    }
    chrono::DateTime::parse_from_rfc2822(raw.trim())
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(raw.trim()))
        .ok()
        .map(|instant| instant.timestamp_millis())
}

/// HH:MM in local time, matching `new Date(retryAt).toTimeString().slice(0, 5)`.
fn retry_clock(at: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at)
        .map(|instant| {
            instant
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "--:--".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn the_breaker_trips_only_after_the_threshold() {
        let mut tracker = HealthTracker::new(3);
        assert_eq!(
            tracker.record(500, &BTreeMap::new(), 1).state,
            HealthState::Degraded
        );
        assert_eq!(
            tracker.record(502, &BTreeMap::new(), 2).state,
            HealthState::Degraded
        );
        let down = tracker.record(503, &BTreeMap::new(), 3);
        assert_eq!(down.state, HealthState::Down);
        assert_eq!(down.consecutive_failures, 3);
        assert!(down.detail.contains("연속 3회"));
    }

    #[test]
    fn success_resets_the_counter_and_the_breaker() {
        let mut tracker = HealthTracker::new(2);
        tracker.record(500, &BTreeMap::new(), 1);
        let ok = tracker.record(200, &BTreeMap::new(), 2);
        assert_eq!(ok.state, HealthState::Ok);
        assert_eq!(ok.consecutive_failures, 0);
        assert_eq!(ok.last_status, Some(200));
    }

    #[test]
    fn a_4xx_does_not_reset_an_already_tripped_counter() {
        let mut tracker = HealthTracker::new(1);
        tracker.record(500, &BTreeMap::new(), 1);
        let rejected = tracker.record(404, &BTreeMap::new(), 2);
        assert_eq!(rejected.state, HealthState::Down);
        assert!(rejected.detail.contains("요청 거부"));
    }

    #[test]
    fn a_4xx_on_a_healthy_tracker_reports_ok() {
        let mut tracker = HealthTracker::new(3);
        assert_eq!(
            tracker.record(400, &BTreeMap::new(), 1).state,
            HealthState::Ok
        );
    }

    #[test]
    fn rate_limiting_carries_the_retry_instant() {
        let mut tracker = HealthTracker::new(3);
        let snapshot = tracker.record(429, &headers(&[("Retry-After", "30")]), 1_000);
        assert_eq!(snapshot.state, HealthState::RateLimited);
        assert_eq!(snapshot.retry_at, Some(31_000));
        assert!(snapshot.detail.contains("재시도"));
    }

    #[test]
    fn retry_after_accepts_seconds_and_http_dates_only() {
        assert_eq!(
            parse_retry_after(&headers(&[("retry-after", "2.5")]), 0),
            Some(2500)
        );
        assert_eq!(
            parse_retry_after(&headers(&[("Retry-After", "later")]), 0),
            None
        );
        assert_eq!(parse_retry_after(&BTreeMap::new(), 0), None);
        let date = parse_retry_after(
            &headers(&[("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT")]),
            0,
        );
        assert_eq!(date, Some(1_445_412_480_000));
    }

    #[test]
    fn transport_failures_are_degraded_then_down() {
        let mut tracker = HealthTracker::new(2);
        let first = tracker.record_failure("connect refused", 5);
        assert_eq!(first.state, HealthState::Degraded);
        assert_eq!(first.last_status, None);
        let second = tracker.record_failure("connect refused", 6);
        assert_eq!(second.state, HealthState::Down);
        assert!(second.detail.contains("connect refused"));
    }
}
