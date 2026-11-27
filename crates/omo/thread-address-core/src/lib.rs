//! The thread and device address rules every thread surface shares (omo #14305): the address grammar, the device
//! resolver, the one name comparison, row identity and naming, the per-device listing state, and the typed refusals
//! as plain data.
//!
//! Upstream counterpart: omo `packages/thread-address-core/src/` (dev 0eaecf33d7). One upstream file maps to one module
//! here. The crate has no runtime dependency beyond Unicode normalization and serde: plain data in, plain data out, and
//! no message names a tool, so a caller words every refusal itself.

pub mod device;
pub mod ladder;
pub mod listing;
pub mod name;
pub mod parse;
pub mod refusals;
pub mod row;

pub use device::{DeviceRef, DeviceResolution, DeviceRoster, ResolveDeviceOptions, resolve_device};
pub use ladder::{IdPrefixStep, LadderMatch, LadderStep, LadderView, match_thread};
pub use listing::{DeviceListing, ListingRound, device_listing};
pub use name::normalize_name;
pub use parse::{
    DEVICE_PREFIX_MIN, HERE, ParsedAddress, format_address, id_shaped, is_id_prefix, is_session_id,
    is_thread_id, parse_address,
};
pub use refusals::{
    AddressRefusal, DeviceCandidate, DeviceRow, NotAskedReason, RefusalCode, RefusalDetails,
    ShareBy, SilentDevice, SilentReason, device_label, device_not_asked, device_offline,
    devices_share, devices_silent, no_device, no_thread_on_device,
};
pub use row::{
    KeyedRow, MergeRow, NamedRow, RowName, SessionRef, listing_key, merge_rows, row_address,
    row_key, row_name, same_session,
};
