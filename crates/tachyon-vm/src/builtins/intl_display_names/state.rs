//! Traced pending state for resumable DisplayNames work.

use super::*;

/// GC-managed state retained across locale, option, and code conversions.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingIntlDisplayNames {
    pub(super) new_target: Value,
    pub(super) options: Value,
    pub(super) locales: Value,
    pub(super) requested_locales: Value,
    pub(super) prototype: Value,
    pub(super) code: Value,
    pub(super) receiver: Value,
    pub(super) locale_matcher: IntlLocaleMatcher,
    pub(super) style: IntlDisplayNamesStyle,
    pub(super) display_type: Option<IntlDisplayNamesType>,
    pub(super) fallback: IntlDisplayNamesFallback,
    pub(super) language_display: IntlDisplayNamesLanguageDisplay,
    pub(super) stage: IntlDisplayNamesStage,
    pub(super) supported_locales: bool,
}

impl Trace for PendingIntlDisplayNames {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.new_target.trace(tracer);
        self.options.trace(tracer);
        self.locales.trace(tracer);
        self.requested_locales.trace(tracer);
        self.prototype.trace(tracer);
        self.code.trace(tracer);
        self.receiver.trace(tracer);
    }
}

struct PendingIntlDisplayNamesRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingIntlDisplayNames,
}

impl Trace for PendingIntlDisplayNamesRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl PendingIntlDisplayNames {
    /// Creates specification defaults before any observable option read.
    pub(super) fn new(new_target: Value, options: Value, supported_locales: bool) -> Self {
        Self {
            new_target,
            options,
            locales: UNDEFINED,
            requested_locales: UNDEFINED,
            prototype: UNDEFINED,
            code: UNDEFINED,
            receiver: UNDEFINED,
            locale_matcher: IntlLocaleMatcher::BestFit,
            style: IntlDisplayNamesStyle::Long,
            display_type: None,
            fallback: IntlDisplayNamesFallback::Code,
            language_display: IntlDisplayNamesLanguageDisplay::Dialect,
            stage: IntlDisplayNamesStage::Locales,
            supported_locales,
        }
    }
}

impl Isolate {
    /// Allocates pending state under a root set containing every managed Value.
    pub(super) fn allocate_pending_intl_display_names(
        &mut self,
        pending: PendingIntlDisplayNames,
    ) -> Result<GcRef<PendingIntlDisplayNames>, ExecutionError> {
        let mut roots = PendingIntlDisplayNamesRoots {
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
                self.types.pending_intl_display_names,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_intl_display_names_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingIntlDisplayNames>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_intl_display_names)
            .map_err(ExecutionError::HeapReference)
    }

    pub(super) fn pending_intl_display_names_snapshot(
        &mut self,
        state: GcRef<PendingIntlDisplayNames>,
    ) -> Result<PendingIntlDisplayNames, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_intl_display_names)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(crate) fn pending_intl_display_names_stage(
        &mut self,
        state: GcRef<PendingIntlDisplayNames>,
    ) -> Result<IntlDisplayNamesStage, ExecutionError> {
        self.pending_intl_display_names_snapshot(state)
            .map(|pending| pending.stage)
    }

    pub(super) fn update_pending_intl_display_names(
        &mut self,
        state: GcRef<PendingIntlDisplayNames>,
        update: impl FnOnce(&mut PendingIntlDisplayNames),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                update(
                    no_gc
                        .borrow_mut(state, self.types.pending_intl_display_names)
                        .map_err(ExecutionError::NoGcBorrow)?,
                );
                Ok(())
            })
        })
    }

    /// Replaces one managed slot and records its generational edge.
    pub(super) fn set_pending_intl_display_names_value(
        &mut self,
        state: GcRef<PendingIntlDisplayNames>,
        select: impl FnOnce(&mut PendingIntlDisplayNames) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                *select(
                    no_gc
                        .borrow_mut(state, self.types.pending_intl_display_names)
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
