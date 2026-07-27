//! Lazy RegExp String Iterator state and branded `matchAll` execution.

use tachyon_gc::{AllocationSpace, GcRef, Trace, Tracer};
use tachyon_value::{Immediate, Value};

use crate::{
    ExecutionError, Isolate, JsString, OrdinaryObject, PropertyAttributes, ShapeId, VmRoots,
    builtins::advance_regexp_split_index, regexp_exec::regexp_to_length,
};

/// Internal slots for one `%RegExpStringIteratorPrototype%` instance.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct RegExpStringIteratorObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) matcher: Value,
    pub(crate) input: Value,
    pub(crate) global: bool,
    pub(crate) unicode: bool,
    pub(crate) done: bool,
}

impl Trace for RegExpStringIteratorObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.matcher.trace(tracer);
        self.input.trace(tracer);
    }
}

struct RegExpStringIteratorRoots<'a> {
    vm: VmRoots<'a>,
    matcher: Value,
    input: Value,
    prototype: Value,
}

impl Trace for RegExpStringIteratorRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.matcher.trace(tracer);
        self.input.trace(tracer);
        self.prototype.trace(tracer);
    }
}

impl Isolate {
    /// Implements the branded `RegExp.prototype[Symbol.matchAll]` creation path.
    pub(crate) fn regexp_match_all(
        &mut self,
        site: &crate::CallSite,
    ) -> Result<Value, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let input = self.regexp_string_argument(argument)?;
        self.regexp_match_all_values(site, site.this_value, input)
    }

    /// Implements the common primitive-string `String.prototype.matchAll` path.
    pub(crate) fn string_match_all(
        &mut self,
        site: &crate::CallSite,
    ) -> Result<Value, ExecutionError> {
        let input = self.string_primitive_value(site.this_value)?;
        let pattern = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let regexp = if self.is_object_value(pattern) && self.regexp_data(pattern).is_ok() {
            let flags_value = self.regexp_data(pattern)?.1;
            let flags = self.regexp_flags(flags_value)?;
            if !flags.global {
                return Err(ExecutionError::RegExpMatchAllRequiresGlobal);
            }
            pattern
        } else {
            self.create_global_regexp_for_match_all(site, pattern)?
        };
        self.regexp_match_all_values(site, regexp, input)
    }

    /// Clones the matcher and cursor into a new lazy RegExp String Iterator.
    fn regexp_match_all_values(
        &mut self,
        site: &crate::CallSite,
        receiver: Value,
        input: Value,
    ) -> Result<Value, ExecutionError> {
        let (source, flags_value) = self.regexp_data(receiver)?;
        let flags = self.regexp_flags(flags_value)?;
        let prototype = self
            .realm
            .regexp_prototype
            .expect("RegExp prototype initializes before matchAll");
        let matcher = self.allocate_regexp_object(source, flags_value, prototype)?;
        self.write(site.caller_base, site.destination, matcher)?;
        let last_index_atom = self.intern_intrinsic_name(b"lastIndex")?;
        let last_index = self
            .own_data_property_with_attributes(receiver, last_index_atom)?
            .map_or(Value::from_i32(0), |(value, _)| value);
        self.define_fresh_data_property(
            matcher,
            last_index_atom,
            last_index,
            PropertyAttributes::data(true, false, false),
        )?;
        self.allocate_regexp_string_iterator(
            matcher,
            input,
            flags.global,
            flags.unicode || flags.unicode_sets,
        )
    }

    /// Executes one lazy iterator step and advances empty matches by code point when required.
    pub(crate) fn regexp_string_iterator_next(
        &mut self,
        site: &crate::CallSite,
    ) -> Result<Value, ExecutionError> {
        let value = site.this_value;
        let iterator = self.regexp_string_iterator_reference(value)?;
        let snapshot = self.regexp_string_iterator_snapshot(iterator)?;
        if snapshot.done {
            return self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true);
        }
        let last_index_atom = self.intern_intrinsic_name(b"lastIndex")?;
        let observed = self
            .own_data_property_with_attributes(snapshot.matcher, last_index_atom)?
            .map_or(Value::from_i32(0), |(value, _)| value);
        let index = regexp_to_length(self.convert_to_number(observed)?)?;
        let state = self.allocate_regexp_exec_state(snapshot.matcher, snapshot.input, 0)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let outcome = self.regexp_builtin_exec(snapshot.matcher, snapshot.input, state, index)?;
        if let Some(last_index) = outcome.last_index {
            self.set_own_data_property(snapshot.matcher, last_index_atom, last_index)?;
        }
        if outcome.value.as_immediate() == Some(Immediate::Null) {
            self.finish_regexp_string_iterator(iterator)?;
            return self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true);
        }
        if !snapshot.global {
            self.finish_regexp_string_iterator(iterator)?;
            return self.create_iterator_result(outcome.value, false);
        }
        if self.regexp_match_result_is_empty(outcome.value)? {
            let current = self
                .own_data_property_with_attributes(snapshot.matcher, last_index_atom)?
                .map_or(Value::from_i32(0), |(value, _)| value);
            let current = usize::try_from(regexp_to_length(self.convert_to_number(current)?)?)
                .unwrap_or(usize::MAX);
            let input = self.regexp_string_units(snapshot.input)?;
            let advanced = advance_regexp_split_index(&input, current, snapshot.unicode);
            self.set_own_data_property(
                snapshot.matcher,
                last_index_atom,
                crate::safe_integer_value(advanced as u64),
            )?;
        }
        self.create_iterator_result(outcome.value, false)
    }

    /// Allocates a globally flagged matcher for a non-RegExp String pattern.
    fn create_global_regexp_for_match_all(
        &mut self,
        site: &crate::CallSite,
        pattern: Value,
    ) -> Result<Value, ExecutionError> {
        let source = if pattern.as_immediate() == Some(Immediate::Undefined) {
            self.allocate_runtime_string(
                JsString::try_from_latin1(b"(?:)").map_err(ExecutionError::ConstantString)?,
            )?
        } else {
            self.regexp_string_argument(Some(pattern))?
        };
        let (flags, source) = self.allocate_runtime_string_retaining(
            JsString::try_from_latin1(b"g").map_err(ExecutionError::ConstantString)?,
            source,
        )?;
        let prototype = self
            .realm
            .regexp_prototype
            .expect("RegExp prototype initializes before String matchAll");
        let regexp = self.allocate_regexp_object(source, flags, prototype)?;
        self.write(site.caller_base, site.destination, regexp)?;
        let last_index = self.intern_intrinsic_name(b"lastIndex")?;
        self.define_fresh_data_property(
            regexp,
            last_index,
            Value::from_i32(0),
            PropertyAttributes::data(true, false, false),
        )?;
        Ok(regexp)
    }

    /// Allocates the traced iterator payload after matcher cloning has completed.
    fn allocate_regexp_string_iterator(
        &mut self,
        matcher: Value,
        input: Value,
        global: bool,
        unicode: bool,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .regexp_string_iterator_prototype
            .expect("RegExp String Iterator prototype initializes before matchAll");
        let mut roots = RegExpStringIteratorRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            matcher,
            input,
            prototype,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.regexp_string_iterator,
                0,
                0,
                RegExpStringIteratorObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                    matcher: roots.matcher,
                    input: roots.input,
                    global,
                    unicode,
                    done: false,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|iterator| Value::from_heap_ref(iterator.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Returns whether match result element zero is the empty String.
    fn regexp_match_result_is_empty(&mut self, result: Value) -> Result<bool, ExecutionError> {
        let zero = self.property_key_atom(Value::from_i32(0))?;
        let matched = if let Some(value) = self.dense_array_value(result, zero.into())? {
            Some(value)
        } else {
            self.own_data_property_with_attributes(result, zero)?
                .map(|(value, _)| value)
        };
        let Some(matched) = matched else {
            return Ok(false);
        };
        Ok(self.regexp_string_units(matched)?.is_empty())
    }

    fn regexp_string_iterator_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<RegExpStringIteratorObject>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        self.heap
            .checked_reference(raw, self.types.regexp_string_iterator)
            .map_err(|_| ExecutionError::NotObject(value))
    }

    fn regexp_string_iterator_snapshot(
        &mut self,
        iterator: GcRef<RegExpStringIteratorObject>,
    ) -> Result<RegExpStringIteratorObject, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(iterator, self.types.regexp_string_iterator)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn finish_regexp_string_iterator(
        &mut self,
        iterator: GcRef<RegExpStringIteratorObject>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let iterator = scope.root(iterator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(iterator, self.types.regexp_string_iterator)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .done = true;
                Ok(())
            })
        })
    }
}
