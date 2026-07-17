//! Isolate-local allocation-debt and memory-pressure collection policy.

use crate::{AllocationSpace, HeapLimit};

use crate::tuning::{
    DEFAULT_HEAP_PRESSURE_PERCENT, DEFAULT_MAJOR_ALLOCATION_DEBT_BYTES,
    DEFAULT_YOUNG_ALLOCATION_DEBT_BYTES,
};

/// Rejects trigger thresholds that would collect continuously or exceed a percentage boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GcTriggerConfigError {
    ZeroYoungAllocationDebt,
    ZeroMajorAllocationDebt,
    InvalidHeapPressurePercent { percent: u8 },
}

/// Host policy for byte-debt and committed-storage collection triggers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GcTriggerConfig {
    young_allocation_debt_bytes: usize,
    major_allocation_debt_bytes: usize,
    heap_pressure_percent: u8,
}

impl GcTriggerConfig {
    pub(crate) const DEFAULT: Self = Self {
        young_allocation_debt_bytes: DEFAULT_YOUNG_ALLOCATION_DEBT_BYTES,
        major_allocation_debt_bytes: DEFAULT_MAJOR_ALLOCATION_DEBT_BYTES,
        heap_pressure_percent: DEFAULT_HEAP_PRESSURE_PERCENT,
    };

    /// Validates all policy knobs before they enter an isolate-local heap.
    pub const fn new(
        young_allocation_debt_bytes: usize,
        major_allocation_debt_bytes: usize,
        heap_pressure_percent: u8,
    ) -> Result<Self, GcTriggerConfigError> {
        if young_allocation_debt_bytes == 0 {
            return Err(GcTriggerConfigError::ZeroYoungAllocationDebt);
        }
        if major_allocation_debt_bytes == 0 {
            return Err(GcTriggerConfigError::ZeroMajorAllocationDebt);
        }
        if heap_pressure_percent == 0 || heap_pressure_percent > 100 {
            return Err(GcTriggerConfigError::InvalidHeapPressurePercent {
                percent: heap_pressure_percent,
            });
        }
        Ok(Self {
            young_allocation_debt_bytes,
            major_allocation_debt_bytes,
            heap_pressure_percent,
        })
    }

    #[must_use]
    pub const fn young_allocation_debt_bytes(self) -> usize {
        self.young_allocation_debt_bytes
    }

    #[must_use]
    pub const fn major_allocation_debt_bytes(self) -> usize {
        self.major_allocation_debt_bytes
    }

    #[must_use]
    pub const fn heap_pressure_percent(self) -> u8 {
        self.heap_pressure_percent
    }
}

impl Default for GcTriggerConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Deterministic stress mode applied at managed allocation safepoints.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ForcedCollectionMode {
    #[default]
    None,
    Minor,
    Major,
}

/// Collection selected before a managed allocation publishes its pending object.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CollectionAction {
    #[default]
    None,
    Minor,
    Major,
}

/// The highest-priority input responsible for a managed collection attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionReason {
    Forced,
    MemoryPressure,
    HeapLimit,
    HeapPressure,
    YoungAllocationDebt,
    OldAllocationDebt,
}

/// Allocation and collection evidence retained without atomics or time sampling.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GcTriggerStats {
    pub young_allocated_bytes: usize,
    pub old_allocated_bytes: usize,
    pub young_debt_bytes: usize,
    pub old_debt_bytes: usize,
    pub minor_attempts: usize,
    pub major_attempts: usize,
    pub minor_successes: usize,
    pub major_successes: usize,
    pub memory_pressure_requests: usize,
    pub memory_pressure_commands_consumed: usize,
    pub heap_limit_attempts: usize,
    pub heap_pressure_attempts: usize,
    pub forced_attempts: usize,
    pub young_debt_attempts: usize,
    pub old_debt_attempts: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CollectionDecision {
    pub action: CollectionAction,
    pub reason: CollectionReason,
}

