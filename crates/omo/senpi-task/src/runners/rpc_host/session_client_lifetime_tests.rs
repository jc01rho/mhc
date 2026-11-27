//! Ports `rpc-host/session-client-lifetime.test.ts` (omo #15064) against a fake host that counts the
//! connections it holds. Every wait is on an exact host-side signal under a bound; nothing sleeps.
//!
//! Not ported, because this crate's seam cannot express them:
//! - "a retained session parked by the host stays revivable": it needs a host that retains and
//!   revives sessions, and the fakes here only count connections.
//! - "a host refusing `close_session` still releases the public connection": `HostSessionPort::close`
//!   is a bare `close_session` and does not stop the connection; `detach` does.
//! - the `park` half of "occupancy returns to baseline": the handle ignores `session_parked` frames
//!   (SAFETY-DIVERGENCE in `handle.rs`), so a parked session keeps its connection.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::Value;

use super::super::handle::create_host_session_handle;
use super::super::handle_port::{HostSessionIdentity, HostSessionPort};
use super::super::open_session::OpenedHostSession;
use super::super::session_client_opening::released_while_dialing;
use super::super::session_wire::is_host_session_detached;
use super::*;
use crate::manager::child_handle::{ManagedChildListener, Unsubscribe};
use crate::runners::RunnerOutcome;

const SIGNAL: Duration = Duration::from_secs(5);

#[derive(Default)]
struct HostState {
    connections: usize,
    commands: Vec<&'static str>,
}

#[derive(Default)]
struct FakeHost {
    state: Mutex<HostState>,
    changed: Condvar,
}

impl FakeHost {
    fn dial(self: &Arc<Self>, withhold_close: bool) -> Arc<FakeConnection> {
        self.state.lock().expect("host state").connections += 1;
        self.changed.notify_all();
        Arc::new(FakeConnection {
            host: Arc::clone(self),
            released: AtomicBool::new(false),
            withhold_close,
            listeners: Mutex::new(Vec::new()),
        })
    }

    fn record(&self, command: &'static str) {
        self.state
            .lock()
            .expect("host state")
            .commands
            .push(command);
        self.changed.notify_all();
    }

    fn commands(&self) -> Vec<&'static str> {
        self.state.lock().expect("host state").commands.clone()
    }

    fn wait_for_command(&self, command: &'static str) {
        let state = self.state.lock().expect("host state");
        let (_state, waited) = self
            .changed
            .wait_timeout_while(state, SIGNAL, |state| !state.commands.contains(&command))
            .expect("host state");
        assert!(!waited.timed_out(), "the host never saw {command}");
    }

    fn wait_for_connections(&self, expected: usize) {
        let state = self.state.lock().expect("host state");
        let (state, waited) = self
            .changed
            .wait_timeout_while(state, SIGNAL, |state| state.connections != expected)
            .expect("host state");
        assert!(
            !waited.timed_out(),
            "the host held {} connections instead of {expected}",
            state.connections
        );
    }
}

struct FakeConnection {
    host: Arc<FakeHost>,
    released: AtomicBool,
    /// `close_session` is never answered: only the connection going away ends it.
    withhold_close: bool,
    listeners: Mutex<Vec<ManagedChildListener>>,
}

impl FakeConnection {
    fn is_released(&self) -> bool {
        self.released.load(Ordering::SeqCst)
    }

    fn release(&self) {
        let mut state = self.host.state.lock().expect("host state");
        if !self.released.swap(true, Ordering::SeqCst) {
            state.connections -= 1;
        }
        drop(state);
        self.host.changed.notify_all();
    }

    fn wait_until_released(&self) {
        let state = self.host.state.lock().expect("host state");
        let (_state, waited) = self
            .host
            .changed
            .wait_timeout_while(state, SIGNAL, |_| !self.is_released())
            .expect("host state");
        assert!(!waited.timed_out(), "the connection was never released");
    }

    fn emit_session_closed(&self, reason: &str) {
        let frame = crate::shared::ManagedChildEvent {
            event_type: "session_closed".into(),
            reason: Some(reason.into()),
            ..crate::shared::ManagedChildEvent::default()
        };
        let listeners: Vec<_> = self.listeners.lock().expect("listeners").clone();
        for listener in listeners {
            listener(&frame);
        }
    }
}

