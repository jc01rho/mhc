//! Port of omo-cpa (senpi extension, package `omo-cpa`) to a native Rust extension.
//!
//! One TS file maps to one module, keeping the upstream names and order:
//!
//! | omo-cpa `src/*.ts` | this crate            |
//! |--------------------|-----------------------|
//! | `types.ts`         | [`types`]             |
//! | `tier-types.ts`    | [`tier_types`]        |
//! | `redact.ts`        | [`redact`]            |
//! | `management-key.ts`| [`management_key`]    |
//! | `config.ts`        | [`config`]            |
//! | `health.ts`        | [`health`]            |
//! | `catalog.ts`       | [`catalog`]           |
//! | `endpoint.ts`      | [`endpoint`]          |
//! | `tier.ts`          | [`tier`]              |
//! | `chain.ts`         | [`chain`]             |
//! | `pricing.ts`       | [`pricing`]           |
//! | `usage.ts`         | [`usage`]             |
//! | `render.ts`        | [`render`]            |
//! | `provider-core.ts` | [`provider_core`]     |
//! | `provider.ts`      | [`provider`]          |
//! | `extension.ts`     | [`extension`]         |
//!
//! `cli.ts` is a standalone operator CLI that `extension.ts` never imports, so it is intentionally
//! not ported.

pub mod catalog;
pub mod chain;
pub mod config;
pub mod endpoint;
pub mod extension;
pub mod health;
pub mod management_key;
pub mod pricing;
pub mod provider;
pub mod provider_core;
pub mod redact;
pub mod render;
pub mod tier;
pub mod tier_types;
pub mod types;
pub mod usage;

pub use extension::{
    COMMAND_ARGUMENT_HINT, COMMAND_DESCRIPTION, COMMAND_NAME, HandlerEnv, OmoCpaExtension,
    OmoCpaOptions, State, handle_cpa_command, register_omo_cpa,
};
pub use provider::{LAST_RESORT_PROVIDER_NAME, PROVIDER_NAME, ProviderRuntime};

use maho_ext_api::{Extension, ExtensionApi};

/// The `omo-cpa` extension, statically registered by `mhc`.
pub struct OmoCpa;

impl Extension for OmoCpa {
    fn register(&self, api: &mut ExtensionApi) {
        register_omo_cpa(api, OmoCpaOptions::default(), ProviderRuntime::default());
    }
}
