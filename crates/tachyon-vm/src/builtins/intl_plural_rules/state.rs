//! Traced pending state for resumable PluralRules construction and selection.

use super::*;

/// GC-managed state retained across locale, option, and numeric conversion callbacks.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingIntlPluralRules {
    pub(super) new_target: Value,
    pub(super) options: Value,
    pub(super) locales: Value,
    pub(super) minimum_fraction_raw: Value,
    pub(super) maximum_fraction_raw: Value,
    pub(super) minimum_significant_raw: Value,
    pub(super) maximum_significant_raw: Value,
    pub(super) locale_matcher: IntlLocaleMatcher,
    pub(super) rule_type: IntlPluralRuleType,
    pub(super) notation: IntlNumberFormatNotation,
    pub(super) compact_display: IntlNumberFormatCompactDisplay,
    pub(super) minimum_integer_digits: u8,
    pub(super) minimum_fraction_digits: Option<u8>,
    pub(super) maximum_fraction_digits: Option<u8>,
    pub(super) minimum_significant_digits: Option<u8>,
    pub(super) maximum_significant_digits: Option<u8>,
    pub(super) rounding_increment: u16,
    pub(super) rounding_mode: IntlNumberFormatRoundingMode,
    pub(super) rounding_priority: IntlNumberFormatRoundingPriority,
    pub(super) trailing_zero_display: IntlNumberFormatTrailingZeroDisplay,
    pub(super) need_fraction: bool,
    pub(super) need_significant: bool,
    pub(super) stage: IntlPluralRulesStage,
    pub(super) supported_locales: bool,
}

impl Trace for PendingIntlPluralRules {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.new_target.trace(tracer);
        self.options.trace(tracer);
        self.locales.trace(tracer);
        self.minimum_fraction_raw.trace(tracer);
        self.maximum_fraction_raw.trace(tracer);
        self.minimum_significant_raw.trace(tracer);
        self.maximum_significant_raw.trace(tracer);
    }
}

struct PendingIntlPluralRulesRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingIntlPluralRules,
}

impl Trace for PendingIntlPluralRulesRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl PendingIntlPluralRules {
    /// Creates the scalar defaults used before any observable option read.
    pub(super) fn new(new_target: Value, options: Value, supported_locales: bool) -> Self {
        Self {
            new_target,
            options,
            locales: UNDEFINED,
            minimum_fraction_raw: UNDEFINED,
            maximum_fraction_raw: UNDEFINED,
            minimum_significant_raw: UNDEFINED,
            maximum_significant_raw: UNDEFINED,
            locale_matcher: IntlLocaleMatcher::BestFit,
            rule_type: IntlPluralRuleType::Cardinal,
            notation: IntlNumberFormatNotation::Standard,
            compact_display: IntlNumberFormatCompactDisplay::Short,
            minimum_integer_digits: 1,
            minimum_fraction_digits: None,
            maximum_fraction_digits: None,
            minimum_significant_digits: None,
            maximum_significant_digits: None,
            rounding_increment: 1,
            rounding_mode: IntlNumberFormatRoundingMode::HalfExpand,
            rounding_priority: IntlNumberFormatRoundingPriority::Auto,
            trailing_zero_display: IntlNumberFormatTrailingZeroDisplay::Auto,
            need_fraction: true,
            need_significant: false,
            stage: IntlPluralRulesStage::Locales,
            supported_locales,
        }
    }
}

impl Isolate {
    /// Allocates compact pending state under a root set containing every managed Value.
    pub(super) fn allocate_pending_intl_plural_rules(
        &mut self,
        pending: PendingIntlPluralRules,
    ) -> Result<GcRef<PendingIntlPluralRules>, ExecutionError> {
        let mut roots = PendingIntlPluralRulesRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                inactive_realms: &mut self.inactive_realms,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            pending,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_intl_plural_rules,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_intl_plural_rules_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingIntlPluralRules>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_intl_plural_rules)
            .map_err(ExecutionError::HeapReference)
    }

    pub(super) fn pending_intl_plural_rules_snapshot(
        &mut self,
        state: GcRef<PendingIntlPluralRules>,
    ) -> Result<PendingIntlPluralRules, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_intl_plural_rules)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(crate) fn pending_intl_plural_rules_stage(
        &mut self,
        state: GcRef<PendingIntlPluralRules>,
    ) -> Result<IntlPluralRulesStage, ExecutionError> {
        self.pending_intl_plural_rules_snapshot(state)
            .map(|pending| pending.stage)
    }

    pub(super) fn update_pending_intl_plural_rules(
        &mut self,
        state: GcRef<PendingIntlPluralRules>,
        update: impl FnOnce(&mut PendingIntlPluralRules),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                update(
                    no_gc
                        .borrow_mut(state, self.types.pending_intl_plural_rules)
                        .map_err(ExecutionError::NoGcBorrow)?,
                );
                Ok(())
            })
        })
    }

    pub(super) fn set_pending_intl_plural_rules_value(
        &mut self,
        state: GcRef<PendingIntlPluralRules>,
        select: impl FnOnce(&mut PendingIntlPluralRules) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                *select(
                    no_gc
                        .borrow_mut(state, self.types.pending_intl_plural_rules)
                        .map_err(ExecutionError::NoGcBorrow)?,
                ) = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    pub(super) fn set_pending_intl_plural_rules_stage(
        &mut self,
        state: GcRef<PendingIntlPluralRules>,
        stage: IntlPluralRulesStage,
    ) -> Result<(), ExecutionError> {
        self.update_pending_intl_plural_rules(state, |pending| pending.stage = stage)
    }
}