pub(crate) struct GcTrigger {
    config: GcTriggerConfig,
    forced_mode: ForcedCollectionMode,
    memory_pressure_pending: bool,
    stats: GcTriggerStats,
}

impl GcTrigger {
    /// Installs validated policy with zero debt and no pending host command or forced mode.
    pub const fn new(config: GcTriggerConfig) -> Self {
        Self {
            config,
            forced_mode: ForcedCollectionMode::None,
            memory_pressure_pending: false,
            stats: GcTriggerStats {
                young_allocated_bytes: 0,
                old_allocated_bytes: 0,
                young_debt_bytes: 0,
                old_debt_bytes: 0,
                minor_attempts: 0,
                major_attempts: 0,
                minor_successes: 0,
                major_successes: 0,
                memory_pressure_requests: 0,
                memory_pressure_commands_consumed: 0,
                heap_limit_attempts: 0,
                heap_pressure_attempts: 0,
                forced_attempts: 0,
                young_debt_attempts: 0,
                old_debt_attempts: 0,
            },
        }
    }

    pub const fn config(&self) -> GcTriggerConfig {
        self.config
    }

    pub const fn forced_mode(&self) -> ForcedCollectionMode {
        self.forced_mode
    }

    pub fn set_forced_mode(&mut self, mode: ForcedCollectionMode) {
        self.forced_mode = mode;
    }

    pub fn request_memory_pressure(&mut self) {
        self.memory_pressure_pending = true;
        self.stats.memory_pressure_requests = self.stats.memory_pressure_requests.saturating_add(1);
    }

    pub const fn stats(&self) -> GcTriggerStats {
        self.stats
    }

    /// Selects one bounded action; pressure checks only fire when allocation needs new storage.
    pub fn decide(
        &self,
        space: AllocationSpace,
        allocation_bytes: usize,
        required_storage_bytes: usize,
        committed_bytes: usize,
        limit: HeapLimit,
    ) -> Option<CollectionDecision> {
        if self.forced_mode == ForcedCollectionMode::Major {
            return Some(CollectionDecision {
                action: CollectionAction::Major,
                reason: CollectionReason::Forced,
            });
        }
        if self.forced_mode == ForcedCollectionMode::Minor && space == AllocationSpace::Young {
            return Some(CollectionDecision {
                action: CollectionAction::Minor,
                reason: CollectionReason::Forced,
            });
        }
        if self.memory_pressure_pending {
            return Some(CollectionDecision {
                action: CollectionAction::Major,
                reason: CollectionReason::MemoryPressure,
            });
        }

        let prospective_storage = committed_bytes.saturating_add(required_storage_bytes);
        if prospective_storage > limit.max_heap_bytes() {
            return Some(CollectionDecision {
                action: CollectionAction::Major,
                reason: CollectionReason::HeapLimit,
            });
        }
        let pressure_threshold =
            percentage_threshold(limit.max_heap_bytes(), self.config.heap_pressure_percent);
        if committed_bytes != 0
            && required_storage_bytes != 0
            && prospective_storage >= pressure_threshold
        {
            return Some(CollectionDecision {
                action: CollectionAction::Major,
                reason: CollectionReason::HeapPressure,
            });
        }

        match space {
            AllocationSpace::Young
                if self.stats.young_debt_bytes.saturating_add(allocation_bytes)
                    >= self.config.young_allocation_debt_bytes =>
            {
                Some(CollectionDecision {
                    action: CollectionAction::Minor,
                    reason: CollectionReason::YoungAllocationDebt,
                })
            }
            AllocationSpace::Old
                if self.stats.old_debt_bytes.saturating_add(allocation_bytes)
                    >= self.config.major_allocation_debt_bytes =>
            {
                Some(CollectionDecision {
                    action: CollectionAction::Major,
                    reason: CollectionReason::OldAllocationDebt,
                })
            }
            AllocationSpace::Young | AllocationSpace::Old => None,
        }
    }

