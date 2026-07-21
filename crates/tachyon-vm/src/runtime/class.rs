//! Rare class-constructor metadata and exact traced public instance-field plans.

use std::mem::size_of;

use tachyon_bytecode::FunctionId;
use tachyon_gc::{GcExternalMemory, GcRef, Trace, Tracer};

use super::callable::{FunctionExecutable, FunctionObject};
use super::environment::Environment;
use crate::object::PropertyKey;
use crate::{CodeId, Value};

/// One normalized public field record retained by a class constructor.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClassFieldRecord {
    pub(crate) key: PropertyKey,
    pub(crate) initializer: Option<Value>,
    pub(crate) infer_name: bool,
}

impl Trace for ClassFieldRecord {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.key.trace(tracer);
        self.initializer.trace(tracer);
    }
}

/// Exact-capacity immutable field plan owned by one class constructor.
#[derive(Debug)]
pub(crate) struct ClassFieldPlan {
    pub(crate) records: Box<[ClassFieldRecord]>,
}

impl Trace for ClassFieldPlan {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for record in self.records.iter_mut() {
            record.trace(tracer);
        }
    }
}

impl GcExternalMemory for ClassFieldPlan {
    #[inline]
    fn external_memory_bytes(&self) -> usize {
        self.records
            .len()
            .saturating_mul(size_of::<ClassFieldRecord>())
    }
}

/// Rare executable payload that leaves ordinary bytecode functions at their existing size.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClassConstructorData {
    pub(crate) code: CodeId,
    pub(crate) function: FunctionId,
    pub(crate) environment: Option<GcRef<Environment>>,
    pub(crate) plan: GcRef<ClassFieldPlan>,
}

impl Trace for ClassConstructorData {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.environment.trace(tracer);
        self.plan.trace(tracer);
    }
}

/// Mutable cursor for one resumable InitializeInstanceElements operation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingInstanceElements {
    pub(crate) receiver: Value,
    pub(crate) plan: GcRef<ClassFieldPlan>,
    pub(crate) index: u32,
}

impl Trace for PendingInstanceElements {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.plan.trace(tracer);
    }
}

const _: [(); 16] = [(); size_of::<ClassConstructorData>()];
const _: [(); 16] = [(); size_of::<FunctionExecutable>()];
const _: [(); 56] = [(); size_of::<FunctionObject>()];
