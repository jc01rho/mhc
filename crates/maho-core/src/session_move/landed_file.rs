use super::{
    move_landing::LandingOutcome,
    ownership::{SESSION_MOVED, SESSION_MOVED_IN},
};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandedFile {
    Gone,
    File {
        session_path: String,
        session_id: String,
        cwd: String,
    },
}

pub fn outcome_in(entries: &[Value], move_id: &str) -> LandingOutcome {
    let mut outcome = LandingOutcome::Absent;
    for entry in entries {
        if entry["type"] != "custom" || entry["data"]["move_id"] != move_id {
            continue;
        }
        if entry["customType"] == SESSION_MOVED_IN {
            outcome = LandingOutcome::MovedIn;
        } else if outcome == LandingOutcome::MovedIn
            && entry["customType"] == SESSION_MOVED
            && entry["data"]["reason"] == "revoked"
        {
            outcome = LandingOutcome::Revoked;
        }
    }
    outcome
}

pub async fn read_landing(target: &Path, move_id: &str) -> (LandingOutcome, LandedFile) {
    if !super::path_exists(target).await {
        return (LandingOutcome::Absent, LandedFile::Gone);
    }
    let entries = super::entries(target).await;
    let outcome = outcome_in(&entries, move_id);
    let header = entries.iter().find(|entry| entry["type"] == "session");
    let Some(header) = header else {
        return (outcome, LandedFile::Gone);
    };
    let (Some(id), Some(cwd)) = (
        header
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty()),
        header
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty()),
    ) else {
        return (outcome, LandedFile::Gone);
    };
    if outcome == LandingOutcome::Absent {
        return (outcome, LandedFile::Gone);
    }
    (
        outcome,
        LandedFile::File {
            session_path: target.to_string_lossy().into_owned(),
            session_id: id.to_owned(),
            cwd: cwd.to_owned(),
        },
    )
}
