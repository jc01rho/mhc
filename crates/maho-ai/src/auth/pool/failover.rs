pub use crate::utils::provider_failure_description::TURN_RETRY_SUPPRESSION_PREFIX;

pub const DEFAULT_SLOT_BLOCK_MS: f64 = 60_000.0;
pub const MAX_SLOT_BLOCK_MS: f64 = 48.0 * 60.0 * 60.0 * 1_000.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_slot_block_bounds_and_suppression_prefix() {
        assert_eq!(DEFAULT_SLOT_BLOCK_MS, 60_000.0);
        assert_eq!(MAX_SLOT_BLOCK_MS, 48.0 * 60.0 * 60.0 * 1_000.0);
        assert_eq!(TURN_RETRY_SUPPRESSION_PREFIX, "senpi:no-turn-retry:");
    }
}
