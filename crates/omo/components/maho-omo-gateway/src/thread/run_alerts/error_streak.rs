use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type ClockFn = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunErrorClass {
    RateLimit,
    Auth,
    Overloaded,
    Network,
    Unknown,
}

impl RunErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RateLimit => "rate_limit",
            Self::Auth => "auth",
            Self::Overloaded => "overloaded",
            Self::Network => "network",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    Error {
        error_class: RunErrorClass,
        provider: Option<String>,
        model: Option<String>,
    },
    Aborted,
    Completed,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentEndFacts {
    pub aborted: Option<bool>,
    pub messages: Vec<Value>,
}

pub fn agent_end_facts_from_messages(
    aborted: Option<bool>,
    messages: &[maho_ext_api::AgentMessage],
) -> AgentEndFacts {
    let serialized = messages
        .iter()
        .filter_map(|msg| serde_json::to_value(msg).ok())
        .collect();
    AgentEndFacts {
        aborted,
        messages: serialized,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreakFacts {
    pub session_id: String,
    pub count: u64,
    pub classes: BTreeMap<RunErrorClass, u64>,
    pub aborted: u64,
    pub first_at: i64,
    pub last_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ErrorStreakEvent {
    ErrorStreak {
        #[serde(flatten)]
        facts: StreakFacts,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        repeat: bool,
    },
    ErrorStreakRecovered {
        #[serde(flatten)]
        facts: StreakFacts,
        recovered_at: i64,
    },
}

impl ErrorStreakEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::ErrorStreak { .. } => "error_streak",
            Self::ErrorStreakRecovered { .. } => "error_streak_recovered",
        }
    }

    pub fn facts(&self) -> &StreakFacts {
        match self {
            Self::ErrorStreak { facts, .. } | Self::ErrorStreakRecovered { facts, .. } => facts,
        }
    }
}

fn error_class_patterns() -> &'static [(RunErrorClass, Regex)] {
    static PATTERNS: OnceLock<Vec<(RunErrorClass, Regex)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let specs = [
            (
                RunErrorClass::RateLimit,
                r"(?i)rate.?limit|\b429\b|too many requests|usage.?limit|quota",
            ),
            (
                RunErrorClass::Auth,
                r"(?i)\b40[13]\b|unauthori[sz]ed|forbidden|authenticat|api.?key|credential|permission",
            ),
            (
                RunErrorClass::Overloaded,
                r"(?i)overloaded|\b5(?:29|03)\b|unavailable|capacity",
            ),
            (
                RunErrorClass::Network,
                r"(?i)fetch failed|network|socket hang up|timed? ?out|\bE(?:CONNRESET|CONNREFUSED|TIMEDOUT|NOTFOUND|AI_AGAIN|PIPE)\b",
            ),
        ];
        specs
            .into_iter()
            .filter_map(|(class, pat)| Regex::new(pat).ok().map(|re| (class, re)))
            .collect()
    })
}

