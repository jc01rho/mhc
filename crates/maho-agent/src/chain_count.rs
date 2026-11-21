//! The hop count of a thread chain (senpi `packages/agent/src/execution/chain-count.ts`, brain/hand split, decision
//! K8): session A's thread tool prompts session B, B's prompts A again, and so on. A -> B -> A is allowed. Each call
//! carries the count of the hop it makes; a session started by a thread call is at that call's count, and its own
//! thread calls carry one more. The only limit is a count the user sets; unset means unlimited. There is no cost
//! limit, no hidden default and no refusal for coming back to an origin.
use serde_json::Value;

/// The refusal reason of a thread call past the user's count limit.
pub const CHAIN_COUNT_LIMIT: &str = "chain_count_limit";

/// A user-started session is at count 0; its first thread call is hop 1.
pub const CHAIN_ORIGIN: u64 = 0;

/// JavaScript's `Number.MAX_SAFE_INTEGER`: the largest count a JSON number carries exactly.
const MAX_SAFE_COUNT: u64 = 9_007_199_254_740_991;

/// A thread call past the user's count limit (senpi `ChainCountLimitError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "This thread call would be hop {count} of its chain; your thread chain count limit is {limit}."
)]
pub struct ChainCountLimitError {
    pub count: u64,
    pub limit: u64,
}

impl ChainCountLimitError {
    /// The machine-readable refusal reason.
    pub const REASON: &'static str = CHAIN_COUNT_LIMIT;
}

/// A session count that is no non-negative safe integer (senpi `nextChainCount`'s `TypeError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a chain count: {0}")]
pub struct NotAChainCount(pub String);

/// A count a thread call carried that no hop can have made (senpi `decodeChainCount`'s `TypeError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed chain count: {0}")]
pub struct MalformedChainCount(pub String);

/// senpi `isCount`: a non-negative safe integer, as a JSON number spells it (`3` and `3.0` are the same count).
fn count_of(value: &Value) -> Option<u64> {
    let count = value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|float| float.fract() == 0.0 && (0.0..=MAX_SAFE_COUNT as f64).contains(float))
            .map(|float| float as u64)
    })?;
    (count <= MAX_SAFE_COUNT).then_some(count)
}

/// The count a thread call sent from a session at `session_count` carries.
pub fn next_chain_count(session_count: u64) -> Result<u64, NotAChainCount> {
    if session_count > MAX_SAFE_COUNT {
        return Err(NotAChainCount(session_count.to_string()));
    }
    Ok(session_count + 1)
}

/// Parses the count a thread call carries; absent means the call starts a chain (hop 1 from a user-started session).
pub fn decode_chain_count(value: Option<&Value>) -> Result<u64, MalformedChainCount> {
    let Some(value) = value else {
        return Ok(CHAIN_ORIGIN + 1);
    };
    match count_of(value) {
        Some(count) if count != CHAIN_ORIGIN => Ok(count),
        _ => Err(MalformedChainCount(value.to_string())),
    }
}

/// Admits a thread call that makes its chain `count` hops long. `read_limit` is read at each admission, never cached
/// (on the CLI: the user's `threads.chainCountLimit` setting); `None` admits every call at any length.
pub fn admit_chain_call(
    count: u64,
    read_limit: impl FnOnce() -> Option<u64>,
) -> Result<u64, ChainCountLimitError> {
    match read_limit() {
        Some(limit) if count > limit => Err(ChainCountLimitError { count, limit }),
        _ => Ok(count),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_session_count_is_one_more_for_its_own_thread_calls() {
        assert_eq!(next_chain_count(CHAIN_ORIGIN), Ok(1));
        assert_eq!(next_chain_count(4), Ok(5));
        assert_eq!(
            next_chain_count(MAX_SAFE_COUNT + 1),
            Err(NotAChainCount((MAX_SAFE_COUNT + 1).to_string()))
        );
    }

    #[test]
    fn an_absent_count_starts_a_chain_and_a_carried_one_is_parsed() {
        assert_eq!(decode_chain_count(None), Ok(1));
        assert_eq!(decode_chain_count(Some(&json!(12))), Ok(12));
        assert_eq!(decode_chain_count(Some(&json!(3.0))), Ok(3));
        assert_eq!(
            decode_chain_count(Some(&json!(MAX_SAFE_COUNT))),
            Ok(MAX_SAFE_COUNT)
        );
    }

    #[test]
    fn a_malformed_count_is_refused() {
        for malformed in [
            json!(null),
            json!(0),
            json!(-3),
            json!(1.5),
            json!("abc"),
            json!(true),
            json!(MAX_SAFE_COUNT + 1),
        ] {
            assert_eq!(
                decode_chain_count(Some(&malformed)),
                Err(MalformedChainCount(malformed.to_string())),
                "{malformed}"
            );
        }
    }

    // Guards `count > limit`: a limit N admits hop N and refuses hop N + 1 with the count and the limit.
    #[test]
    fn a_limit_admits_its_own_count_and_refuses_the_next_hop() {
        assert_eq!(admit_chain_call(2, || Some(2)), Ok(2));
        let refused = admit_chain_call(3, || Some(2));
        assert_eq!(refused, Err(ChainCountLimitError { count: 3, limit: 2 }));
        assert_eq!(
            refused.map_err(|error| error.to_string()),
            Err(
                "This thread call would be hop 3 of its chain; your thread chain count limit is 2."
                    .to_owned()
            )
        );
        assert_eq!(ChainCountLimitError::REASON, "chain_count_limit");
    }

    #[test]
    fn no_limit_admits_a_chain_of_any_length_and_the_limit_is_read_at_each_admission() {
        assert_eq!(admit_chain_call(12, || None), Ok(12));
        assert_eq!(
            admit_chain_call(MAX_SAFE_COUNT, || None),
            Ok(MAX_SAFE_COUNT)
        );
        let mut reads = 0;
        for _ in 0..3 {
            let _ = admit_chain_call(1, || {
                reads += 1;
                None
            });
        }
        assert_eq!(reads, 3);
    }
}