impl HostRpcClient for FakeConnection {
    fn command(&self, _command: Value) -> Result<(), HostError> {
        Ok(())
    }
    fn on_event(&self, listener: ManagedChildListener) -> Unsubscribe {
        self.listeners.lock().expect("listeners").push(listener);
        Box::new(|| {})
    }
    fn close(&self, _routing_id: &str) -> Result<(), HostError> {
        self.host.record("close_session");
        if self.withhold_close {
            self.wait_until_released();
            return Err(HostError {
                message: "RPC transport is gone".into(),
            });
        }
        Ok(())
    }
    fn detach(&self, _routing_id: &str) -> Result<(), HostError> {
        self.release();
        Ok(())
    }
}

impl HostRunHoldClient for FakeConnection {}

enum Answer {
    Opens,
    HeldUntil(Receiver<()>),
    Withheld,
}

/// Mirrors the sequence of `HostAssembly::open_with_policy`: the open stops a connection its owner
/// refused and a connection whose `open_session` failed.
fn open_through(
    host: &Arc<FakeHost>,
    bind: &BindClient,
    dial: Option<Receiver<()>>,
    answer: Answer,
) -> Result<Arc<FakeConnection>, HostError> {
    let connection = host.dial(false);
    if let Some(resume) = dial {
        host.record("dialing");
        resume.recv_timeout(SIGNAL).expect("the dial was resumed");
    }
    let client: Arc<dyn HostRpcClient> = connection.clone();
    if !bind(Some(client)) {
        connection.release();
        return Err(released_while_dialing());
    }
    host.record("open_session");
    match answer {
        Answer::Opens => {}
        Answer::HeldUntil(answered) => {
            host.record("attaching");
            answered
                .recv_timeout(SIGNAL)
                .expect("open_session was answered");
        }
        Answer::Withheld => {
            connection.wait_until_released();
            bind(None);
            return Err(HostError {
                message: "RPC transport is gone".into(),
            });
        }
    }
    Ok(connection)
}

/// "#given an unacknowledged close_session #when teardown detaches #then it releases the in-flight
/// close transport".
#[test]
fn a_detach_during_an_unacknowledged_close_releases_that_closes_connection() {
    // given
    let host = Arc::new(FakeHost::default());
    let connection = host.dial(true);
    let port = Arc::new(SessionClientConnection::new(
        "route".into(),
        Arc::clone(&connection) as Arc<dyn HostRpcClient>,
        Arc::clone(&connection) as Arc<dyn HostRunHoldClient>,
    ));
    host.wait_for_connections(1);
    let closing = {
        let port = Arc::clone(&port);
        std::thread::spawn(move || port.close())
    };
    host.wait_for_command("close_session");

    // when
    port.detach().expect("detach");

    // then
    host.wait_for_connections(0);
    assert!(closing.join().expect("the closing thread").is_err());
}

/// "#given an open still dialing #when teardown detaches #then the open ends refused and leaves no
/// connection on the host". The dial is held after the connection exists and before it is bound, the
/// window a slow transport widens (#14906).
#[test]
fn a_detach_while_the_open_is_dialing_ends_it_refused_and_leaves_no_connection() {
    // given
    let host = Arc::new(FakeHost::default());
    let owner = Arc::new(HostSessionClient::default());
    let bind = owner.begin_open();
    let (resume, resumed) = sync_channel(1);
    let opening = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || open_through(&host, &bind, Some(resumed), Answer::Opens).err())
    };
    host.wait_for_command("dialing");

    // when
    owner.detach().expect("detach");
    resume.send(()).expect("resume the dial");
    let outcome = opening.join().expect("the opening thread");

    // then
    let refusal = outcome.expect("the open was refused");
    assert!(is_host_session_detached(&refusal));
    host.wait_for_connections(0);
    assert!(!host.commands().contains(&"open_session"));
}

