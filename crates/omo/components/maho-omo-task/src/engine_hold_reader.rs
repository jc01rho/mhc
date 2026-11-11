//! `engine-hold-reader.ts`: normalize a native engine pause projection and an RPC pause record list
//! into one Stop answer.
//!
//! A run's Stop is `session_hold` present plus every validated `run_fence` reference. An unrelated
//! hold (an input hold, a workpool fence) can never impersonate a Stop: only `session_hold` sets
//! `stopped`, and only `run_fence` entries contribute run references, deduplicated by kind+id.

use senpi_task::pause::ports::{RunRef, RunRefKind};
use serde_json::Value;

pub use senpi_task::pause::ports::{EngineHold as EngineHoldEntry, EnginePauseState};

/// One RPC pause record (`RpcPauseState.records`): the same hold, as a child's engine serializes it.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcPauseRecord {
    pub kind: String,
    pub key: String,
    pub run_ref: Option<RunRef>,
    pub parent_session_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RpcPauseState {
    pub pause_seq: u64,
    pub records: Vec<RpcPauseRecord>,
}

/// Either projection of the same hold.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineHoldState {
    Native(EnginePauseState),
    Rpc(RpcPauseState),
}

/// What the run's Stop means right now.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineRunHold {
    pub pause_seq: u64,
    pub stopped: bool,
    pub run_refs: Vec<RunRef>,
}

fn is_run_ref(value: &Value) -> Option<RunRef> {
    let object = value.as_object()?;
    let id = object.get("id")?.as_str()?;
    let kind = match object.get("kind")?.as_str()? {
        "dag" => RunRefKind::Dag,
        "task" => RunRefKind::Task,
        "workpool" => RunRefKind::Workpool,
        "team" => RunRefKind::Team,
        _ => return None,
    };
    Some(RunRef::new(kind, id))
}

fn push_unique(refs: &mut Vec<RunRef>, candidate: RunRef) {
    if !refs
        .iter()
        .any(|existing| existing.kind == candidate.kind && existing.id == candidate.id)
    {
        refs.push(candidate);
    }
}

