//! Port of `device-service-core/src/state-dir.ts`: where the device service's endpoint and per-boot
//! token live.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Set by the device service on every process it launches: the absolute state dir it runs with.
pub const DEVICE_SERVICE_STATE_DIR_ENV: &str = "OMO_DEVICE_SERVICE_STATE_DIR";

/// Maintained beside the default state dir: `{"v":1,"state_dir":"<absolute>"}`.
pub const LOCATION_FILE: &str = "device-service.location.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Posix,
    Windows,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

fn is_absolute_on(platform: Platform, path: &str) -> bool {
    match platform {
        Platform::Posix => path.starts_with('/'),
        Platform::Windows => {
            let bytes = path.as_bytes();
            path.starts_with('\\')
                || path.starts_with('/')
                || (bytes.len() >= 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && (bytes[2] == b'\\' || bytes[2] == b'/'))
        }
    }
}

fn default_state_dir(
    platform: Platform,
    env: &dyn Fn(&str) -> Option<String>,
    home: &Path,
) -> PathBuf {
    match platform {
        Platform::Posix => home.join(".omo").join("device-service"),
        Platform::Windows => env("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Local"))
            .join("OmO")
            .join("device-service"),
    }
}

// The contract is a silent fall-through: an unreadable, unparsable or dangling discovery file means
// the default.
fn discovered_state_dir(platform: Platform, default_dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(default_dir.parent()?.join(LOCATION_FILE)).ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    if parsed.get("v").and_then(Value::as_f64) != Some(1.0) {
        return None;
    }
    let state_dir = parsed.get("state_dir")?.as_str()?;
    if !is_absolute_on(platform, state_dir) {
        return None;
    }
    let path = PathBuf::from(state_dir);
    path.is_dir().then_some(path)
}

/// A non-empty absolute `OMO_DEVICE_SERVICE_STATE_DIR`, else the existing absolute directory named by
/// `device-service.location.json` beside the default dir, else the default `~/.omo/device-service`
/// (POSIX) or `%LOCALAPPDATA%\OmO\device-service` (Windows).
pub fn device_service_state_dir_with(
    platform: Platform,
    env: &dyn Fn(&str) -> Option<String>,
    home: &Path,
) -> PathBuf {
    if let Some(explicit) = env(DEVICE_SERVICE_STATE_DIR_ENV)
        .filter(|value| !value.is_empty() && is_absolute_on(platform, value))
    {
        return PathBuf::from(explicit);
    }
    let default_dir = default_state_dir(platform, env, home);
    discovered_state_dir(platform, &default_dir).unwrap_or(default_dir)
}

/// [`device_service_state_dir_with`] for this process.
pub fn device_service_state_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    device_service_state_dir_with(Platform::current(), &|key| std::env::var(key).ok(), &home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_then_discovered_then_default() {
        let home = tempfile::tempdir().unwrap();
        let none = |_: &str| None;
        assert_eq!(
            device_service_state_dir_with(Platform::Posix, &none, home.path()),
            home.path().join(".omo/device-service")
        );
        let elsewhere = home.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(home.path().join(".omo")).unwrap();
        std::fs::write(
            home.path().join(".omo").join(LOCATION_FILE),
            serde_json::json!({"v": 1, "state_dir": elsewhere}).to_string(),
        )
        .unwrap();
        assert_eq!(
            device_service_state_dir_with(Platform::Posix, &none, home.path()),
            elsewhere
        );
        let explicit =
            |key: &str| (key == DEVICE_SERVICE_STATE_DIR_ENV).then(|| "/explicit".to_owned());
        assert_eq!(
            device_service_state_dir_with(Platform::Posix, &explicit, home.path()),
            PathBuf::from("/explicit")
        );
        let relative =
            |key: &str| (key == DEVICE_SERVICE_STATE_DIR_ENV).then(|| "relative".to_owned());
        assert_eq!(
            device_service_state_dir_with(Platform::Posix, &relative, home.path()),
            elsewhere
        );
    }

    #[test]
    fn windows_uses_local_app_data() {
        let env =
            |key: &str| (key == "LOCALAPPDATA").then(|| "C:\\Users\\me\\AppData\\Local".to_owned());
        let dir = device_service_state_dir_with(Platform::Windows, &env, Path::new("/nowhere"));
        assert!(dir.ends_with(Path::new("OmO").join("device-service")));
        assert!(is_absolute_on(Platform::Windows, "C:\\x"));
        assert!(!is_absolute_on(Platform::Windows, "C:x"));
    }
}