    /// Consumes one-shot pressure and attributes a selected managed collection attempt.
    pub fn record_attempt(&mut self, decision: CollectionDecision) {
        if self.memory_pressure_pending {
            self.memory_pressure_pending = false;
            self.stats.memory_pressure_commands_consumed = self
                .stats
                .memory_pressure_commands_consumed
                .saturating_add(1);
        }
        match decision.action {
            CollectionAction::None => {}
            CollectionAction::Minor => {
                self.stats.minor_attempts = self.stats.minor_attempts.saturating_add(1);
            }
            CollectionAction::Major => {
                self.stats.major_attempts = self.stats.major_attempts.saturating_add(1);
                if decision.reason == CollectionReason::HeapLimit {
                    self.stats.heap_limit_attempts =
                        self.stats.heap_limit_attempts.saturating_add(1);
                }
            }
        }
        match decision.reason {
            CollectionReason::Forced => {
                self.stats.forced_attempts = self.stats.forced_attempts.saturating_add(1);
            }
            CollectionReason::MemoryPressure | CollectionReason::HeapLimit => {}
            CollectionReason::HeapPressure => {
                self.stats.heap_pressure_attempts =
                    self.stats.heap_pressure_attempts.saturating_add(1);
            }
            CollectionReason::YoungAllocationDebt => {
                self.stats.young_debt_attempts = self.stats.young_debt_attempts.saturating_add(1);
            }
            CollectionReason::OldAllocationDebt => {
                self.stats.old_debt_attempts = self.stats.old_debt_attempts.saturating_add(1);
            }
        }
    }

    /// Repays generation debt only after the corresponding collection phase completes.
    pub fn record_collection_success(&mut self, action: CollectionAction) {
        match action {
            CollectionAction::None => {}
            CollectionAction::Minor => {
                self.stats.minor_successes = self.stats.minor_successes.saturating_add(1);
                self.stats.young_debt_bytes = 0;
            }
            CollectionAction::Major => {
                self.stats.major_successes = self.stats.major_successes.saturating_add(1);
                self.stats.young_debt_bytes = 0;
                self.stats.old_debt_bytes = 0;
            }
        }
    }

    /// Charges successful raw or managed publication to cumulative bytes and current debt.
    pub fn record_allocation(&mut self, space: AllocationSpace, bytes: usize) {
        match space {
            AllocationSpace::Young => {
                self.stats.young_allocated_bytes =
                    self.stats.young_allocated_bytes.saturating_add(bytes);
                self.stats.young_debt_bytes = self.stats.young_debt_bytes.saturating_add(bytes);
            }
            AllocationSpace::Old => {
                self.stats.old_allocated_bytes =
                    self.stats.old_allocated_bytes.saturating_add(bytes);
                self.stats.old_debt_bytes = self.stats.old_debt_bytes.saturating_add(bytes);
            }
        }
    }
}

/// Computes `ceil(bytes * percent / 100)` without overflowing the multiplication.
#[inline]
fn percentage_threshold(bytes: usize, percent: u8) -> usize {
    let percent = usize::from(percent);
    let whole = (bytes / 100).saturating_mul(percent);
    let remainder = (bytes % 100).saturating_mul(percent).div_ceil(100);
    whole.saturating_add(remainder)
}

#[cfg(test)]
mod tests {
    use super::{GcTriggerConfig, GcTriggerConfigError};

    #[test]
    fn trigger_configuration_rejects_continuous_or_invalid_thresholds() {
        assert_eq!(
            GcTriggerConfig::new(0, 1, 90),
            Err(GcTriggerConfigError::ZeroYoungAllocationDebt)
        );
        assert_eq!(
            GcTriggerConfig::new(1, 0, 90),
            Err(GcTriggerConfigError::ZeroMajorAllocationDebt)
        );
        assert_eq!(
            GcTriggerConfig::new(1, 1, 101),
            Err(GcTriggerConfigError::InvalidHeapPressurePercent { percent: 101 })
        );
    }
}
