//! Rotation on revoke (sync-design section 7; #12280 design section 3.1). A revoke, a sign-out or a
//! recovery-key removal makes the service set `rotation_pending` and refuse every push `stale_base`
//! until a member appends a rotation that excludes every revoked principal. Any eligible member does
//! it, racing on the sequencer: the loser reads `stale_base`, discards its key and adopts the
//! winner's. The device list only subtracts members, wraps go only to principals of the verified
//! fold, and a rotation counts as landed only once this device's verified chain shows it. Key
//! material is zeroed on every exit (`EpochMaterial` holds it in `Zeroizing`).
//!
//! The chain is reached through [`FreezeChain`] so the settler runs over memory-core's
//! [`AccountChain`] and over a harness's own chain alike.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use super::account_chain::AccountChain;
use super::freeze_eligibility::{
    Gate, SettleMemory, at_served_head, chain_gate, revoked_grew, rotation_members,
    unchanged_since, view_gate,
};
use super::http::AccountHttp;
use super::revocation_view::RevocationView;
use crate::sync::crypto::chain::ChainState;
use crate::sync::crypto::entry::Head;
use crate::sync::engine::error::EngineResult;
use crate::sync::engine::ports::{FreezeOutcome, FreezeReason, FreezeSettler, PortFuture};
use crate::sync::engine::push::{stale_backoff_ms, system_random};
use crate::sync::keyring_port::DeviceKeyring;
use crate::sync::recovery::key_material::EpochMaterial;
use crate::sync::recovery::keyring_append::{RotationInput, own_material, rotation_append};
use crate::sync::recovery::ports::TransportRefusal;
use crate::sync::recovery::served_chain::{committed_wrap, expected_history};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeEvent {
    Rotated,
    RotationAdopted,
    RotationRefused,
    Unverified,
    MemberUnlisted,
}

impl FreezeEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rotated => "sync.keyring.rotated",
            Self::RotationAdopted => "sync.keyring.rotation_adopted",
            Self::RotationRefused => "sync.keyring.rotation_refused",
            Self::Unverified => "sync.keyring.unverified",
            Self::MemberUnlisted => "sync.keyring.member_unlisted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeFaultPoint {
    AfterStateRead,
    AfterDevicesRead,
    BeforeAppend,
}

/// The verified account chain as the settler reads it.
pub trait FreezeChain {
    fn account(&self) -> String;
    /// Extends the verified chain until it covers `seq` (or the service has nothing more).
    fn reach(&self, seq: u64) -> PortFuture<'_, ()>;
    fn snapshot(&self) -> PortFuture<'_, ChainState>;
    /// This device's key for `epoch`, opened from its own committed wrap.
    fn epoch_key(&self, epoch: u64) -> PortFuture<'_, Option<Vec<u8>>>;
}

/// memory-core's own account chain, shared with the session's verifier and sealer.
pub struct SharedAccountChain {
    pub account: String,
    pub chain: Rc<tokio::sync::Mutex<AccountChain>>,
}

impl FreezeChain for SharedAccountChain {
    fn account(&self) -> String {
        self.account.clone()
    }
    fn reach(&self, seq: u64) -> PortFuture<'_, ()> {
        Box::pin(async move {
            self.chain.lock().await.reach(seq).await?;
            Ok(())
        })
    }
    fn snapshot(&self) -> PortFuture<'_, ChainState> {
        Box::pin(async move { Ok(self.chain.lock().await.state().clone()) })
    }
    fn epoch_key(&self, epoch: u64) -> PortFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move { Ok(self.chain.lock().await.epoch_key(epoch)) })
    }
}

pub type RevocationSource = Rc<dyn Fn() -> PortFuture<'static, RevocationView>>;
pub type FreezeSleep = Rc<dyn Fn(u64) -> PortFuture<'static, ()>>;
pub type FreezeFault = Rc<dyn Fn(FreezeFaultPoint) -> PortFuture<'static, ()>>;

pub struct FreezeSettlerInput {
    pub chain: Rc<dyn FreezeChain>,
    pub http: AccountHttp,
    pub keyring: Rc<dyn DeviceKeyring>,
    pub revocation_view: RevocationSource,
    /// One account on one device: the in-process single-flight and the rule-7 memory are keyed by it.
    pub scope: String,
    pub on_event: Option<Rc<dyn Fn(FreezeEvent)>>,
    /// Test-only: awaited at the named points so a test can admit a competing record first.
    pub fault: Option<FreezeFault>,
    pub sleep: Option<FreezeSleep>,
}

const ATTEMPTS: u32 = 3;

thread_local! {
    static ROTATED: RefCell<HashMap<String, SettleMemory>> = RefCell::new(HashMap::new());
    static REFUSED: RefCell<HashMap<String, SettleMemory>> = RefCell::new(HashMap::new());
    static IN_FLIGHT: RefCell<HashMap<String, Rc<tokio::sync::Mutex<()>>>> =
        RefCell::new(HashMap::new());
}

