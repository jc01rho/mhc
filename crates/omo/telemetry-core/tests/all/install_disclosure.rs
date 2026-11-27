use std::path::Path;

use pretty_assertions::assert_eq;
use telemetry_core::TELEMETRY_DESTINATIONS;
use telemetry_core::telemetry_install_disclosure_lines;

fn shipped(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("get-worker")
        .join("assets")
        .join(name);
    std::fs::read_to_string(&path).unwrap()
}

#[test]
fn install_sh_carries_every_disclosure_line() {
    let script = shipped("install.sh");
    for line in telemetry_install_disclosure_lines() {
        assert!(script.contains(&format!("say \"{line}\"")), "{line}");
    }
}

#[test]
fn install_ps1_carries_every_disclosure_line() {
    let script = shipped("install.ps1");
    for line in telemetry_install_disclosure_lines() {
        assert!(script.contains(&format!("Write-OmoSay '{line}'")), "{line}");
    }
}

#[test]
fn the_disclosure_names_every_destination_host() {
    let joined = telemetry_install_disclosure_lines().join("\n");
    for destination in TELEMETRY_DESTINATIONS {
        assert!(joined.contains(destination.host), "{}", destination.host);
    }
    assert_eq!(TELEMETRY_DESTINATIONS.len(), 2);
}
