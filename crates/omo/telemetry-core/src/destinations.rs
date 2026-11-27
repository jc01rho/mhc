//! Rust port of `packages/telemetry-core/src/destinations.ts`.

use crate::constants::DEFAULT_POSTHOG_HOST;
use crate::otlp_config::DEFAULT_OTLP_ENDPOINT;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryDestinationKind {
    Posthog,
    Otlp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryDestination {
    pub kind: TelemetryDestinationKind,
    pub label: &'static str,
    pub host: &'static str,
}

/// Every host the shipped senders talk to; the install disclosure and the first-run consent question name
/// all of them.
pub const TELEMETRY_DESTINATIONS: [TelemetryDestination; 2] = [
    TelemetryDestination {
        kind: TelemetryDestinationKind::Posthog,
        label: "PostHog in the United States",
        host: DEFAULT_POSTHOG_HOST,
    },
    TelemetryDestination {
        kind: TelemetryDestinationKind::Otlp,
        label: "the OmO telemetry collector",
        host: DEFAULT_OTLP_ENDPOINT,
    },
];

/// `PostHog in the United States (https://us.i.posthog.com) and the OmO telemetry collector (https://t.omo.dev)`.
pub fn telemetry_destinations_phrase() -> String {
    TELEMETRY_DESTINATIONS
        .iter()
        .map(|destination| format!("{} ({})", destination.label, destination.host))
        .collect::<Vec<_>>()
        .join(" and ")
}
