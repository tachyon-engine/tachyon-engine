//! Isolate-owned host handoff state for ECMAScript dynamic imports.

use core::num::NonZeroU32;

use tachyon_gc::{Trace, Tracer};

use super::{ModuleError, ModuleId};
use crate::{ExecutionError, Isolate, PromiseState, Value, tuning::modules::*};

/// Stable identity for one dynamic import handoff.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct DynamicImportRequestId(NonZeroU32);

const _: [(); 4] = [(); core::mem::size_of::<DynamicImportRequestId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<DynamicImportRequestId>>()];

/// One import attribute preserving exact ECMAScript string code units.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicImportAttribute {
    key: Box<[u16]>,
    value: Box<[u16]>,
}

impl DynamicImportAttribute {
    pub fn try_from_utf16(key: &[u16], value: &[u16]) -> Result<Self, DynamicImportError> {
        Ok(Self {
            key: try_owned_units(key, "dynamic import attribute key")?,
            value: try_owned_units(value, "dynamic import attribute value")?,
        })
    }

    #[must_use]
    pub fn key(&self) -> &[u16] {
        &self.key
    }

    #[must_use]
    pub fn value(&self) -> &[u16] {
        &self.value
    }
}

/// Owned request data that may leave the isolate while its Promise remains rooted inside it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicImportRequest {
    id: DynamicImportRequestId,
    specifier: Box<[u16]>,
    referrer: Option<ModuleId>,
    attributes: Vec<DynamicImportAttribute>,
}

impl DynamicImportRequest {
    #[must_use]
    pub const fn id(&self) -> DynamicImportRequestId {
        self.id
    }

    #[must_use]
    pub fn specifier(&self) -> &[u16] {
        &self.specifier
    }

    #[must_use]
    pub const fn referrer(&self) -> Option<ModuleId> {
        self.referrer
    }

    #[must_use]
    pub fn attributes(&self) -> &[DynamicImportAttribute] {
        &self.attributes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DynamicImportError {
    AllocationFailed { collection: &'static str },
    RequestLimit { limit: u32 },
    RequestIdExhausted,
    UnknownRequest(DynamicImportRequestId),
}

#[derive(Debug)]
struct ActiveDynamicImport {
    id: DynamicImportRequestId,
    request: Option<DynamicImportRequest>,
    promise: Value,
    module: Option<ModuleId>,
    evaluation_promise: Option<Value>,
}

impl Trace for ActiveDynamicImport {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.promise.trace(tracer);
        self.evaluation_promise.trace(tracer);
    }
}

#[derive(Debug)]
pub(super) struct DynamicImportState {
    active: Vec<ActiveDynamicImport>,
    next_id: NonZeroU32,
    limit: u32,
}

impl DynamicImportState {
    pub(super) fn try_new() -> Result<Self, ModuleError> {
        let limit = MAX_PENDING_DYNAMIC_IMPORTS;
        let mut active = Vec::new();
        active
            .try_reserve_exact(INITIAL_DYNAMIC_IMPORT_CAPACITY.min(limit as usize))
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "dynamic import requests",
            })?;
        Ok(Self {
            active,
            next_id: NonZeroU32::MIN,
            limit,
        })
    }

    /// Publishes owned handoff data only after every fallible allocation succeeds.
    fn enqueue(
        &mut self,
        specifier: &[u16],
        referrer: Option<ModuleId>,
        attributes: &[DynamicImportAttribute],
        promise: Value,
    ) -> Result<DynamicImportRequestId, DynamicImportError> {
        if self.active.len() >= self.limit as usize {
            return Err(DynamicImportError::RequestLimit { limit: self.limit });
        }
        let id = DynamicImportRequestId(self.next_id);
        let next_id = self
            .next_id
            .get()
            .checked_add(1)
            .and_then(NonZeroU32::new)
            .ok_or(DynamicImportError::RequestIdExhausted)?;
        let request = DynamicImportRequest {
            id,
            specifier: try_owned_units(specifier, "dynamic import specifier")?,
            referrer,
            attributes: try_owned_attributes(attributes)?,
        };
        self.active
            .try_reserve_exact(1)
            .map_err(|_| DynamicImportError::AllocationFailed {
                collection: "dynamic import requests",
            })?;
        self.active.push(ActiveDynamicImport {
            id,
            request: Some(request),
            promise,
            module: None,
            evaluation_promise: None,
        });
        self.next_id = next_id;
        Ok(id)
    }

    fn take_pending(&mut self) -> Option<DynamicImportRequest> {
        self.active
            .iter_mut()
            .find_map(|entry| entry.request.take())
    }