fn remembered(
    table: &'static std::thread::LocalKey<RefCell<HashMap<String, SettleMemory>>>,
    scope: &str,
) -> Option<SettleMemory> {
    table.with(|t| t.borrow().get(scope).cloned())
}

/// Forgets what this process remembered about `scope`'s rotations (a sign-out or a re-join).
pub fn forget_settle_memory(scope: &str) {
    ROTATED.with(|t| t.borrow_mut().remove(scope));
    REFUSED.with(|t| t.borrow_mut().remove(scope));
}

enum Attempt {
    Done(FreezeOutcome),
    Stale,
    Reread,
}

struct Usable<'a> {
    revoked: &'a BTreeSet<String>,
    listed: &'a BTreeSet<String>,
}

pub struct ChainFreezeSettler {
    input: FreezeSettlerInput,
    self_id: String,
    self_signing: String,
}

pub fn create_freeze_settler(input: FreezeSettlerInput) -> ChainFreezeSettler {
    let self_id = input.keyring.principal().id;
    let self_signing = input.keyring.public_keys().signing;
    ChainFreezeSettler {
        input,
        self_id,
        self_signing,
    }
}

fn frozen(reason: FreezeReason) -> FreezeOutcome {
    FreezeOutcome::Frozen(reason)
}

fn same_members(state: &ChainState, ids: &BTreeSet<String>) -> bool {
    state.authority().is_some_and(|a| {
        a.members.len() == ids.len() && ids.iter().all(|id| a.members.contains_key(id))
    })
}

impl ChainFreezeSettler {
    fn emit(&self, event: FreezeEvent) {
        if let Some(on_event) = &self.input.on_event {
            on_event(event);
        }
    }

    async fn fault(&self, point: FreezeFaultPoint) -> EngineResult<()> {
        match &self.input.fault {
            Some(fault) => fault(point).await,
            None => Ok(()),
        }
    }

    async fn sleep(&self, ms: u64) -> EngineResult<()> {
        match &self.input.sleep {
            Some(sleep) => sleep(ms).await,
            None => {
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                Ok(())
            }
        }
    }

    /// One attempt at the served head: rotate with the material this device opened from its own wrap.
    async fn attempt(
        &self,
        served: &Head,
        material: &EpochMaterial,
        view: Usable<'_>,
        state: &ChainState,
    ) -> EngineResult<Attempt> {
        let scope = &self.input.scope;
        let (Some(authority), Some(head)) = (state.authority(), state.head()) else {
            return Ok(Attempt::Done(frozen(FreezeReason::Unverified)));
        };
        if unchanged_since(
            remembered(&ROTATED, scope).as_ref(),
            &served.keyring_link,
            view.revoked,
        ) {
            return Ok(Attempt::Done(frozen(FreezeReason::FlagStuck)));
        }
        if unchanged_since(
            remembered(&REFUSED, scope).as_ref(),
            &served.keyring_link,
            view.revoked,
        ) {
            return Ok(Attempt::Done(frozen(FreezeReason::Refused)));
        }
        let (members, unlisted) = rotation_members(authority, view.revoked, view.listed);
        for _ in 0..unlisted {
            self.emit(FreezeEvent::MemberUnlisted);
        }
        let account = self.input.chain.account();
        let registry: Vec<_> = authority.registry.values().cloned().collect();
        let request = rotation_append(RotationInput {
            keyring: self.input.keyring.as_ref(),
            head,
            account: &account,
            authority,
            retained: material,
            members: &members,
            registry: &registry,
        })
        .map_err(crate::sync::engine::error::EngineError::Recovery)?;
        self.fault(FreezeFaultPoint::BeforeAppend).await?;
        match self.input.http.append_keys(request).await? {
            Ok(seq) => {
                self.input.chain.reach(seq).await?;
                let after = self.input.chain.snapshot().await?;
                let next_epoch = authority.epoch + 1;
                let ids: BTreeSet<String> = members.iter().map(|m| m.id.clone()).collect();
                let verified = after.authority().is_some_and(|a| a.epoch == next_epoch)
                    && same_members(&after, &ids)
                    && self.input.chain.epoch_key(next_epoch).await?.is_some();
                let Some(after_head) = after.head().filter(|_| verified) else {
                    self.emit(FreezeEvent::Unverified);
                    return Ok(Attempt::Done(frozen(FreezeReason::Unverified)));
                };
                let memory = SettleMemory {
                    keyring_link: after_head.keyring_link.clone(),
                    revoked: view.revoked.clone(),
                };
                ROTATED.with(|t| t.borrow_mut().insert(scope.clone(), memory));
                REFUSED.with(|t| t.borrow_mut().remove(scope));
                self.emit(FreezeEvent::Rotated);
                Ok(Attempt::Done(FreezeOutcome::Rotated))
            }
            Err(TransportRefusal::StaleBase) => Ok(Attempt::Stale),
            Err(TransportRefusal::RateLimited) => {
                Ok(Attempt::Done(frozen(FreezeReason::RateLimited)))
            }
            Err(TransportRefusal::NotFound) => Ok(Attempt::Done(frozen(FreezeReason::Unavailable))),
            Err(TransportRefusal::Refused) => Ok(Attempt::Reread),
        }
    }

