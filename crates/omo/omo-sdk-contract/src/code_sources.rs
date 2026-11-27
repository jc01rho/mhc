//! The omo SDK's own failure codes, one of the three code lists design 4.4 unions into the registry (the thread
//! taxonomy and the remote module's codes are the other two).

pub const OMO_SDK_CODES: [&str; 16] = [
    "signed_out",
    "session_ended",
    "desktop_sign_in",
    "credential_store_locked",
    "credential_store_unavailable",
    "service_unreachable",
    "service_refused",
    "device_service_unavailable",
    "not_found",
    "already_signed_in",
    "approval_expired",
    "approval_denied",
    "device_limit",
    "machine_exists",
    "accounts_unreachable",
    "internal_error",
];