    fn remove(
        &mut self,
        id: DynamicImportRequestId,
    ) -> Result<ActiveDynamicImport, DynamicImportError> {
        let index = self
            .active
            .iter()
            .position(|entry| entry.id == id && entry.request.is_none())
            .ok_or(DynamicImportError::UnknownRequest(id))?;
        Ok(self.active.swap_remove(index))
    }

    fn dispatched_promise(&self, id: DynamicImportRequestId) -> Result<Value, DynamicImportError> {
        self.active
            .iter()
            .find(|entry| entry.id == id && entry.request.is_none())
            .map(|entry| entry.promise)
            .ok_or(DynamicImportError::UnknownRequest(id))
    }

    fn attach_module(
        &mut self,
        id: DynamicImportRequestId,
        module: ModuleId,
    ) -> Result<(), DynamicImportError> {
        let entry = self
            .active
            .iter_mut()
            .find(|entry| entry.id == id && entry.request.is_none() && entry.module.is_none())
            .ok_or(DynamicImportError::UnknownRequest(id))?;
        entry.module = Some(module);
        Ok(())
    }

    fn next_unstarted(&self) -> Option<(DynamicImportRequestId, ModuleId, Value)> {
        self.active.iter().find_map(|entry| {
            (entry.evaluation_promise.is_none())
                .then(|| entry.module.map(|module| (entry.id, module, entry.promise)))
                .flatten()
        })
    }

    #[inline]
    fn started_at(&self, index: usize) -> Option<(ModuleId, Value)> {
        let entry = self.active.get(index)?;
        entry.module.zip(entry.evaluation_promise)
    }

    #[inline]
    fn active_len(&self) -> usize {
        self.active.len()
    }

    fn set_evaluation_promise(
        &mut self,
        id: DynamicImportRequestId,
        promise: Value,
    ) -> Result<(), DynamicImportError> {
        let entry = self
            .active
            .iter_mut()
            .find(|entry| entry.id == id && entry.module.is_some())
            .ok_or(DynamicImportError::UnknownRequest(id))?;
        entry.evaluation_promise = Some(promise);
        Ok(())
    }

    #[inline]
    fn contains_promise(&self, promise: Value) -> bool {
        self.active.iter().any(|entry| entry.promise == promise)
    }

    fn completion_for_promise(&self, promise: Value) -> Option<(DynamicImportRequestId, ModuleId)> {
        self.active
            .iter()
            .find(|entry| entry.promise == promise && entry.evaluation_promise.is_some())
            .and_then(|entry| entry.module.map(|module| (entry.id, module)))
    }
}

impl Trace for DynamicImportState {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for entry in &mut self.active {
            entry.trace(tracer);
        }
    }
}

impl Isolate {
    /// Creates the result Promise and queues an owned request without invoking host code.
    #[cfg(test)]
    pub(crate) fn enqueue_dynamic_import(
        &mut self,
        specifier: &[u16],
        referrer: Option<ModuleId>,
        attributes: &[DynamicImportAttribute],
    ) -> Result<(DynamicImportRequestId, Value), ExecutionError> {
        let promise = self.create_promise(
            PromiseState::Pending,
            Value::from_immediate(tachyon_value::Immediate::Undefined),
        )?;
        self.enqueue_dynamic_import_with_promise(specifier, referrer, attributes, promise)
    }

    /// Enqueues a dynamic import using a Promise created before observable conversion work.
    pub(crate) fn enqueue_dynamic_import_with_promise(
        &mut self,
        specifier: &[u16],
        referrer: Option<ModuleId>,
        attributes: &[DynamicImportAttribute],
        promise: Value,
    ) -> Result<(DynamicImportRequestId, Value), ExecutionError> {
        let id = self
            .module_graph
            .dynamic_imports
            .enqueue(specifier, referrer, attributes, promise)
            .map_err(dynamic_import_execution_error)?;
        Ok((id, promise))
    }

    /// Removes the oldest undispatched request for host resolve/load work.
    pub fn take_pending_dynamic_import(&mut self) -> Option<DynamicImportRequest> {
        self.module_graph.dynamic_imports.take_pending()
    }

    /// Rejects one host-dispatched request with the original JavaScript reason.
    pub fn complete_dynamic_import_failure(
        &mut self,
        id: DynamicImportRequestId,
        reason: Value,
    ) -> Result<Value, ExecutionError> {
        let promise = self
            .module_graph
            .dynamic_imports
            .dispatched_promise(id)
            .map_err(dynamic_import_execution_error)?;
        self.settle_promise(promise, PromiseState::Rejected, reason)?;
        self.module_graph
            .dynamic_imports
            .remove(id)
            .map_err(dynamic_import_execution_error)?;
        Ok(promise)
    }