    async fn settle_once(&self) -> EngineResult<FreezeOutcome> {
        let scope = &self.input.scope;
        let mut lost_race = false;
        let mut reread = false;
        for round in 0..ATTEMPTS {
            if round > 0 && !lost_race {
                self.sleep(stale_backoff_ms(round - 1, &system_random))
                    .await?;
            }
            let served = self.input.http.state().await?;
            self.fault(FreezeFaultPoint::AfterStateRead).await?;
            let Some(served) = served else {
                return Ok(frozen(FreezeReason::Unavailable));
            };
            if !served.rotation_pending {
                if lost_race {
                    self.emit(FreezeEvent::RotationAdopted);
                    return Ok(FreezeOutcome::Adopted);
                }
                return Ok(FreezeOutcome::Clear);
            }
            self.input.chain.reach(served.head.seq).await?;
            let state = self.input.chain.snapshot().await?;
            let Some(authority) = state
                .authority()
                .filter(|_| at_served_head(&state, &served.head))
            else {
                self.emit(FreezeEvent::Unverified);
                return Ok(frozen(FreezeReason::Unverified));
            };
            match chain_gate(&state, &self.self_id, &self.self_signing) {
                Gate::Ok => {}
                Gate::NotMember => return Ok(FreezeOutcome::NotMember),
                Gate::Frozen(reason) => return Ok(frozen(reason)),
            }
            let account = self.input.chain.account();
            let material =
                committed_wrap(&state, authority.epoch, &format!("device:{}", self.self_id))
                    .and_then(|wrap| {
                        own_material(
                            self.input.keyring.as_ref(),
                            &account,
                            &wrap,
                            &expected_history(authority, authority.epoch),
                        )
                    });
            let Some(material) = material else {
                return Ok(frozen(FreezeReason::NoEpochKey));
            };
            let view = (self.input.revocation_view)().await?;
            self.fault(FreezeFaultPoint::AfterDevicesRead).await?;
            match view_gate(&view, &self.self_id) {
                Gate::Ok => {}
                Gate::NotMember => return Ok(FreezeOutcome::NotMember),
                Gate::Frozen(reason) => return Ok(frozen(reason)),
            }
            let RevocationView::Usable {
                revoked, listed, ..
            } = &view
            else {
                return Ok(frozen(FreezeReason::DeviceListUnusable));
            };
            let usable = Usable { revoked, listed };
            match self
                .attempt(&served.head, &material, usable, &state)
                .await?
            {
                Attempt::Done(outcome) => return Ok(outcome),
                Attempt::Stale => {
                    lost_race = true;
                    continue;
                }
                Attempt::Reread => {}
            }
            if !reread {
                reread = true;
                if revoked_grew(revoked, &(self.input.revocation_view)().await?) {
                    continue;
                }
            }
            let memory = SettleMemory {
                keyring_link: served.head.keyring_link.clone(),
                revoked: revoked.clone(),
            };
            REFUSED.with(|t| t.borrow_mut().insert(scope.clone(), memory));
            self.emit(FreezeEvent::RotationRefused);
            return Ok(frozen(FreezeReason::Refused));
        }
        Ok(frozen(FreezeReason::Rotating))
    }

    /// One settle per scope at a time. PORT-DIVERGENCE(omo #12302): upstream hands a concurrent
    /// caller the running settle's promise; this port makes it wait for that settle to finish and
    /// then settle again, which reads the ended freeze back as `clear`.
    pub async fn settle_scoped(&self) -> FreezeOutcome {
        let gate = IN_FLIGHT.with(|t| {
            t.borrow_mut()
                .entry(self.input.scope.clone())
                .or_insert_with(|| Rc::new(tokio::sync::Mutex::new(())))
                .clone()
        });
        let _running = gate.lock().await;
        self.settle_once()
            .await
            .unwrap_or_else(|_| frozen(FreezeReason::Unavailable))
    }
}

impl FreezeSettler for ChainFreezeSettler {
    fn settle(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = FreezeOutcome> + '_>> {
        Box::pin(self.settle_scoped())
    }
}

#[cfg(test)]
#[path = "freeze_settler_tests.rs"]
mod tests;
