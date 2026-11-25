//! The store facade of woken sessions, session waits and intake marks.
//!
//! Upstream counterpart: `thread/gateway/store-woken-facade.ts`. Each call runs one op on the store's connection,
//! stamped with the store's clock: all woken-session time comes from it.

use super::session_wait_types::{
    DrainedMark, IntakeMark, IntakePair, OpenIntakeSession, QueuedWaitingOn, SessionWaitState,
    SessionWaitWrite, SessionWork,
};
use super::store::SqliteGatewayStore;
use super::store_api::GatewayStoreApi;
use super::store_ops::StoreError;
use super::store_woken_ops as ops;
use super::woken_types::{
    LostLease, WakeRequestOutcome, WakeRetryOutcome, WakeStartOutcome, WakeTakeOverOutcome,
    WokenSessionFence, WokenSessionRow, WokenSessionState,
};

pub struct WokenSessions<'a> {
    store: &'a SqliteGatewayStore,
}

pub struct SessionWaits<'a> {
    store: &'a SqliteGatewayStore,
}

pub struct IntakeMarks<'a> {
    store: &'a SqliteGatewayStore,
}

impl SqliteGatewayStore {
    pub fn woken(&self) -> WokenSessions<'_> {
        WokenSessions { store: self }
    }

    pub fn session_wait(&self) -> SessionWaits<'_> {
        SessionWaits { store: self }
    }

    pub fn intake_marks(&self) -> IntakeMarks<'_> {
        IntakeMarks { store: self }
    }
}

impl WokenSessions<'_> {
    pub fn request(
        &self,
        durable_id: &str,
        session_file: &str,
    ) -> Result<WakeRequestOutcome, StoreError> {
        let now = self.store.now();
        self.store
            .checked(|ctx| ops::request_wake(ctx, durable_id, session_file, now))
    }

    pub fn start(&self, durable_id: &str) -> Result<WakeStartOutcome, StoreError> {
        let now = self.store.now();
        self.store
            .checked(|ctx| ops::start_wake(ctx, durable_id, now))
    }

    pub fn renew(
        &self,
        fence: &WokenSessionFence,
    ) -> Result<Result<WokenSessionRow, LostLease>, StoreError> {
        let now = self.store.now();
        self.store.checked(|ctx| ops::renew_lease(ctx, fence, now))
    }

    pub fn take_over(&self, durable_id: &str) -> Result<WakeTakeOverOutcome, StoreError> {
        let now = self.store.now();
        self.store
            .checked(|ctx| ops::take_over_wake(ctx, durable_id, now))
    }

    pub fn set_state(
        &self,
        fence: &WokenSessionFence,
        state: WokenSessionState,
        host_instance: Option<&str>,
    ) -> Result<Result<WokenSessionRow, LostLease>, StoreError> {
        let now = self.store.now();
        self.store
            .checked(|ctx| ops::set_woken_state(ctx, fence, state, host_instance, now))
    }

    pub fn record_waiting(
        &self,
        fence: &WokenSessionFence,
    ) -> Result<Result<WokenSessionRow, LostLease>, StoreError> {
        let now = self.store.now();
        self.store
            .checked(|ctx| ops::record_waiting(ctx, fence, now))
    }

    pub fn retry_later(&self, fence: &WokenSessionFence) -> Result<WakeRetryOutcome, StoreError> {
        let now = self.store.now();
        self.store
            .checked(|ctx| ops::retry_wake_later(ctx, fence, now))
    }

    pub fn clear_waiting(
        &self,
        fence: &WokenSessionFence,
    ) -> Result<Result<(), LostLease>, StoreError> {
        self.store.checked(|ctx| ops::clear_waiting(ctx, fence))
    }

    pub fn end(&self, fence: &WokenSessionFence) -> Result<Result<(), LostLease>, StoreError> {
        let now = self.store.now();
        self.store.checked(|ctx| ops::end_wake(ctx, fence, now))
    }

    pub fn gated(&self, durable_id: &str) -> Result<bool, StoreError> {
        let now = self.store.now();
        self.store
            .checked(|ctx| ops::is_gated(ctx, durable_id, now))
    }

    pub fn list(&self) -> Result<Vec<WokenSessionRow>, StoreError> {
        self.store.checked(ops::list_woken)
    }

    pub fn count_wake(
        &self,
        durable_id: &str,
        human_input_revision: Option<i64>,
    ) -> Result<i64, StoreError> {
        self.store
            .checked(|ctx| ops::count_wake(ctx, durable_id, human_input_revision))
    }

    pub fn wake_count(&self, durable_id: &str) -> Result<i64, StoreError> {
        self.store.checked(|ctx| ops::wake_count(ctx, durable_id))
    }

    pub fn reset_count(&self, durable_id: &str) -> Result<(), StoreError> {
        self.store.checked(|ctx| ops::reset_count(ctx, durable_id))
    }

    pub fn session_work(&self, durable_id: &str) -> Result<SessionWork, StoreError> {
        self.store.checked(|ctx| ops::session_work(ctx, durable_id))
    }
}

impl SessionWaits<'_> {
    pub fn write(&self, write: &SessionWaitWrite) -> Result<(), StoreError> {
        let now = self.store.now();
        self.store.checked(|ctx| ops::write_wait(ctx, write, now))
    }

    pub fn read(&self, durable_id: &str) -> Result<Option<SessionWaitState>, StoreError> {
        self.store.checked(|ctx| ops::read_wait(ctx, durable_id))
    }

    pub fn clear(&self, durable_id: &str) -> Result<(), StoreError> {
        self.store.checked(|ctx| ops::clear_wait(ctx, durable_id))
    }

    pub fn waiting_on(&self, durable_id: &str) -> Result<Option<QueuedWaitingOn>, StoreError> {
        self.store.checked(|ctx| ops::waiting_on(ctx, durable_id))
    }
}

impl IntakeMarks<'_> {
    pub fn record(
        &self,
        pair: &IntakePair,
        command_id: &str,
        ledger_id: Option<&str>,
    ) -> Result<(), StoreError> {
        let now = self.store.now();
        self.store
            .checked(|ctx| ops::record_intake_command(ctx, pair, command_id, ledger_id, now))
    }

    pub fn command_for(&self, ledger_id: &str) -> Result<Option<String>, StoreError> {
        self.store
            .checked(|ctx| ops::command_for_row(ctx, ledger_id))
    }

    pub fn open_for_command(
        &self,
        sender_device: &str,
        command_id: &str,
    ) -> Result<bool, StoreError> {
        self.store
            .checked(|ctx| ops::open_for_mark_command(ctx, sender_device, command_id))
    }

    pub fn marked(&self) -> Result<Vec<IntakePair>, StoreError> {
        self.store.checked(ops::marked_pairs)
    }

    pub fn pending(&self) -> Result<Vec<IntakeMark>, StoreError> {
        self.store.checked(ops::pending_marks)
    }

    pub fn drained(&self, pair: &IntakePair) -> Result<DrainedMark, StoreError> {
        self.store.checked(|ctx| ops::drained_mark(ctx, pair))
    }

    pub fn open_sessions(&self) -> Result<Vec<OpenIntakeSession>, StoreError> {
        self.store.checked(ops::open_intake_sessions)
    }
}
