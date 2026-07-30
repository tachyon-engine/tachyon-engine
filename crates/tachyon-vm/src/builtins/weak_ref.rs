//! WeakRef construction and dereference over collector-cleared weak edges.

use super::super::*;
use tachyon_gc::WeakGcRef;

impl Isolate {
    /// Constructs a WeakRef and retains the target through the current ECMAScript job.
    pub(crate) fn create_weak_ref_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let target = self.weak_key(target)?;
        let raw = target.as_heap_ref().expect("weak target was validated");
        let fallback = self
            .realm
            .weak_ref_prototype
            .expect("WeakRef prototype initializes before construction");
        let prototype_atom = self.intern_intrinsic_name(b"prototype")?;
        let prototype = self
            .get_data_property(site.new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .unwrap_or(fallback);
        self.heap
            .add_to_kept_objects(raw)
            .map_err(ExecutionError::KeptObject)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.weak_ref_object,
                0,
                0,
                WeakRefObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                    target: WeakGcRef::new(GcRef::from_erased_raw(raw)),
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|reference| Value::from_heap_ref(reference.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Returns the live target and implements AddToKeptObjects before exposing its identity.
    pub(crate) fn weak_ref_deref(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(receiver))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.weak_ref_object)
            .map_err(|_| ExecutionError::NotObject(receiver))?;
        let target = self.heap.with_running_scope(|scope| {
            let reference = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(reference, self.types.weak_ref_object)
                    .map(|reference| reference.target.get())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let Some(target) = target else {
            return Ok(Value::from_immediate(Immediate::Undefined));
        };
        self.heap
            .add_to_kept_objects(target.raw())
            .map_err(ExecutionError::KeptObject)?;
        Ok(Value::from_heap_ref(target.raw()))
    }
}
