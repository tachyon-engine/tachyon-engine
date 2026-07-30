//! Symbol identity and Agent registry capacity tuning.

/// Entries reserved whenever the Agent's global Symbol registry exhausts capacity.
pub(crate) const REGISTRY_CAPACITY_GROWTH: usize = 8;
