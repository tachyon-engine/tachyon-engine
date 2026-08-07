//! Traced pending state for resumable RelativeTimeFormat work.

use super::*;

/// GC-managed state retained across locale, option, and argument conversions.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingIntlRelativeTimeFormat {
    pub(super) new_target: Value,
    pub(super) options: Value,
    pub(super) locales: Value,
    pub(super) numbering_system: Value,
    pub(super) value: Value,
    pub(super) unit: Value,
    pub(super) receiver: Value,
    pub(super) locale_matcher: IntlLocaleMatcher,
    pub(super) style: IntlRelativeTimeFormatStyle,
    pub(super) numeric: IntlRelativeTimeFormatNumeric,
    pub(super) stage: IntlRelativeTimeFormatStage,
    pub(super) supported_locales: bool,
}

impl Trace for PendingIntlRelativeTimeFormat {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.new_target.trace(tracer);
        self.options.trace(tracer);
        self.locales.trace(tracer);
        self.numbering_system.trace(tracer);
        self.value.trace(tracer);
        self.unit.trace(tracer);
        self.receiver.trace(tracer);
    }
}

struct PendingIntlRelativeTimeFormatRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingIntlRelativeTimeFormat,
}

impl Trace for PendingIntlRelativeTimeFormatRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl PendingIntlRelativeTimeFormat {
    /// Creates specification defaults before any observable option read.
    pub(super) fn new(new_target: Value, options: Value, supported_locales: bool) -> Self {
        Self {
            new_target,
            options,
            locales: UNDEFINED,
            numbering_system: UNDEFINED,
            value: UNDEFINED,
            unit: UNDEFINED,
            receiver: UNDEFINED,
            locale_matcher: IntlLocaleMatcher::BestFit,
            style: IntlRelativeTimeFormatStyle::Long,
            numeric: IntlRelativeTimeFormatNumeric::Always,
            stage: IntlRelativeTimeFormatStage::Locales,
            supported_locales,
        }
    }
}

impl Isolate {
    /// Allocates pending state under a root set containing every managed Value.
    pub(super) fn allocate_pending_intl_relative_time_format(
        &mut self,
        pending: PendingIntlRelativeTimeFormat,
    ) -> Result<GcRef<PendingIntlRelativeTimeFormat>, ExecutionError> {
        let mut roots = PendingIntlRelativeTimeFormatRoots {
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
                self.types.pending_intl_relative_time_format,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_intl_relative_time_format_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingIntlRelativeTimeFormat>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_intl_relative_time_format)
            .map_err(ExecutionError::HeapReference)
    }

    pub(super) fn pending_intl_relative_time_format_snapshot(
        &mut self,
        state: GcRef<PendingIntlRelativeTimeFormat>,
    ) -> Result<PendingIntlRelativeTimeFormat, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_intl_relative_time_format)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(crate) fn pending_intl_relative_time_format_stage(
        &mut self,
        state: GcRef<PendingIntlRelativeTimeFormat>,
    ) -> Result<IntlRelativeTimeFormatStage, ExecutionError> {
        self.pending_intl_relative_time_format_snapshot(state)
            .map(|pending| pending.stage)
    }

    pub(super) fn update_pending_intl_relative_time_format(
        &mut self,
        state: GcRef<PendingIntlRelativeTimeFormat>,
        update: impl FnOnce(&mut PendingIntlRelativeTimeFormat),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                update(
                    no_gc
                        .borrow_mut(state, self.types.pending_intl_relative_time_format)
                        .map_err(ExecutionError::NoGcBorrow)?,
                );
                Ok(())
            })
        })
    }

    /// Replaces one managed slot and records its generational edge.
    pub(super) fn set_pending_intl_relative_time_format_value(
        &mut self,
        state: GcRef<PendingIntlRelativeTimeFormat>,
        select: impl FnOnce(&mut PendingIntlRelativeTimeFormat) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                *select(
                    no_gc
                        .borrow_mut(state, self.types.pending_intl_relative_time_format)
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
}