#[test]
fn a_detach_while_open_session_is_in_flight_stops_the_bound_connection() {
    // given
    let host = Arc::new(FakeHost::default());
    let owner = Arc::new(HostSessionClient::default());
    let bind = owner.begin_open();
    let opening = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || open_through(&host, &bind, None, Answer::Withheld).err())
    };
    host.wait_for_command("open_session");

    // when
    owner.detach().expect("detach");

    // then
    host.wait_for_connections(0);
    let failure = opening
        .join()
        .expect("the opening thread")
        .expect("the open failed");
    assert!(!is_host_session_detached(&failure));
    owner.detach().expect("a second detach has nothing to stop");
}

/// "#given an attached task session #when the host sends close #then public client occupancy returns
/// to baseline": the host's `session_closed` frame ends the child and detaches its connection.
#[test]
fn a_session_closed_frame_returns_the_hosts_connections_to_baseline() {
    // given
    let host = Arc::new(FakeHost::default());
    let baseline = 0;
    let connection = host.dial(false);
    let port = SessionClientConnection::new(
        "route".into(),
        Arc::clone(&connection) as Arc<dyn HostRpcClient>,
        Arc::clone(&connection) as Arc<dyn HostRunHoldClient>,
    );
    let opened = OpenedHostSession {
        session: HostSessionIdentity {
            routing_id: "route".into(),
            session_path: "/tmp/sessions/lifetime-close.jsonl".into(),
            instance_id: "lifetime".into(),
        },
        attached: true,
        durable_session_id: None,
        reported_model: None,
    };
    let handle = create_host_session_handle(
        "st_00000001".into(),
        Arc::new(port) as Arc<dyn HostSessionPort>,
        opened,
        Arc::new(|| RunnerOutcome::Cancelled),
    );
    host.wait_for_connections(baseline + 1);

    // when
    connection.emit_session_closed("host_shutdown");

    // then
    host.wait_for_connections(baseline);
    assert!(handle.stopped());
}

/// "#given two sessions sharing a host with one mid-attach #when the other closes #then the survivor
/// stays connected and attaches".
#[test]
fn a_detach_on_one_owner_leaves_a_sibling_mid_attach_connected() {
    // given
    let host = Arc::new(FakeHost::default());
    let first = Arc::new(HostSessionClient::default());
    open_through(&host, &first.begin_open(), None, Answer::Opens).expect("the first session opens");
    let survivor = Arc::new(HostSessionClient::default());
    let bind = survivor.begin_open();
    let (answer, answered) = sync_channel(1);
    let attaching = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || {
            open_through(&host, &bind, None, Answer::HeldUntil(answered)).err()
        })
    };
    host.wait_for_command("attaching");
    host.wait_for_connections(2);

    // when
    first.detach().expect("detach");

    // then
    host.wait_for_connections(1);
    answer.send(()).expect("answer open_session");
    assert!(attaching.join().expect("the attaching thread").is_none());
    host.wait_for_connections(1);
    survivor.detach().expect("detach");
    host.wait_for_connections(0);
}

#[test]
fn a_detach_refuses_only_the_opens_that_began_before_it() {
    let host = Arc::new(FakeHost::default());
    let owner = Arc::new(HostSessionClient::default());
    let earlier = owner.begin_open();
    owner.detach().expect("detach");
    let connection: Arc<dyn HostRpcClient> = host.dial(false);

    assert!(!earlier(Some(Arc::clone(&connection))));
    assert!(earlier(None), "unbinding is never refused");
    let later = owner.begin_open();
    assert!(later(Some(connection)));

    owner.detach().expect("detach");
    host.wait_for_connections(0);
}

#[test]
fn a_recovery_that_gave_up_detaches_only_the_opens_still_running() {
    let host = Arc::new(FakeHost::default());
    let opens = Arc::new(InFlightOpens::default());
    let running = Arc::new(HostSessionClient::default());
    let finished = Arc::new(HostSessionClient::default());
    let running_open = opens.register(&running);
    let finished_open = opens.register(&finished);
    open_through(&host, &running.begin_open(), None, Answer::Opens)
        .expect("the running open binds");
    open_through(&host, &finished.begin_open(), None, Answer::Opens)
        .expect("the finished open binds");
    host.wait_for_connections(2);
    drop(finished_open);

    opens.detach_all().expect("detach the running open");

    host.wait_for_connections(1);
    drop(running_open);
    opens.detach_all().expect("nothing is left to detach");
    host.wait_for_connections(1);
}
