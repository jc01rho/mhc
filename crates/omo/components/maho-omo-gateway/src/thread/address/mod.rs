//! The thread address module (omo `thread/address/`, #14305).
//!
//! One resolver answers every address form the thread tools take: `self`, a bare address (durable id, session id, alias,
//! name, or a unique id prefix) and `<device>/<thread>`. Upstream ships this module over the account surface
//! (`RemoteThreadSurface`: the device service, the account listing and the per-device probe). mhc has no such surface:
//! there is no account read, no relay transport and no single-device probe, so every branch upstream guards with
//! `ctx.remote === undefined` is the only branch here. That is upstream's own no-account behaviour, not an
//! approximation of it: a `here/<thread>` address resolves on this device's book, any other device part is retried as a
//! whole bare address (a thread name may contain `/`), and a pinned reply cannot be asked of another device at all.
//!
//! Upstream counterpart: omo `packages/omo-senpi/src/components/thread/address/` at `v5.1.83` (`69a1863a457`). One
//! upstream file maps to one module here: `types.ts` -> `types.rs`, `refusals.ts` -> `refusals.rs`, `device.ts` ->
//! `device.rs`, `ladder.ts` -> `ladder.rs`, `bare.ts` -> `bare.rs`, `resolve.ts` -> `resolve.rs`, and `index.ts` is
//! this module's re-exports. `account.ts`, `holder.ts`, `on-device.ts`, `probe.ts` and `lazy.ts` read the account
//! surface and are not ported; each is named with its reason in `docs/omo-sync/v5.1.83/thread-address.md`.

pub mod bare;
pub mod device;
pub mod ladder;
pub mod refusals;
pub mod resolve;
pub mod types;

pub use bare::resolve_bare;
pub use device::{AccountDevice, AccountDevices, device_rows, resolve_account_device};
pub use ladder::{LadderResult, ladder};
pub use refusals::{
    AddressCandidate, ThreadRefusal, as_failure, as_refusal, tool_refusal, with_addresses,
};
pub use resolve::resolve_address;
pub use types::{
    Found, LocalBookFor, Pin, Resolution, ResolveContext, ResolveOptions, Resolved, ResolvedWhere,
};
