//! A socket host's answer to `watch_host_occupancy` (senpi `host-occupancy-watch.ts`, oh-my-openagent#14272): how
//! many of its attached connections still occupy it, sent to every watcher at once and again whenever the number
//! changes. A connection occupies the host while it holds a session, has an open or a command in flight, or has
//! never held a session at all (an ensure's attach hold, a client that has not opened yet). One whose sessions have
//! all closed stops counting, so a parent that keeps its shard connection after its last session does not keep a
//! 0-session host alive.
//!
//! A watcher is the supervisor's observer link; while one is connected the host's own empty exit stays with the
//! supervisor ([`HostOccupancyWatch::watching`]).
use crate::host_observe_request::HOST_OCCUPANCY_RECORD;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Default)]
pub struct HostOccupancyWatch {
    watchers: BTreeSet<String>,
    in_flight: HashMap<String, usize>,
    /// Connections that held a session here at least once: only these stop counting once they hold nothing.
    held_once: HashSet<String>,
    reported: Option<usize>,
}

impl HostOccupancyWatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn watching(&self) -> bool {
        !self.watchers.is_empty()
    }

    pub fn watch<'a>(
        &mut self,
        connection: &str,
        connections: impl IntoIterator<Item = &'a str>,
        attached: impl Fn(&str) -> bool,
        holds_sessions: impl Fn(&str) -> bool,
        mut send: impl FnMut(&str, Value),
    ) {
        self.watchers.insert(connection.to_owned());
        let holders = self.holders(connections, attached, holds_sessions);
        self.reported = Some(holders);
        send(
            connection,
            json!({"type": HOST_OCCUPANCY_RECORD, "holders": holders}),
        );
    }

    pub fn begin<'a>(
        &mut self,
        connection: &str,
        connections: impl IntoIterator<Item = &'a str>,
        attached: impl Fn(&str) -> bool,
        holds_sessions: impl Fn(&str) -> bool,
        send: impl FnMut(&str, Value),
    ) {
        *self.in_flight.entry(connection.to_owned()).or_insert(0) += 1;
        self.refresh(connections, attached, holds_sessions, send);
    }

    pub fn end<'a>(
        &mut self,
        connection: &str,
        connections: impl IntoIterator<Item = &'a str>,
        attached: impl Fn(&str) -> bool,
        holds_sessions: impl Fn(&str) -> bool,
        send: impl FnMut(&str, Value),
    ) {
        if holds_sessions(connection) {
            self.held_once.insert(connection.to_owned());
        }
        if let Some(count) = self.in_flight.get_mut(connection) {
            if *count > 1 {
                *count -= 1;
            } else {
                self.in_flight.remove(connection);
            }
        }
        self.refresh(connections, attached, holds_sessions, send);
    }

    pub fn forget<'a>(
        &mut self,
        connection: &str,
        connections: impl IntoIterator<Item = &'a str>,
        attached: impl Fn(&str) -> bool,
        holds_sessions: impl Fn(&str) -> bool,
        send: impl FnMut(&str, Value),
    ) {
        self.watchers.remove(connection);
        self.in_flight.remove(connection);
        self.held_once.remove(connection);
        self.refresh(connections, attached, holds_sessions, send);
    }

    pub fn refresh<'a>(
        &mut self,
        connections: impl IntoIterator<Item = &'a str>,
        attached: impl Fn(&str) -> bool,
        holds_sessions: impl Fn(&str) -> bool,
        mut send: impl FnMut(&str, Value),
    ) {
        if self.watchers.is_empty() {
            return;
        }
        let holders = self.holders(connections, attached, holds_sessions);
        if self.reported == Some(holders) {
            return;
        }
        self.reported = Some(holders);
        let record = json!({"type": HOST_OCCUPANCY_RECORD, "holders": holders});
        for watcher in &self.watchers {
            send(watcher, record.clone());
        }
    }

    fn holders<'a>(
        &mut self,
        connections: impl IntoIterator<Item = &'a str>,
        attached: impl Fn(&str) -> bool,
        holds_sessions: impl Fn(&str) -> bool,
    ) -> usize {
        let mut holders = 0;
        for connection in connections {
            if self.watchers.contains(connection) || !attached(connection) {
                continue;
            }
            let holding = holds_sessions(connection);
            if holding {
                self.held_once.insert(connection.to_owned());
            }
            if holding
                || self.in_flight.contains_key(connection)
                || !self.held_once.contains(connection)
            {
                holders += 1;
            }
        }
        holders
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_whose_sessions_closed_stops_counting_while_fresh_and_in_flight_ones_count() {
        let mut watch = HostOccupancyWatch::new();
        let conns = ["obs", "parent", "fresh"];
        let mut parent_holds = false;
        let mut sent = Vec::<(String, usize)>::new();

        watch.watch(
            "obs",
            conns,
            |c| c != "obs",
            |c| c == "parent" && parent_holds,
            |to, rec| sent.push((to.to_owned(), rec["holders"].as_u64().unwrap() as usize)),
        );
        assert!(watch.watching());
        assert_eq!(sent, [("obs".into(), 2)]);

        parent_holds = true;
        watch.begin(
            "parent",
            conns,
            |c| c != "obs",
            |c| c == "parent" && parent_holds,
            |to, rec| sent.push((to.to_owned(), rec["holders"].as_u64().unwrap() as usize)),
        );
        watch.end(
            "parent",
            conns,
            |c| c != "obs",
            |c| c == "parent" && parent_holds,
            |to, rec| sent.push((to.to_owned(), rec["holders"].as_u64().unwrap() as usize)),
        );
        assert_eq!(sent.len(), 1);

        parent_holds = false;
        watch.refresh(
            conns,
            |c| c != "obs",
            |c| c == "parent" && parent_holds,
            |to, rec| sent.push((to.to_owned(), rec["holders"].as_u64().unwrap() as usize)),
        );
        assert_eq!(sent, [("obs".into(), 2), ("obs".into(), 1)]);

        watch.begin(
            "parent",
            conns,
            |c| c != "obs",
            |c| c == "parent" && parent_holds,
            |to, rec| sent.push((to.to_owned(), rec["holders"].as_u64().unwrap() as usize)),
        );
        assert_eq!(sent.last(), Some(&("obs".to_owned(), 2)));
        watch.end(
            "parent",
            conns,
            |c| c != "obs",
            |c| c == "parent" && parent_holds,
            |to, rec| sent.push((to.to_owned(), rec["holders"].as_u64().unwrap() as usize)),
        );
        assert_eq!(sent.last(), Some(&("obs".to_owned(), 1)));

        watch.forget(
            "obs",
            conns,
            |c| c != "obs",
            |_| false,
            |to, rec| sent.push((to.to_owned(), rec["holders"].as_u64().unwrap() as usize)),
        );
        assert!(!watch.watching());
    }
}
