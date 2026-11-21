//! Admission of a thread call's chain count at the RPC input boundary (senpi `modes/rpc/chain-admission.ts`, decision
//! K8, senpi#5923): `prompt`, `steer` and `follow_up` accept `chainCount`, the hop count of a thread call from another
//! session, and admit it against the user's `threads.chainCountLimit` before anything of the input is recorded or
//! held. Input without `chainCount` is not a thread call and is admitted.
//!
//! ADAPTATION: senpi also reports an admitted count on the session's `input` extension event (`InputEvent.chainCount`)
//! so a thread call that session sends next carries one more. This port checks and refuses at the boundary; the `input`
//! event and the prompt options carry no count yet, so an admitted count is verified but not forwarded.
use crate::rpc_types::{ResponseRecordType, RpcResponse, RpcResponseResult};
use maho_agent::chain_count::{CHAIN_COUNT_LIMIT, admit_chain_call, decode_chain_count};
use serde_json::{Value, json};

/// The refusal reason of a thread call whose `chainCount` is not a positive safe integer (1 is a chain's first hop).
pub const RPC_ERROR_INVALID_CHAIN_COUNT: &str = "invalid_chain_count";

/// A thread call's refusal as an RPC response's `error`, `errorCode` and `errorData`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainRefusal {
    pub message: String,
    pub error_code: &'static str,
    pub error_data: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChainAdmission {
    /// Admitted, with the count the input carried (`None` when it carried none).
    Admitted {
        chain_count: Option<u64>,
    },
    Refused(ChainRefusal),
}

/// senpi `admitInputChain`: admits RPC input that may be a thread call from another session against the user's
/// limit, read at each admission. A `chainCount` that is not a positive safe integer is refused first, since the RPC
/// boundary only shape-checks a command and a malformed count would never compare above the limit.
pub fn admit_input_chain(
    chain_count: Option<&Value>,
    read_limit: impl FnOnce() -> Option<u64>,
) -> ChainAdmission {
    let Some(chain_count) = chain_count else {
        return ChainAdmission::Admitted { chain_count: None };
    };
    let Ok(count) = decode_chain_count(Some(chain_count)) else {
        return ChainAdmission::Refused(ChainRefusal {
            message: format!(
                "chainCount must be a positive integer (1 for a chain's first hop), not {chain_count}"
            ),
            error_code: RPC_ERROR_INVALID_CHAIN_COUNT,
            error_data: json!({ "chainCount": chain_count }),
        });
    };
    match admit_chain_call(count, read_limit) {
        Ok(count) => ChainAdmission::Admitted {
            chain_count: Some(count),
        },
        Err(error) => ChainAdmission::Refused(ChainRefusal {
            message: error.to_string(),
            error_code: CHAIN_COUNT_LIMIT,
            error_data: json!({ "count": error.count, "limit": error.limit }),
        }),
    }
}

/// The refusal answer of a `prompt`, `steer` or `follow_up` whose `chainCount` is refused, or `None` when the command
/// is another one or its count is admitted. Every entry that accepts these inputs runs it before anything of the input
/// is recorded or held.
pub fn refuse_input_chain(
    command: &Value,
    read_limit: impl FnOnce() -> Option<u64>,
) -> Option<RpcResponse> {
    let kind = command["type"].as_str()?;
    if !matches!(kind, "prompt" | "steer" | "follow_up") {
        return None;
    }
    let ChainAdmission::Refused(refusal) = admit_input_chain(command.get("chainCount"), read_limit)
    else {
        return None;
    };
    Some(RpcResponse {
        id: command["id"].as_str().map(str::to_owned),
        record_type: ResponseRecordType::Response,
        command: kind.to_owned(),
        session_id: command["sessionId"].as_str().map(str::to_owned),
        result: RpcResponseResult::Error {
            error: refusal.message,
            error_code: Some(refusal.error_code.to_owned()),
            error_data: Some(refusal.error_data),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn refusal(admission: ChainAdmission) -> ChainRefusal {
        match admission {
            ChainAdmission::Refused(refusal) => refusal,
            ChainAdmission::Admitted { chain_count } => {
                panic!("admitted with {chain_count:?}")
            }
        }
    }

    #[test]
    fn input_without_a_chain_count_is_not_a_thread_call_and_never_reads_the_limit() {
        let read = Cell::new(false);
        let admission = admit_input_chain(None, || {
            read.set(true);
            Some(0)
        });
        assert_eq!(admission, ChainAdmission::Admitted { chain_count: None });
        assert!(!read.get());
    }

    #[test]
    fn a_count_is_admitted_up_to_the_limit_and_at_any_length_without_one() {
        assert_eq!(
            admit_input_chain(Some(&json!(12)), || None),
            ChainAdmission::Admitted {
                chain_count: Some(12)
            }
        );
        assert_eq!(
            admit_input_chain(Some(&json!(2)), || Some(2)),
            ChainAdmission::Admitted {
                chain_count: Some(2)
            }
        );
    }

    // Guards the limit comparison: call N + 1 under a limit N is refused with the count and the limit.
    #[test]
    fn the_next_hop_past_the_limit_is_refused_with_the_typed_reason() {
        let refused = refusal(admit_input_chain(Some(&json!(3)), || Some(2)));
        assert_eq!(refused.error_code, "chain_count_limit");
        assert_eq!(refused.error_data, json!({"count": 3, "limit": 2}));
        assert_eq!(
            refused.message,
            "This thread call would be hop 3 of its chain; your thread chain count limit is 2."
        );
    }

    // Guards the shape check that runs before the limit is read: a malformed count would never compare above the
    // limit, so it would be admitted without it.
    #[test]
    fn a_malformed_count_is_refused_before_the_limit_is_read() {
        for malformed in [
            json!(null),
            json!(0),
            json!(-3),
            json!(1.5),
            json!("abc"),
            json!(9_007_199_254_740_992u64),
        ] {
            let read = Cell::new(false);
            let refused = refusal(admit_input_chain(Some(&malformed), || {
                read.set(true);
                Some(100)
            }));
            assert_eq!(refused.error_code, "invalid_chain_count", "{malformed}");
            assert_eq!(refused.error_data, json!({ "chainCount": malformed }));
            assert!(!read.get(), "{malformed}");
        }
    }

    #[test]
    fn only_prompt_steer_and_follow_up_are_refused_and_the_answer_keeps_the_routing() {
        for kind in ["prompt", "steer", "follow_up"] {
            let command =
                json!({"id":"r1","sessionId":"s1","type":kind,"message":"from B","chainCount":3});
            let answered = refuse_input_chain(&command, || Some(2)).expect("refused");
            let wire = serde_json::to_value(&answered).expect("wire");
            assert_eq!(wire["id"], "r1");
            assert_eq!(wire["sessionId"], "s1");
            assert_eq!(wire["command"], kind);
            assert_eq!(wire["success"], false);
            assert_eq!(wire["errorCode"], "chain_count_limit");
            assert_eq!(wire["errorData"], json!({"count": 3, "limit": 2}));
        }
        let other = json!({"id":"r1","type":"get_state","chainCount":3});
        assert!(refuse_input_chain(&other, || Some(2)).is_none());
        let admitted = json!({"id":"r1","type":"prompt","message":"x","chainCount":2});
        assert!(refuse_input_chain(&admitted, || Some(2)).is_none());
        let plain = json!({"id":"r1","type":"prompt","message":"x"});
        assert!(refuse_input_chain(&plain, || Some(0)).is_none());
    }
}
