//! Port of `packages/omo-senpi/src/components/telemetry/omo-native-consent.ts`: the persisted first-run
//! telemetry answer. (`omo_native_consent.rs` is the older port of the session-side consent cache.)

use std::io::Write;
use std::path::Path;

use serde_json::{Value, json};

pub const CONSENT_MARKER_FILE_NAME: &str = "consent-answer";
const CONSENT_MARKER_VERSION: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsentAnswer {
    Accepted,
    Declined,
}

impl ConsentAnswer {
    fn as_str(self) -> &'static str {
        match self {
            ConsentAnswer::Accepted => "accepted",
            ConsentAnswer::Declined => "declined",
        }
    }
}

#[cfg(unix)]
fn private_file(options: &mut std::fs::OpenOptions) -> &mut std::fs::OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600)
}

#[cfg(not(unix))]
fn private_file(options: &mut std::fs::OpenOptions) -> &mut std::fs::OpenOptions {
    options
}

/// Telemetry is allowed only when this reads `Accepted`. An absent, unreadable, corrupt or future-version
/// marker is "not consented", and a run with no UI to ask in never writes one.
pub fn read_omo_native_consent(state_dir: &Path) -> Option<ConsentAnswer> {
    let text = std::fs::read_to_string(state_dir.join(CONSENT_MARKER_FILE_NAME)).ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    if parsed.get("v").and_then(Value::as_u64) != Some(CONSENT_MARKER_VERSION) {
        return None;
    }
    match parsed.get("answer").and_then(Value::as_str) {
        Some("accepted") => Some(ConsentAnswer::Accepted),
        Some("declined") => Some(ConsentAnswer::Declined),
        _ => None,
    }
}

pub fn is_omo_native_consent_accepted(state_dir: &Path) -> bool {
    read_omo_native_consent(state_dir) == Some(ConsentAnswer::Accepted)
}

/// The marker is created exclusively, so the first answer wins and a concurrent asker loses the race cleanly:
/// `Ok(true)` when this call created it, `Ok(false)` when an answer already existed.
pub fn record_omo_native_consent(state_dir: &Path, answer: ConsentAnswer) -> std::io::Result<bool> {
    std::fs::create_dir_all(state_dir)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    match private_file(&mut options).open(state_dir.join(CONSENT_MARKER_FILE_NAME)) {
        Ok(mut file) => {
            let marker = json!({ "v": CONSENT_MARKER_VERSION, "answer": answer.as_str() });
            writeln!(file, "{marker}")?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::SenpiTelemetryOptions;
    use crate::omo_native_session::is_omo_native_telemetry_enabled;
    use std::sync::Arc;

    #[test]
    fn a_recorded_accept_survives_a_read_back() {
        let dir = tempfile::tempdir().unwrap();
        assert!(record_omo_native_consent(dir.path(), ConsentAnswer::Accepted).unwrap());
        assert_eq!(
            read_omo_native_consent(dir.path()),
            Some(ConsentAnswer::Accepted)
        );
        assert!(is_omo_native_consent_accepted(dir.path()));
    }

    #[test]
    fn a_recorded_decline_is_not_consent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(record_omo_native_consent(dir.path(), ConsentAnswer::Declined).unwrap());
        assert_eq!(
            read_omo_native_consent(dir.path()),
            Some(ConsentAnswer::Declined)
        );
        assert!(!is_omo_native_consent_accepted(dir.path()));
    }

    #[test]
    fn no_marker_means_nothing_is_consented() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_omo_native_consent(dir.path()), None);
        assert!(!is_omo_native_consent_accepted(dir.path()));
    }

    #[test]
    fn a_corrupt_or_future_version_marker_is_not_consent() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(CONSENT_MARKER_FILE_NAME);
        for body in [
            "{not json",
            r#"{"v":99,"answer":"accepted"}"#,
            r#"{"v":1,"answer":"maybe"}"#,
            r#"["accepted"]"#,
        ] {
            std::fs::write(&marker, body).unwrap();
            assert_eq!(read_omo_native_consent(dir.path()), None, "{body}");
        }
    }

    #[test]
    fn the_first_answer_wins() {
        let dir = tempfile::tempdir().unwrap();
        assert!(record_omo_native_consent(dir.path(), ConsentAnswer::Declined).unwrap());
        assert!(!record_omo_native_consent(dir.path(), ConsentAnswer::Accepted).unwrap());
        assert_eq!(
            read_omo_native_consent(dir.path()),
            Some(ConsentAnswer::Declined)
        );
    }

    #[test]
    fn a_blocked_state_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, "").unwrap();
        assert!(record_omo_native_consent(&blocked, ConsentAnswer::Accepted).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_marker_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        record_omo_native_consent(dir.path(), ConsentAnswer::Accepted).unwrap();
        let mode = std::fs::metadata(dir.path().join(CONSENT_MARKER_FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    fn gate_options(state_dir: &Path) -> SenpiTelemetryOptions {
        SenpiTelemetryOptions {
            env: Some(
                [
                    ("POSTHOG_API_KEY".to_string(), "test-api-key".to_string()),
                    (
                        "OMO_TELEMETRY_ALLOW_AUTOMATION".to_string(),
                        "1".to_string(),
                    ),
                ]
                .into_iter()
                .collect(),
            ),
            state_dir: Some(state_dir.to_path_buf()),
            is_config_enabled: Some(Arc::new(|_| true)),
            ..Default::default()
        }
    }

    fn gate_open(options: &SenpiTelemetryOptions) -> bool {
        let env = options.env.clone().unwrap();
        is_omo_native_telemetry_enabled(options, None, &env)
    }

    #[test]
    fn the_enablement_gate_is_off_until_the_first_run_answer_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let options = gate_options(dir.path());
        assert!(!gate_open(&options));
        record_omo_native_consent(dir.path(), ConsentAnswer::Accepted).unwrap();
        assert!(gate_open(&options));
    }

    #[test]
    fn a_declined_answer_keeps_the_enablement_gate_off() {
        let dir = tempfile::tempdir().unwrap();
        let options = gate_options(dir.path());
        record_omo_native_consent(dir.path(), ConsentAnswer::Declined).unwrap();
        assert!(!gate_open(&options));
    }
}
