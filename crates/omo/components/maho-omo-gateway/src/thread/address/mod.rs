//! The thread address module (omo `thread/address/`, #14305).
//!
//! One resolver answers every address form the thread tools take: `self`, a bare address (durable id, session id, alias,
//! name, or a unique id prefix) and `<device>/<thread>`. It runs over this device's book and, when
//! `ResolveContext.remote` is set, over the account surface (`thread::remote::ports::RemoteThreadSurface`: the device
//! service client, the account listing with its single probe, and the alias registry). Without that surface every
//! branch upstream guards with `ctx.remote === undefined` is the only branch: a `here/<thread>` address resolves on
//! this device's book, any other device part is retried as a whole bare address (a thread name may contain `/`), and a
//! pinned reply cannot be asked of another device at all.
//!
//! Upstream counterpart: omo `packages/omo-senpi/src/components/thread/address/` at `v5.1.83` (`69a1863a457`). One
//! upstream file maps to one module here: `types.ts` -> `types.rs`, `refusals.ts` -> `refusals.rs`, `device.ts` ->
//! `device.rs`, `ladder.ts` -> `ladder.rs`, `bare.ts` -> `bare.rs`, `account.ts` -> `account.rs`, `holder.ts` ->
//! `holder.rs`, `on-device.ts` -> `on_device.rs`, `probe.ts` -> `probe.rs`, `resolve.ts` -> `resolve.rs`, and `index.ts`
//! is this module's re-exports. `lazy.ts` is a JS entry-bundle split with no Rust counterpart.

pub mod account;
pub mod bare;
pub mod device;
pub mod holder;
pub mod ladder;
pub mod on_device;
pub mod probe;
pub mod refusals;
pub mod resolve;
pub mod types;

#[cfg(test)]
mod two_device_tests;
#[cfg(test)]
mod two_devices;

pub use account::{
    Account, AccountAttempt, AccountRead, AccountReads, Reads, StatusRound, account_devices,
    read_account, read_roster, roster_account,
};
pub use bare::{BareResolved, device_name_of, listed_entry, resolve_bare};
pub use device::{AccountDevice, AccountDevices, device_rows, resolve_account_device};
pub use holder::{RemoteTarget, holder_target};
pub use ladder::{LadderResult, ladder};
pub use on_device::resolve_on_device;
pub use probe::{Known, pinned, probe_across};
pub use refusals::{
    AddressCandidate, ThreadRefusal, as_failure, as_refusal, refusal, tool_refusal, with_addresses,
};
pub use resolve::resolve_address;
pub use types::{
    Found, LocalBookFor, Pin, Resolution, ResolveContext, ResolveOptions, Resolved, ResolvedWhere,
};