/// The host's `getPauseState()` answer (`{pauseSeq, holds: [{kind, key, data}]}`) as the native
/// projection. `None` when it is not one: a ledger that cannot be read proves nothing was released.
pub fn native_pause_state(value: &Value) -> Option<EngineHoldState> {
    let pause_seq = value.get("pauseSeq")?.as_u64()?;
    let holds = value
        .get("holds")?
        .as_array()?
        .iter()
        .map(|hold| {
            Some(EngineHoldEntry {
                kind: hold.get("kind")?.as_str()?.to_string(),
                key: hold.get("key")?.as_str()?.to_string(),
                data: hold.get("data").filter(|data| !data.is_null()).cloned(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(EngineHoldState::Native(EnginePauseState {
        pause_seq,
        holds,
    }))
}

/// `readEngineRunHold`: the pause sequence, whether the parent is stopped, and the fenced runs.
pub fn read_engine_run_hold(state: &EngineHoldState) -> EngineRunHold {
    let (pause_seq, stopped, refs) = match state {
        EngineHoldState::Native(state) => {
            let mut refs = Vec::new();
            let mut stopped = false;
            for entry in &state.holds {
                if entry.kind == "session_hold" {
                    stopped = true;
                }
                if entry.kind != "run_fence" {
                    continue;
                }
                if let Some(candidate) = entry
                    .data
                    .as_ref()
                    .and_then(|data| data.get("runRef"))
                    .and_then(is_run_ref)
                {
                    push_unique(&mut refs, candidate);
                }
            }
            (state.pause_seq, stopped, refs)
        }
        EngineHoldState::Rpc(state) => {
            let mut refs = Vec::new();
            let mut stopped = false;
            for record in &state.records {
                if record.kind == "session_hold" {
                    stopped = true;
                }
                if record.kind != "run_fence" {
                    continue;
                }
                if let Some(candidate) = record.run_ref.clone() {
                    push_unique(&mut refs, candidate);
                }
            }
            (state.pause_seq, stopped, refs)
        }
    };
    EngineRunHold {
        pause_seq,
        stopped,
        run_refs: refs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_native_and_an_rpc_projection_of_the_same_fenced_run_agree() {
        let native = EngineHoldState::Native(EnginePauseState {
            pause_seq: 4,
            holds: vec![
                EngineHoldEntry {
                    kind: "session_hold".into(),
                    key: "parent".into(),
                    data: None,
                },
                EngineHoldEntry {
                    kind: "run_fence".into(),
                    key: "task:st_1".into(),
                    data: Some(json!({"runRef": {"kind": "task", "id": "st_1"}})),
                },
            ],
        });
        let rpc = EngineHoldState::Rpc(RpcPauseState {
            pause_seq: 4,
            records: vec![
                RpcPauseRecord {
                    kind: "session_hold".into(),
                    key: "parent".into(),
                    run_ref: None,
                    parent_session_id: None,
                },
                RpcPauseRecord {
                    kind: "run_fence".into(),
                    key: "task:st_1".into(),
                    run_ref: Some(RunRef::task("st_1")),
                    parent_session_id: Some("parent".into()),
                },
            ],
        });
        assert_eq!(read_engine_run_hold(&native), read_engine_run_hold(&rpc));
        let hold = read_engine_run_hold(&native);
        assert!(hold.stopped);
        assert_eq!(hold.pause_seq, 4);
        assert_eq!(hold.run_refs, vec![RunRef::task("st_1")]);
    }

    #[test]
    fn unrelated_holds_cannot_impersonate_a_stop() {
        let state = EngineHoldState::Native(EnginePauseState {
            pause_seq: 2,
            holds: vec![
                EngineHoldEntry {
                    kind: "input_hold".into(),
                    key: "input".into(),
                    data: None,
                },
                EngineHoldEntry {
                    kind: "run_fence".into(),
                    key: "workpool:pool".into(),
                    data: Some(json!({"runRef": {"kind": "workpool", "id": "pool"}})),
                },
            ],
        });
        let hold = read_engine_run_hold(&state);
        assert!(!hold.stopped);
        assert_eq!(
            hold.run_refs,
            vec![RunRef::new(RunRefKind::Workpool, "pool")]
        );
    }

    #[test]
    fn a_malformed_run_reference_is_ignored_rather_than_trusted() {
        let state = EngineHoldState::Native(EnginePauseState {
            pause_seq: 1,
            holds: vec![
                EngineHoldEntry {
                    kind: "run_fence".into(),
                    key: "bad".into(),
                    data: Some(json!({"runRef": {"kind": "unknown", "id": "x"}})),
                },
                EngineHoldEntry {
                    kind: "run_fence".into(),
                    key: "dup".into(),
                    data: Some(json!({"runRef": {"kind": "dag", "id": "d1"}})),
                },
                EngineHoldEntry {
                    kind: "run_fence".into(),
                    key: "dup2".into(),
                    data: Some(json!({"runRef": {"kind": "dag", "id": "d1"}})),
                },
            ],
        });
        let hold = read_engine_run_hold(&state);
        assert_eq!(hold.run_refs.len(), 1);
        assert_eq!(hold.run_refs[0].id, "d1");
        assert!(!hold.stopped);
    }

    #[test]
    fn the_hosts_pause_state_reads_as_the_native_projection_and_a_malformed_hold_proves_nothing() {
        let stopped = native_pause_state(&json!({"pauseSeq": 8, "holds": [
            {"kind": "session_hold", "key": "session_hold:7", "since": "t", "data": null},
            {"kind": "run_fence", "key": "run_fence:task:st_1",
             "data": {"runRef": {"kind": "task", "id": "st_1"}}},
        ]}))
        .expect("native state");
        let hold = read_engine_run_hold(&stopped);
        assert_eq!(hold.pause_seq, 8);
        assert!(hold.stopped);
        assert_eq!(hold.run_refs, vec![RunRef::task("st_1")]);

        let released = native_pause_state(&json!({"pauseSeq": 8, "holds": []})).expect("state");
        assert!(!read_engine_run_hold(&released).stopped);

        assert_eq!(
            native_pause_state(&json!({"pauseSeq": 8, "holds": [{"key": "no kind"}]})),
            None
        );
        assert_eq!(native_pause_state(&json!({"holds": []})), None);
    }
}
