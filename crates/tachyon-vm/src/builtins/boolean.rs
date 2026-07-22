//! ECMAScript Boolean wrapper and prototype operations.

use super::super::*;

impl Isolate {
    /// Extracts `[[BooleanData]]` from a primitive or genuine Boolean wrapper.
    pub(crate) fn this_boolean_value(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if matches!(
            value.as_immediate(),
            Some(Immediate::True | Immediate::False)
        ) {
            return Ok(value);
        }
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let boolean = self
            .heap
            .checked_reference(raw, self.types.boolean_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let boolean = scope.root(boolean).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(boolean, self.types.boolean_object)
                    .map(|boolean| boolean.boolean_data)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Boxes one converted Boolean with newTarget's current prototype.
    pub(crate) fn box_boolean_from_constructor(
        &mut self,
        boolean: Value,
        new_target: Value,
    ) -> Result<Value, ExecutionError> {
        let prototype_atom = self.prototype_atom()?;
        let prototype = self
            .constructor_prototype_value(new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .or_else(|| {
                self.realm_for_callable(new_target).ok().and_then(|realm| {
                    self.realm_intrinsic_prototype(realm, IntrinsicPrototypeKind::Boolean)
                })
            })
            .unwrap_or_else(|| {
                self.realm
                    .boolean_prototype
                    .expect("Boolean prototype initializes before construction")
            });
        self.allocate_boolean_object(boolean, prototype, AllocationSpace::Young)
    }

    /// Implements Boolean.prototype.toString without Rust formatting or allocation intermediates.
    pub(crate) fn boolean_to_string(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        let boolean = self.this_boolean_value(receiver)?;
        let text = if boolean.as_immediate() == Some(Immediate::True) {
            b"true".as_slice()
        } else {
            b"false".as_slice()
        };
        self.allocate_runtime_string(
            JsString::try_from_latin1(text).map_err(ExecutionError::PropertyKeyString)?,
        )
    }
}
