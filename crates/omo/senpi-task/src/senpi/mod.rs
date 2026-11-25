//! Host-runtime (senpi) value adapters (`senpi/` in TypeScript).
//!
//! `minimal-resource-loader.ts` builds a host `ResourceLoader` object; the host owns that shape, so
//! it lives behind the [`crate::host`] seam rather than here (see parity.md).

pub mod model_decorators;
mod thinking_level;

pub use model_decorators::{
    ModelDecorators, SENPI_SERVICE_TIERS, SENPI_THINKING_LEVELS, split_model_decorators,
};
pub use thinking_level::{SenpiThinkingLevel, as_senpi_thinking_level};

#[cfg(test)]
#[path = "senpi_tests.rs"]
mod tests;