    /// Attaches the loaded module; the isolate-wide driver owns evaluation and settlement.
    pub fn complete_dynamic_import_success(
        &mut self,
        id: DynamicImportRequestId,
        module: ModuleId,
    ) -> Result<Value, ExecutionError> {
        let promise = self
            .module_graph
            .dynamic_imports
            .dispatched_promise(id)
            .map_err(dynamic_import_execution_error)?;
        self.module_graph
            .dynamic_imports
            .attach_module(id, module)
            .map_err(dynamic_import_execution_error)?;
        Ok(promise)
    }

    /// Attaches one import to its cycle-root evaluation Promise through the FIFO reaction queue.
    pub(crate) fn advance_dynamic_import(&mut self) -> Result<bool, ExecutionError> {
        if let Some((id, module, promise)) = self.module_graph.dynamic_imports.next_unstarted() {
            let evaluation = self
                .evaluate_module_promise(module)
                .map_err(|error| match error {
                    super::ModuleEvaluationError::Graph(error) => ExecutionError::Module(error),
                    super::ModuleEvaluationError::Execution(error) => error,
                    super::ModuleEvaluationError::AsyncEvaluationPending(module) => {
                        ExecutionError::Module(ModuleError::DynamicImportModuleNotEvaluated(module))
                    }
                })?;
            self.perform_promise_then_with_capability(evaluation, None, None, promise)?;
            self.module_graph
                .dynamic_imports
                .set_evaluation_promise(id, evaluation)
                .map_err(dynamic_import_execution_error)?;
            return Ok(true);
        }
        let mut completable = None;
        for index in 0..self.module_graph.dynamic_imports.active_len() {
            let Some((module, evaluation)) = self.module_graph.dynamic_imports.started_at(index)
            else {
                continue;
            };
            if self
                .module_graph
                .evaluation_outcome(module)
                .map_err(ExecutionError::Module)?
                .is_some()
            {
                completable = Some((module, evaluation));
                break;
            }
        }
        let Some((_, evaluation)) = completable else {
            return Ok(false);
        };
        let before = self.promise_snapshot(evaluation)?.state;
        let root = self.module_graph.evaluation_root_for_promise(evaluation);
        self.settle_completed_module_promise(root, evaluation)?;
        Ok(before != self.promise_snapshot(evaluation)?.state)
    }

    #[inline]
    pub(crate) fn is_dynamic_import_promise(&self, promise: Value) -> bool {
        self.module_graph.dynamic_imports.contains_promise(promise)
    }

    /// Runs the internal ContinueDynamicImport reaction after module evaluation settles.
    pub(crate) fn resume_dynamic_import_job(
        &mut self,
        promise: Value,
        reason: Value,
        rejected: bool,
    ) -> Result<(), ExecutionError> {
        let (id, module) = self
            .module_graph
            .dynamic_imports
            .completion_for_promise(promise)
            .ok_or(ExecutionError::Module(ModuleError::InvalidLinkState))?;
        let (state, result) = if rejected {
            (PromiseState::Rejected, reason)
        } else {
            (PromiseState::Fulfilled, self.get_module_namespace(module)?)
        };
        self.settle_promise(promise, state, result)?;
        self.module_graph
            .dynamic_imports
            .remove(id)
            .map_err(dynamic_import_execution_error)?;
        Ok(())
    }
}

fn try_owned_units(
    value: &[u16],
    collection: &'static str,
) -> Result<Box<[u16]>, DynamicImportError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(value.len())
        .map_err(|_| DynamicImportError::AllocationFailed { collection })?;
    owned.extend_from_slice(value);
    Ok(owned.into_boxed_slice())
}

fn try_owned_attributes(
    attributes: &[DynamicImportAttribute],
) -> Result<Vec<DynamicImportAttribute>, DynamicImportError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(
            attributes
                .len()
                .max(INITIAL_DYNAMIC_IMPORT_ATTRIBUTE_CAPACITY),
        )
        .map_err(|_| DynamicImportError::AllocationFailed {
            collection: "dynamic import attributes",
        })?;
    for attribute in attributes {
        owned.push(DynamicImportAttribute::try_from_utf16(
            attribute.key(),
            attribute.value(),
        )?);
    }
    Ok(owned)
}

fn dynamic_import_execution_error(error: DynamicImportError) -> ExecutionError {
    let error = match error {
        DynamicImportError::AllocationFailed { collection } => {
            ModuleError::AllocationFailed { collection }
        }
        DynamicImportError::RequestLimit { limit } => {
            ModuleError::DynamicImportRequestLimit { limit }
        }
        DynamicImportError::RequestIdExhausted => ModuleError::DynamicImportRequestIdExhausted,
        DynamicImportError::UnknownRequest(id) => ModuleError::UnknownDynamicImportRequest(id),
    };
    ExecutionError::Module(error)
}
