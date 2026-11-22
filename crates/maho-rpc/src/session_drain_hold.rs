use std::collections::BTreeSet;

pub fn unattended_turn_targets(rows: &[serde_json::Value], held: &BTreeSet<String>) -> Vec<String> {
    rows.iter()
        .filter(|row| row["status"] == "open")
        .filter(|row| row["attachments"].as_i64().unwrap_or(0) == 0)
        .filter_map(|row| row["sessionId"].as_str())
        .filter(|session_id| !held.contains(*session_id))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(session_id: &str, status: &str, attachments: i64) -> serde_json::Value {
        json!({"sessionId": session_id, "status": status, "attachments": attachments})
    }

    #[test]
    fn only_open_unattached_sessions_are_held() {
        let rows = [
            row("gone", "open", 0),
            row("live", "open", 1),
            row("opening", "opening", 0),
            row("closing", "closing", 0),
        ];
        assert_eq!(
            unattended_turn_targets(&rows, &BTreeSet::new()),
            vec!["gone".to_owned()]
        );
    }

    #[test]
    fn a_session_an_earlier_pass_held_is_not_held_again() {
        let rows = [row("a", "open", 0), row("b", "open", 0)];
        let held = BTreeSet::from(["a".to_owned()]);
        assert_eq!(unattended_turn_targets(&rows, &held), vec!["b".to_owned()]);
        let held = BTreeSet::from(["a".to_owned(), "b".to_owned()]);
        assert!(unattended_turn_targets(&rows, &held).is_empty());
    }
}
