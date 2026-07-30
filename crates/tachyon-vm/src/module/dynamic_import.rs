//! Isolate-owned host handoff state for ECMAScript dynamic imports.

use core::num::NonZeroU32;

use tachyon_gc::{Trace, Tracer};

use super::{ModuleError, ModuleEvaluationState, ModuleGraph, ModuleId};
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
    ModuleNotEvaluated(ModuleId),
}

#[derive(Debug)]
struct ActiveDynamicImport {
    id: DynamicImportRequestId,
    request: Option<DynamicImportRequest>,
    promise: Value,
}

impl Trace for ActiveDynamicImport {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.promise.trace(tracer);
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
}

impl Trace for DynamicImportState {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for entry in &mut self.active {
            entry.trace(tracer);
        }
    }
}

impl ModuleGraph {
    fn dynamic_import_outcome(
        &self,
        module: ModuleId,
    ) -> Result<Result<(), Value>, DynamicImportError> {
        let record = self
            .record(module)
            .map_err(|_| DynamicImportError::ModuleNotEvaluated(module))?;
        match record.evaluation {
            ModuleEvaluationState::Evaluated(_) => Ok(Ok(())),
            ModuleEvaluationState::Errored(reason) => Ok(Err(reason)),
            ModuleEvaluationState::Unevaluated
            | ModuleEvaluationState::Waiting
            | ModuleEvaluationState::Evaluating
            | ModuleEvaluationState::AsyncEvaluating(_) => {
                Err(DynamicImportError::ModuleNotEvaluated(module))
            }
        }
    }
}

impl Isolate {
    /// Creates the result Promise and queues an owned request without invoking host code.
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
    ) -> Result<(), ExecutionError> {
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
        Ok(())
    }

    /// Fulfills with an evaluated module namespace, or preserves module evaluation rejection.
    pub fn complete_dynamic_import_success(
        &mut self,
        id: DynamicImportRequestId,
        module: ModuleId,
    ) -> Result<(), ExecutionError> {
        let outcome = self
            .module_graph
            .dynamic_import_outcome(module)
            .map_err(dynamic_import_execution_error)?;
        let promise = self
            .module_graph
            .dynamic_imports
            .dispatched_promise(id)
            .map_err(dynamic_import_execution_error)?;
        match outcome {
            Ok(()) => {
                let namespace = self.get_module_namespace(module)?;
                self.settle_promise(promise, PromiseState::Fulfilled, namespace)?;
            }
            Err(reason) => self.settle_promise(promise, PromiseState::Rejected, reason)?,
        }
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
        DynamicImportError::ModuleNotEvaluated(module) => {
            ModuleError::DynamicImportModuleNotEvaluated(module)
        }
    };
    ExecutionError::Module(error)
}