pub fn classify_run_error(error_message: Option<&str>) -> RunErrorClass {
    let Some(text) = error_message else {
        return RunErrorClass::Unknown;
    };
    error_class_patterns()
        .iter()
        .find(|(_, pattern)| pattern.is_match(text))
        .map_or(RunErrorClass::Unknown, |(class, _)| *class)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionOutcome {
    Completed,
    Failed,
    Cancelled,
}

pub fn completion_outcome(event: &AgentEndFacts) -> CompletionOutcome {
    if event.aborted == Some(true) {
        return CompletionOutcome::Cancelled;
    }
    let last_assistant = event
        .messages
        .iter()
        .rev()
        .find(|msg| msg.get("role").and_then(Value::as_str) == Some("assistant"));
    let stop_reason = last_assistant.and_then(|msg| {
        msg.get("stopReason")
            .or_else(|| msg.get("stop_reason"))
            .and_then(Value::as_str)
    });
    match stop_reason {
        Some("aborted") => CompletionOutcome::Cancelled,
        Some("error") => CompletionOutcome::Failed,
        _ => CompletionOutcome::Completed,
    }
}

pub fn run_end_of(event: &AgentEndFacts) -> RunEnd {
    match completion_outcome(event) {
        CompletionOutcome::Cancelled => RunEnd::Aborted,
        CompletionOutcome::Completed => RunEnd::Completed,
        CompletionOutcome::Failed => {
            let last_assistant = event
                .messages
                .iter()
                .rev()
                .find(|msg| msg.get("role").and_then(Value::as_str) == Some("assistant"));
            let error_msg = last_assistant.and_then(|msg| {
                msg.get("errorMessage")
                    .or_else(|| msg.get("error_message"))
                    .and_then(Value::as_str)
            });
            let provider = last_assistant
                .and_then(|msg| msg.get("provider"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let model = last_assistant
                .and_then(|msg| msg.get("model"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            RunEnd::Error {
                error_class: classify_run_error(error_msg),
                provider,
                model,
            }
        }
    }
}

struct Streak {
    count: u64,
    classes: BTreeMap<RunErrorClass, u64>,
    aborted: u64,
    first_at: i64,
    last_at: i64,
    provider: Option<String>,
    model: Option<String>,
    alerted_at: Option<i64>,
}

pub struct ErrorStreakDetectorOptions {
    pub threshold: u64,
    pub realert_ms: i64,
    pub now: ClockFn,
}

pub struct ErrorStreakDetector {
    options: ErrorStreakDetectorOptions,
    streaks: Mutex<BTreeMap<String, Streak>>,
}

pub fn create_error_streak_detector(options: ErrorStreakDetectorOptions) -> ErrorStreakDetector {
    ErrorStreakDetector {
        options,
        streaks: Mutex::new(BTreeMap::new()),
    }
}

impl ErrorStreakDetector {
    pub fn forget(&self, session_id: &str) {
        self.streaks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
    }

    pub fn observe(&self, session_id: &str, end: &RunEnd) -> Option<ErrorStreakEvent> {
        let mut streaks = self
            .streaks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match end {
            RunEnd::Aborted => {
                if let Some(streak) = streaks.get_mut(session_id) {
                    streak.aborted += 1;
                }
                None
            }
            RunEnd::Completed => {
                let streak = streaks.remove(session_id)?;
                streak.alerted_at?;
                Some(ErrorStreakEvent::ErrorStreakRecovered {
                    facts: Self::facts(session_id, &streak),
                    recovered_at: (self.options.now)(),
                })
            }
            RunEnd::Error {
                error_class,
                provider,
                model,
            } => {
                let at = (self.options.now)();
                let current = streaks
                    .entry(session_id.to_owned())
                    .or_insert_with(|| Streak {
                        count: 0,
                        classes: BTreeMap::new(),
                        aborted: 0,
                        first_at: at,
                        last_at: at,
                        provider: None,
                        model: None,
                        alerted_at: None,
                    });
                current.count += 1;
                *current.classes.entry(*error_class).or_insert(0) += 1;
                current.last_at = at;
                if provider.is_some() {
                    current.provider.clone_from(provider);
                }
                if model.is_some() {
                    current.model.clone_from(model);
                }
                if current.count < self.options.threshold {
                    return None;
                }
                let repeat = current.alerted_at.is_some();
                if let Some(alerted_at) = current.alerted_at
                    && (self.options.realert_ms <= 0 || at - alerted_at < self.options.realert_ms)
                {
                    return None;
                }
                current.alerted_at = Some(at);
                Some(ErrorStreakEvent::ErrorStreak {
                    facts: Self::facts(session_id, current),
                    provider: current.provider.clone(),
                    model: current.model.clone(),
                    repeat,
                })
            }
        }
    }

    fn facts(session_id: &str, streak: &Streak) -> StreakFacts {
        StreakFacts {
            session_id: session_id.to_owned(),
            count: streak.count,
            classes: streak.classes.clone(),
            aborted: streak.aborted,
            first_at: streak.first_at,
            last_at: streak.last_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use serde_json::json;

    use super::*;
    use crate::thread::wake::session_target::parse_iso_timestamp_ms;

    const MINUTE: i64 = 60_000;
    const SEAT: &str = "dur-seat";

    struct VirtualClock {
        at: Arc<AtomicI64>,
    }

    impl VirtualClock {
        fn new() -> Self {
            let start =
                parse_iso_timestamp_ms("2026-11-19T22:00:00.000Z").expect("valid start timestamp");
            Self {
                at: Arc::new(AtomicI64::new(start)),
            }
        }

        fn now(&self) -> i64 {
            self.at.load(Ordering::SeqCst)
        }

        fn advance(&self, ms: i64) {
            self.at.fetch_add(ms, Ordering::SeqCst);
        }

        fn clock_fn(&self) -> ClockFn {
            let at = self.at.clone();
            Arc::new(move || at.load(Ordering::SeqCst))
        }
    }

    fn rate_limited() -> RunEnd {
        RunEnd::Error {
            error_class: RunErrorClass::RateLimit,
            provider: Some("anthropic".into()),
            model: Some("claude-x".into()),
        }
    }

    #[test]
    fn given_9_consecutive_error_runs_when_the_10th_ends_with_an_error_then_exactly_one_alert_names_the_count_class_model_and_first_last_time()
     {
        let clock = VirtualClock::new();
        let detector = create_error_streak_detector(ErrorStreakDetectorOptions {
            threshold: 10,
            realert_ms: 60 * MINUTE,
            now: clock.clock_fn(),
        });
        let first_at = clock.now();
        let err = rate_limited();
        for _ in 1..=9 {
            assert_eq!(detector.observe(SEAT, &err), None);
            clock.advance(15_000);
        }
        let alert = detector.observe(SEAT, &err);
        let mut classes = BTreeMap::new();
        classes.insert(RunErrorClass::RateLimit, 10);
        assert_eq!(
            alert,
            Some(ErrorStreakEvent::ErrorStreak {
                facts: StreakFacts {
                    session_id: SEAT.into(),
                    count: 10,
                    classes,
                    aborted: 0,
                    first_at,
                    last_at: first_at + 9 * 15_000,
                },
                provider: Some("anthropic".into()),
                model: Some("claude-x".into()),
                repeat: false,
            })
        );
    }

    #[test]
    fn given_an_alerted_streak_when_25_more_errors_end_inside_the_realert_interval_then_nothing_more_is_sent_and_first_error_past_interval_sends_still_failing()
     {
        let clock = VirtualClock::new();
        let detector = create_error_streak_detector(ErrorStreakDetectorOptions {
            threshold: 10,
            realert_ms: 60 * MINUTE,
            now: clock.clock_fn(),
        });
        let err = rate_limited();
        for _ in 1..=10 {
            detector.observe(SEAT, &err);
        }
        for _ in 1..=25 {
            clock.advance(MINUTE);
            assert_eq!(detector.observe(SEAT, &err), None);
        }
        clock.advance(35 * MINUTE);
        let again = detector.observe(SEAT, &err).expect("still-failing alert");
        assert_eq!(again.event_name(), "error_streak");
        assert_eq!(again.facts().count, 36);
        assert!(matches!(
            again,
            ErrorStreakEvent::ErrorStreak { repeat: true, .. }
        ));
    }

    #[test]
    fn given_an_alerted_streak_when_a_run_ends_normally_then_one_recovered_notice_closes_it_and_next_9_errors_send_nothing()
     {
        let clock = VirtualClock::new();
        let detector = create_error_streak_detector(ErrorStreakDetectorOptions {
            threshold: 10,
            realert_ms: 60 * MINUTE,
            now: clock.clock_fn(),
        });
        let first_at = clock.now();
        let err = rate_limited();
        for _ in 1..=12 {
            detector.observe(SEAT, &err);
            clock.advance(1_000);
        }
        let last_at = clock.now() - 1_000;
        let recovered = detector.observe(SEAT, &RunEnd::Completed);
        for _ in 0..9 {
            assert_eq!(detector.observe(SEAT, &err), None);
        }
        let mut classes = BTreeMap::new();
        classes.insert(RunErrorClass::RateLimit, 12);
        assert_eq!(
            recovered,
            Some(ErrorStreakEvent::ErrorStreakRecovered {
                facts: StreakFacts {
                    session_id: SEAT.into(),
                    count: 12,
                    classes,
                    aborted: 0,
                    first_at,
                    last_at,
                },
                recovered_at: clock.now(),
            })
        );
    }

    #[test]
    fn given_a_streak_shorter_than_threshold_when_a_run_ends_normally_then_no_recovered_notice_is_sent()
     {
        let clock = VirtualClock::new();
        let detector = create_error_streak_detector(ErrorStreakDetectorOptions {
            threshold: 10,
            realert_ms: 60 * MINUTE,
            now: clock.clock_fn(),
        });
        let err = rate_limited();
        for _ in 1..=9 {
            detector.observe(SEAT, &err);
        }
        assert_eq!(detector.observe(SEAT, &RunEnd::Completed), None);
    }

    #[test]
    fn given_errors_of_several_classes_when_streak_reaches_threshold_then_alert_counts_each_class()
    {
        let clock = VirtualClock::new();
        let detector = create_error_streak_detector(ErrorStreakDetectorOptions {
            threshold: 10,
            realert_ms: 60 * MINUTE,
            now: clock.clock_fn(),
        });
        let err = rate_limited();
        for _ in 1..=6 {
            detector.observe(SEAT, &err);
        }
        for _ in 1..=3 {
            detector.observe(
                SEAT,
                &RunEnd::Error {
                    error_class: RunErrorClass::Auth,
                    provider: None,
                    model: None,
                },
            );
        }
        let alert = detector
            .observe(
                SEAT,
                &RunEnd::Error {
                    error_class: RunErrorClass::Network,
                    provider: None,
                    model: None,
                },
            )
            .expect("alert");
        assert_eq!(alert.facts().count, 10);
        assert_eq!(
            alert.facts().classes.get(&RunErrorClass::RateLimit),
            Some(&6)
        );
        assert_eq!(alert.facts().classes.get(&RunErrorClass::Auth), Some(&3));
        assert_eq!(alert.facts().classes.get(&RunErrorClass::Network), Some(&1));
    }

    #[test]
    fn given_aborted_runs_when_they_end_alone_or_inside_a_streak_then_they_never_alert_never_end_a_streak_and_are_counted_beside_it()
     {
        let clock = VirtualClock::new();
        let detector = create_error_streak_detector(ErrorStreakDetectorOptions {
            threshold: 10,
            realert_ms: 60 * MINUTE,
            now: clock.clock_fn(),
        });
        for _ in 0..86 {
            assert_eq!(detector.observe("dur-chat", &RunEnd::Aborted), None);
        }
        let err = rate_limited();
        for _ in 1..=5 {
            detector.observe(SEAT, &err);
        }
        assert_eq!(detector.observe(SEAT, &RunEnd::Aborted), None);
        for _ in 1..=4 {
            detector.observe(SEAT, &err);
        }
        let alert = detector.observe(SEAT, &err).expect("alert");
        assert_eq!(alert.facts().count, 10);
        assert_eq!(alert.facts().aborted, 1);
    }

    #[test]
    fn given_a_session_forgotten_mid_streak_when_it_fails_again_then_it_starts_a_new_streak_and_holds_nothing_before_that()
     {
        let clock = VirtualClock::new();
        let detector = create_error_streak_detector(ErrorStreakDetectorOptions {
            threshold: 10,
            realert_ms: 60 * MINUTE,
            now: clock.clock_fn(),
        });
        let err = rate_limited();
        for _ in 1..=9 {
            detector.observe(SEAT, &err);
        }
        detector.forget(SEAT);
        assert_eq!(detector.observe(SEAT, &RunEnd::Completed), None);
        for _ in 0..9 {
            assert_eq!(detector.observe(SEAT, &err), None);
        }
        let alert = detector.observe(SEAT, &err).expect("alert");
        assert_eq!(alert.facts().count, 10);
    }

    #[test]
    fn given_two_sessions_when_only_one_keeps_failing_then_only_that_one_alerts() {
        let clock = VirtualClock::new();
        let detector = create_error_streak_detector(ErrorStreakDetectorOptions {
            threshold: 10,
            realert_ms: 60 * MINUTE,
            now: clock.clock_fn(),
        });
        let err = rate_limited();
        let mut alerted = Vec::new();
        for _ in 1..=10 {
            if let Some(ev) = detector.observe(SEAT, &err) {
                alerted.push(ev.facts().session_id.clone());
            }
            if let Some(ev) = detector.observe("dur-other", &RunEnd::Completed) {
                alerted.push(ev.facts().session_id.clone());
            }
        }
        assert_eq!(alerted, vec![SEAT.to_owned()]);
    }

    #[test]
    fn given_realert_0_when_the_streak_goes_on_for_hours_then_only_the_first_alert_is_sent() {
        let clock = VirtualClock::new();
        let detector = create_error_streak_detector(ErrorStreakDetectorOptions {
            threshold: 10,
            realert_ms: 0,
            now: clock.clock_fn(),
        });
        let err = rate_limited();
        let mut count = 0;
        for _ in 1..=500 {
            if detector.observe(SEAT, &err).is_some() {
                count += 1;
            }
            clock.advance(MINUTE);
        }
        assert_eq!(count, 1);
    }

    #[test]
    fn given_the_measured_2026_11_19_night_2769_rate_limit_errors_15s_apart_then_normal_stop_when_replayed_then_hourly_alerts_and_recovery_fire()
     {
        let clock = VirtualClock::new();
        let detector = create_error_streak_detector(ErrorStreakDetectorOptions {
            threshold: 10,
            realert_ms: 60 * MINUTE,
            now: clock.clock_fn(),
        });
        let err = rate_limited();
        let mut sent = Vec::new();
        for _ in 1..=2_769 {
            if let Some(ev) = detector.observe(SEAT, &err) {
                sent.push(format!("{} {}", ev.event_name(), ev.facts().count));
            }
            clock.advance(15_000);
        }
        if let Some(ev) = detector.observe(SEAT, &RunEnd::Completed) {
            sent.push(format!("{} {}", ev.event_name(), ev.facts().count));
        }
        let mut expected = vec!["error_streak 10".to_owned()];
        for hour in 1..=11 {
            expected.push(format!("error_streak {}", 10 + hour * 240));
        }
        expected.push("error_streak_recovered 2769".to_owned());
        assert_eq!(sent, expected);
    }

    #[test]
    fn given_error_messages_when_classified_then_classes_match() {
        let cases = [
            (
                Some("rate_limit_error: This request would exceed your account's rate limit"),
                RunErrorClass::RateLimit,
            ),
            (Some("429 Too Many Requests"), RunErrorClass::RateLimit),
            (
                Some("You have hit your usage limit"),
                RunErrorClass::RateLimit,
            ),
            (Some("401 Unauthorized"), RunErrorClass::Auth),
            (
                Some("authentication_error: invalid x-api-key"),
                RunErrorClass::Auth,
            ),
            (
                Some("overloaded_error: Overloaded"),
                RunErrorClass::Overloaded,
            ),
            (Some("529 service overloaded"), RunErrorClass::Overloaded),
            (Some("503 Service Unavailable"), RunErrorClass::Overloaded),
            (Some("fetch failed"), RunErrorClass::Network),
            (Some("read ECONNRESET"), RunErrorClass::Network),
            (Some("the model said something odd"), RunErrorClass::Unknown),
            (None, RunErrorClass::Unknown),
        ];
        for (msg, expected) in cases {
            assert_eq!(classify_run_error(msg), expected, "msg: {msg:?}");
        }
    }

    #[test]
    fn given_an_agent_end_whose_last_assistant_message_stopped_on_an_error_when_outcome_is_read_then_message_text_is_not_kept()
     {
        let end = run_end_of(&AgentEndFacts {
            aborted: None,
            messages: vec![
                json!({ "role": "user", "content": "PROMPT-SENTINEL" }),
                json!({
                    "role": "assistant",
                    "stopReason": "error",
                    "errorMessage": "rate_limit_error: OUTPUT-SENTINEL",
                    "provider": "anthropic",
                    "model": "claude-x",
                }),
            ],
        });
        assert_eq!(
            end,
            RunEnd::Error {
                error_class: RunErrorClass::RateLimit,
                provider: Some("anthropic".into()),
                model: Some("claude-x".into()),
            }
        );
    }

    #[test]
    fn given_an_aborted_run_and_a_normal_run_when_outcomes_are_read_then_they_are_aborted_and_completed()
     {
        assert_eq!(
            run_end_of(&AgentEndFacts {
                aborted: Some(true),
                messages: vec![json!({ "role": "assistant", "stopReason": "error" })],
            }),
            RunEnd::Aborted
        );
        assert_eq!(
            run_end_of(&AgentEndFacts {
                aborted: None,
                messages: vec![json!({ "role": "assistant", "stopReason": "stop" })],
            }),
            RunEnd::Completed
        );
    }
}
