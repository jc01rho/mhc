use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{LazyLock, Mutex},
};

pub const ENGINE_TRUST_MARKER_ENV: &str = "SENPI_ENGINE_TRUST";
pub const ENGINE_TRUST_HOP_ENV: &str = "SENPI_ENGINE_TRUST_HOP";
const EXTRA_CA_ENV: &str = "NODE_EXTRA_CA_CERTS";

pub const ENGINE_TRUST_RESTORE_NAMES: [&str; 5] = [
    "NODE_EXTRA_CA_CERTS",
    "NODE_USE_SYSTEM_CA",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "BUN_OPTIONS",
];

const SERVER_REMOVED_NAMES: [&str; 2] = ["NODE_USE_SYSTEM_CA", "BUN_OPTIONS"];
const ALWAYS_RESTORED_NAMES: [&str; 4] = [
    "NODE_EXTRA_CA_CERTS",
    "NODE_USE_SYSTEM_CA",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

pub const ENGINE_TRUST_FALLBACK_REASONS: [&str; 10] = [
    "helper-timeout",
    "helper-crash",
    "helper-malformed-output",
    "helper-missing",
    "helper-wrong-arch",
    "helper-signature",
    "access-denied",
    "negatives-incomplete",
    "linux-bundle-unreadable",
    "linux-bundle-malformed",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineTrustContractErrorCode {
    TrustMarkerInvalid,
    TrustPathUnrepresentable,
}

impl EngineTrustContractErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TrustMarkerInvalid => "trust-marker-invalid",
            Self::TrustPathUnrepresentable => "trust-path-unrepresentable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineTrustContractError {
    pub code: EngineTrustContractErrorCode,
    pub message: String,
}

impl EngineTrustContractError {
    pub fn new(code: EngineTrustContractErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for EngineTrustContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EngineTrustContractError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineTrustDefaults {
    pub file: String,
    pub dir: String,
}

pub type EngineTrustRestore = BTreeMap<String, Option<String>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineTrustMarker {
    pub v: u32,
    pub file: String,
    pub sha256: String,
    pub automatic: String,
    pub fallback_reason: Option<String>,
    pub owner: String,
    pub user: Option<String>,
    pub defaults: Option<EngineTrustDefaults>,
    pub restore: EngineTrustRestore,
}

fn is_valid_path(value: &str) -> bool {
    !value.is_empty() && !value.contains('\0')
}

fn is_valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_marker(marker: &EngineTrustMarker) -> bool {
    if marker.v != 1 || !is_valid_path(&marker.file) || !is_valid_digest(&marker.sha256) {
        return false;
    }
    if !matches!(marker.automatic.as_str(), "os" | "fallback") {
        return false;
    }
    if let Some(reason) = &marker.fallback_reason
        && !ENGINE_TRUST_FALLBACK_REASONS.contains(&reason.as_str())
    {
        return false;
    }
    if (marker.automatic == "fallback") != marker.fallback_reason.is_some() {
        return false;
    }
    if !matches!(marker.owner.as_str(), "server" | "standalone") {
        return false;
    }
    if let Some(user) = &marker.user
        && !is_valid_digest(user)
    {
        return false;
    }
    match &marker.defaults {
        Some(defaults) => {
            if !is_valid_path(&defaults.file) || !is_valid_path(&defaults.dir) {
                return false;
            }
        }
        None => {
            if marker.owner != "standalone" {
                return false;
            }
        }
    }
    for key in marker.restore.keys() {
        if !ENGINE_TRUST_RESTORE_NAMES.contains(&key.as_str()) {
            return false;
        }
    }
    if marker.owner != "server" && marker.restore.contains_key("BUN_OPTIONS") {
        return false;
    }
    true
}

pub fn parse_engine_trust_marker(text: &str) -> Option<EngineTrustMarker> {
    let raw: Value = serde_json::from_str(text).ok()?;
    let obj = raw.as_object()?;
    const REQUIRED_KEYS: [&str; 9] = [
        "v",
        "file",
        "sha256",
        "automatic",
        "fallbackReason",
        "owner",
        "user",
        "defaults",
        "restore",
    ];
    if obj.len() != REQUIRED_KEYS.len() || !REQUIRED_KEYS.iter().all(|key| obj.contains_key(*key)) {
        return None;
    }
    if let Some(defaults) = obj.get("defaults")
        && !defaults.is_null()
    {
        let def_obj = defaults.as_object()?;
        if def_obj.len() != 2 || !def_obj.contains_key("file") || !def_obj.contains_key("dir") {
            return None;
        }
    }
    let restore_obj = obj.get("restore")?.as_object()?;
    for (key, value) in restore_obj {
        if !ENGINE_TRUST_RESTORE_NAMES.contains(&key.as_str())
            || !(value.is_string() || value.is_null())
        {
            return None;
        }
    }
    let marker: EngineTrustMarker = serde_json::from_value(raw).ok()?;
    validate_marker(&marker).then_some(marker)
}

pub fn serialize_engine_trust_marker(
    marker: &EngineTrustMarker,
) -> Result<String, EngineTrustContractError> {
    if !validate_marker(marker) {
        return Err(EngineTrustContractError::new(
            EngineTrustContractErrorCode::TrustMarkerInvalid,
            "engine trust marker is not version 1",
        ));
    }
    let mut map = Map::new();
    map.insert("v".into(), marker.v.into());
    map.insert("file".into(), marker.file.clone().into());
    map.insert("sha256".into(), marker.sha256.clone().into());
    map.insert("automatic".into(), marker.automatic.clone().into());
    map.insert(
        "fallbackReason".into(),
        marker
            .fallback_reason
            .clone()
            .map_or(Value::Null, Value::String),
    );
    map.insert("owner".into(), marker.owner.clone().into());
    map.insert(
        "user".into(),
        marker.user.clone().map_or(Value::Null, Value::String),
    );
    map.insert(
        "defaults".into(),
        match &marker.defaults {
            Some(defaults) => serde_json::json!({
                "file": defaults.file,
                "dir": defaults.dir,
            }),
            None => Value::Null,
        },
    );
    let mut restore = Map::new();
    for (key, value) in &marker.restore {
        restore.insert(
            key.clone(),
            value.clone().map_or(Value::Null, Value::String),
        );
    }
    map.insert("restore".into(), Value::Object(restore));
    serde_json::to_string(&Value::Object(map)).map_err(|_| {
        EngineTrustContractError::new(
            EngineTrustContractErrorCode::TrustMarkerInvalid,
            "engine trust marker is not version 1",
        )
    })
}

fn case_forms_of<'a>(
    env: &'a BTreeMap<String, String>,
    name: &str,
    platform: &str,
) -> Vec<&'a String> {
    if platform != "win32" {
        return env
            .get_key_value(name)
            .map(|(k, _)| k)
            .into_iter()
            .collect();
    }
    env.keys()
        .filter(|key| key.eq_ignore_ascii_case(name))
        .collect()
}

pub fn has_name(env: &BTreeMap<String, String>, name: &str, platform: &str) -> bool {
    !case_forms_of(env, name, platform).is_empty()
}

pub fn values_of(env: &BTreeMap<String, String>, name: &str, platform: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for key in case_forms_of(env, name, platform) {
        if let Some(value) = env.get(key)
            && seen.insert(value.clone())
        {
            out.push(value.clone());
        }
    }
    out
}

pub fn delete_name(env: &mut BTreeMap<String, String>, name: &str, platform: &str) {
    if platform == "win32" {
        env.retain(|key, _| !key.eq_ignore_ascii_case(name));
    } else {
        env.remove(name);
    }
}

pub fn set_name(
    env: &mut BTreeMap<String, String>,
    name: &str,
    value: impl Into<String>,
    platform: &str,
) {
    delete_name(env, name, platform);
    env.insert(name.to_owned(), value.into());
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustMarkerArrival {
    None,
    Addressed(EngineTrustMarker),
    Foreign(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustArrival {
    pub hop: bool,
    pub marker: TrustMarkerArrival,
}

pub fn classify_trust_arrival(env: &BTreeMap<String, String>, platform: &str) -> TrustArrival {
    let hop = has_name(env, ENGINE_TRUST_HOP_ENV, platform);
    let marker_texts = values_of(env, ENGINE_TRUST_MARKER_ENV, platform);
    if marker_texts.is_empty() {
        return TrustArrival {
            hop,
            marker: TrustMarkerArrival::None,
        };
    }
    let extra_files = values_of(env, EXTRA_CA_ENV, platform);
    if marker_texts.len() > 1 || extra_files.len() > 1 {
        return TrustArrival {
            hop,
            marker: TrustMarkerArrival::Foreign("ambiguous"),
        };
    }
    let Some(marker) = parse_engine_trust_marker(&marker_texts[0]) else {
        return TrustArrival {
            hop,
            marker: TrustMarkerArrival::Foreign("unparseable"),
        };
    };
    if extra_files.first().map(String::as_str) != Some(marker.file.as_str()) {
        return TrustArrival {
            hop,
            marker: TrustMarkerArrival::Foreign("file-mismatch"),
        };
    }
    TrustArrival {
        hop,
        marker: TrustMarkerArrival::Addressed(marker),
    }
}

pub struct TrustLaunchOptions<'a> {
    pub marker: &'a EngineTrustMarker,
    pub hop: bool,
    pub platform: &'a str,
}

fn representable_path<'a>(
    path: &'a str,
    platform: &str,
) -> Result<&'a str, EngineTrustContractError> {
    let sep = if platform == "win32" { ';' } else { ':' };
    if path.contains(sep) {
        return Err(EngineTrustContractError::new(
            EngineTrustContractErrorCode::TrustPathUnrepresentable,
            "engine trust closure path holds a list separator",
        ));
    }
    Ok(path)
}

pub fn build_trust_launch_environment(
    base: &BTreeMap<String, String>,
    options: TrustLaunchOptions<'_>,
) -> Result<BTreeMap<String, String>, EngineTrustContractError> {
    let TrustLaunchOptions {
        marker,
        hop,
        platform,
    } = options;
    let mut env = base.clone();
    if marker.owner == "server" {
        for name in SERVER_REMOVED_NAMES {
            delete_name(&mut env, name, platform);
        }
    }
    if let Some(defaults) = &marker.defaults {
        let file = representable_path(&defaults.file, platform)?;
        let dir = representable_path(&defaults.dir, platform)?;
        set_name(&mut env, "SSL_CERT_FILE", file, platform);
        set_name(&mut env, "SSL_CERT_DIR", dir, platform);
    }
    set_name(&mut env, EXTRA_CA_ENV, &marker.file, platform);
    let serialized = serialize_engine_trust_marker(marker)?;
    set_name(&mut env, ENGINE_TRUST_MARKER_ENV, serialized, platform);
    if hop {
        set_name(&mut env, ENGINE_TRUST_HOP_ENV, "1", platform);
    } else {
        delete_name(&mut env, ENGINE_TRUST_HOP_ENV, platform);
    }
    Ok(env)
}

pub fn apply_engine_trust_restore(
    env: &mut BTreeMap<String, String>,
    restore: &EngineTrustRestore,
    platform: &str,
) {
    let names: &[&str] = if restore.contains_key("BUN_OPTIONS") {
        &ENGINE_TRUST_RESTORE_NAMES
    } else {
        &ALWAYS_RESTORED_NAMES
    };
    for name in names {
        match restore.get(*name) {
            Some(Some(value)) => set_name(env, name, value, platform),
            _ => delete_name(env, name, platform),
        }
    }
    delete_name(env, ENGINE_TRUST_MARKER_ENV, platform);
    delete_name(env, ENGINE_TRUST_HOP_ENV, platform);
}

static ADOPTED: LazyLock<Mutex<Option<EngineTrustMarker>>> = LazyLock::new(Default::default);

pub fn record_adopted_engine_trust(marker: EngineTrustMarker) {
    *ADOPTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(marker);
}

pub fn clear_adopted_engine_trust() {
    *ADOPTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

pub fn adopted_engine_trust() -> Option<EngineTrustMarker> {
    ADOPTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

pub fn engine_trust_hand_off_environment(
    marker: &EngineTrustMarker,
    platform: &str,
) -> Result<BTreeMap<String, String>, EngineTrustContractError> {
    let mut cleared = marker.clone();
    cleared.restore.clear();
    build_trust_launch_environment(
        &BTreeMap::new(),
        TrustLaunchOptions {
            marker: &cleared,
            hop: false,
            platform,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_marker() -> EngineTrustMarker {
        EngineTrustMarker {
            v: 1,
            file: "/home/u/.omo/engine-trust/aaaa.pem".into(),
            sha256: "a".repeat(64),
            automatic: "os".into(),
            fallback_reason: None,
            owner: "server".into(),
            user: None,
            defaults: Some(EngineTrustDefaults {
                file: "/home/u/.omo/engine-trust/defaults/empty.pem".into(),
                dir: "/home/u/.omo/engine-trust/defaults/empty.d".into(),
            }),
            restore: BTreeMap::from([
                ("NODE_USE_SYSTEM_CA".to_owned(), Some("1".to_owned())),
                ("SSL_CERT_FILE".to_owned(), None),
                ("BUN_OPTIONS".to_owned(), Some("--smol".to_owned())),
            ]),
        }
    }

    fn standalone_marker() -> EngineTrustMarker {
        EngineTrustMarker {
            automatic: "fallback".into(),
            fallback_reason: Some("helper-timeout".into()),
            owner: "standalone".into(),
            user: Some("b".repeat(64)),
            restore: BTreeMap::new(),
            ..server_marker()
        }
    }

    #[test]
    fn marker_version_1_round_trips_and_rejects_invalid_variants() {
        let server = server_marker();
        let text = serialize_engine_trust_marker(&server).expect("serialize");
        assert!(!text.contains('\n'));
        assert_eq!(parse_engine_trust_marker(&text), Some(server.clone()));

        let mut opt_in = standalone_marker();
        opt_in.defaults = None;
        let opt_text = serialize_engine_trust_marker(&opt_in).expect("serialize standalone");
        assert_eq!(parse_engine_trust_marker(&opt_text), Some(opt_in));

        assert!(parse_engine_trust_marker("{").is_none());
        assert!(parse_engine_trust_marker("[]").is_none());

        let mut bad = server;
        bad.defaults = None;
        assert!(serialize_engine_trust_marker(&bad).is_err());
    }

    #[test]
    fn classify_build_and_restore_handle_windows_case_forms_and_handoff() {
        let server = server_marker();
        let marker = serialize_engine_trust_marker(&server).expect("serialize");
        let lower = BTreeMap::from([
            ("Senpi_Engine_Trust".to_owned(), marker.clone()),
            ("node_extra_ca_certs".to_owned(), server.file.clone()),
        ]);
        assert_eq!(
            classify_trust_arrival(&lower, "win32").marker,
            TrustMarkerArrival::Addressed(server.clone())
        );
        assert_eq!(
            classify_trust_arrival(&lower, "linux").marker,
            TrustMarkerArrival::None
        );

        let mut two_files = lower.clone();
        two_files.insert("NODE_EXTRA_CA_CERTS".to_owned(), "/other.pem".to_owned());
        assert_eq!(
            classify_trust_arrival(&two_files, "win32").marker,
            TrustMarkerArrival::Foreign("ambiguous")
        );

        let handoff = engine_trust_hand_off_environment(&server, "linux").expect("handoff");
        let mut expected = server.clone();
        expected.restore.clear();
        assert_eq!(
            classify_trust_arrival(&handoff, "linux"),
            TrustArrival {
                hop: false,
                marker: TrustMarkerArrival::Addressed(expected),
            }
        );

        let mut env = BTreeMap::from([
            ("PATH".to_owned(), "/bin".to_owned()),
            ("NODE_EXTRA_CA_CERTS".to_owned(), server.file.clone()),
            ("NODE_USE_SYSTEM_CA".to_owned(), "ambient".to_owned()),
            ("SSL_CERT_FILE".to_owned(), "/closure.pem".to_owned()),
            ("SSL_CERT_DIR".to_owned(), "/closure.d".to_owned()),
            ("BUN_OPTIONS".to_owned(), "kept".to_owned()),
            ("SENPI_ENGINE_TRUST".to_owned(), "{}".to_owned()),
            ("SENPI_ENGINE_TRUST_HOP".to_owned(), "1".to_owned()),
        ]);
        apply_engine_trust_restore(
            &mut env,
            &BTreeMap::from([
                (
                    "NODE_EXTRA_CA_CERTS".to_owned(),
                    Some("/user.pem".to_owned()),
                ),
                ("SSL_CERT_FILE".to_owned(), None),
            ]),
            "linux",
        );
        assert_eq!(
            env,
            BTreeMap::from([
                ("PATH".to_owned(), "/bin".to_owned()),
                ("NODE_EXTRA_CA_CERTS".to_owned(), "/user.pem".to_owned()),
                ("BUN_OPTIONS".to_owned(), "kept".to_owned()),
            ])
        );
    }
}
