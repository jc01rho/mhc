//! The local-only mark: a command whose whole path is this machine (an account read plus the local
//! device-service API) marks its process, and every network call site mhc owns refuses while the
//! mark is set, with a typed [`LocalOnlyNetworkRefused`] naming the site, before any I/O.
//!
//! Behaviour follows omo#13440 (the local-only `omo remote` verbs): the mark is process-wide and is
//! never cleared once set. A [`LocalOnlyGuard`] value is one mark; [`LocalOnlyGuard::process`] is the
//! process's own, and the free functions act on it.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

/// A network call reached while the process is marked local-only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalOnlyNetworkRefused {
    site: String,
}

impl LocalOnlyNetworkRefused {
    /// The machine code of this refusal.
    pub const CODE: &'static str = "local_only_network_refused";

    pub fn new(site: impl Into<String>) -> Self {
        Self { site: site.into() }
    }

    /// The call site that was refused (`feedback.submit`, `memory.account`, ...).
    pub fn site(&self) -> &str {
        &self.site
    }

    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

impl fmt::Display for LocalOnlyNetworkRefused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a local-only command reached the network at {}",
            self.site
        )
    }
}

impl std::error::Error for LocalOnlyNetworkRefused {}

/// One local-only mark.
#[derive(Debug, Default)]
pub struct LocalOnlyGuard {
    marked: AtomicBool,
}

static PROCESS: LocalOnlyGuard = LocalOnlyGuard::new();

impl LocalOnlyGuard {
    pub const fn new() -> Self {
        Self {
            marked: AtomicBool::new(false),
        }
    }

    /// The mark every call site of this process consults.
    pub fn process() -> &'static LocalOnlyGuard {
        &PROCESS
    }

    /// Sets the mark; it stays set for the life of the guard.
    pub fn mark(&self) {
        self.marked.store(true, Ordering::SeqCst);
    }

    pub fn is_marked(&self) -> bool {
        self.marked.load(Ordering::SeqCst)
    }

    /// `Err` before any I/O when the mark is set; `site` names the call site in the error.
    pub fn refuse(&self, site: &str) -> Result<(), LocalOnlyNetworkRefused> {
        if self.is_marked() {
            Err(LocalOnlyNetworkRefused::new(site))
        } else {
            Ok(())
        }
    }
}

/// Marks this process local-only.
pub fn mark_local_only_process() {
    PROCESS.mark();
}

pub fn is_local_only_process() -> bool {
    PROCESS.is_marked()
}

/// Refuses before any I/O when this process is marked local-only.
pub fn refuse_if_local_only(site: &str) -> Result<(), LocalOnlyNetworkRefused> {
    PROCESS.refuse(site)
}
