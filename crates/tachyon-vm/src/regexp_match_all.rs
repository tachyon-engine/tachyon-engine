//! Lazy RegExp String Iterator state and branded `matchAll` execution.

use tachyon_gc::{AllocationSpace, GcRef, Trace, Tracer};
use tachyon_value::{Immediate, Value};

use crate::{
    ExecutionError, Isolate, JsString, NativeCallState, OrdinaryObject, PropertyAttributes,
    ShapeId, VmRoots,
    builtins::advance_regexp_split_index,
    regexp_exec::{REGEXP_EXEC_RESULT, regexp_to_length},
    runtime::fiber::{
        ConversionConsumer, NativeContinuation, NativeContinuationSite, ProxySetMode,
        RegExpStringIteratorStage,
    },
};

const ITERATOR_INPUT: usize = 0;
const ITERATOR_MATCHER: usize = 1;
const ITERATOR_OBJECT: usize = 2;
const ITERATOR_RESULT: usize = 3;

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

    /// Starts one lazy iterator step through the observable RegExpExec protocol.
    pub(crate) fn regexp_string_iterator_next(
        &mut self,
        site: &crate::CallSite,
    ) -> Result<(), ExecutionError> {
        let value = site.this_value;
        let iterator = self.regexp_string_iterator_reference(value)?;
        let snapshot = self.regexp_string_iterator_snapshot(iterator)?;
        if snapshot.done {
            let result =
                self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true)?;
            return self.write(site.caller_base, site.destination, result);
        }
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let state = self.allocate_regexp_exec_state(snapshot.matcher, snapshot.input, 1)?;
        self.update_regexp_exec_state_value(state, ITERATOR_OBJECT, value)?;
        self.root_regexp_string_iterator_state(native_site, state)?;
        let exec = self.intern_intrinsic_name(b"exec")?;
        self.dispatch_regexp_string_iterator_read(
            native_site,
            state,
            snapshot.matcher,
            exec.into(),
            RegExpStringIteratorStage::ExecGet,
        )
    }

    /// Advances an observable iterator Get, Call, or Set completion.
    pub(crate) fn resume_regexp_string_iterator(
        &mut self,
        continuation: NativeContinuation,
        stage: RegExpStringIteratorStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        let state = self.native_call_state_reference(continuation.first())?;
        self.root_regexp_string_iterator_state(site, state)?;
        match stage {
            RegExpStringIteratorStage::ExecGet if self.is_callable_value(value)? => {
                self.dispatch_regexp_string_iterator_call(site, state, value)
            }
            RegExpStringIteratorStage::ExecGet => {
                self.finish_regexp_string_iterator_builtin(site, state)
            }
            RegExpStringIteratorStage::ExecCall => {
                self.validate_regexp_string_iterator_exec_result(value)?;
                self.update_regexp_exec_state_value(state, ITERATOR_RESULT, value)?;
                self.finish_regexp_string_iterator_exec(site, state)
            }
            RegExpStringIteratorStage::MatchGet => {
                self.begin_regexp_string_iterator_match_conversion(site, state, value)
            }
            RegExpStringIteratorStage::LastIndexGet => {
                self.begin_regexp_string_iterator_last_index_conversion(site, state, value)
            }
            RegExpStringIteratorStage::LastIndexSet => {
                self.publish_regexp_string_iterator_result(site, state)
            }
        }
    }

    /// Resumes ToString for an observable custom match element zero.
    pub(crate) fn resume_regexp_string_iterator_match_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.root_regexp_string_iterator_state(site, state)?;
        self.finish_regexp_string_iterator_match_conversion(site, state, primitive)
    }

    /// Resumes ToLength for an observable matcher lastIndex value.
    pub(crate) fn resume_regexp_string_iterator_last_index_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.root_regexp_string_iterator_state(site, state)?;
        self.finish_regexp_string_iterator_last_index(site, state, primitive)
    }

    /// Calls a custom exec method with the original matcher and String argument.
    fn dispatch_regexp_string_iterator_call(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        method: Value,
    ) -> Result<(), ExecutionError> {
        let matcher = self.native_call_state_snapshot(state)?.values[ITERATOR_MATCHER];
        self.dispatch_property_callback(
            NativeContinuation::regexp_string_iterator(
                site,
                RegExpStringIteratorStage::ExecCall,
                Value::from_heap_ref(state.raw()),
                matcher,
            ),
            method,
        )
        .map(|_| ())
    }

    /// Retains the legacy branded fallback while custom exec uses the resumable protocol.
    fn finish_regexp_string_iterator_builtin(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let matcher = snapshot.values[ITERATOR_MATCHER];
        let input = snapshot.values[ITERATOR_INPUT];
        self.regexp_data(matcher)?;
        let last_index = self.intern_intrinsic_name(b"lastIndex")?;
        let observed = self
            .own_data_property_with_attributes(matcher, last_index)?
            .map_or(Value::from_i32(0), |(value, _)| value);
        let index = regexp_to_length(self.convert_to_number(observed)?)?;
        let exec_state = self.allocate_regexp_exec_state(matcher, input, 0)?;
        self.fiber
            .completions
            .push_native(NativeContinuation::regexp_string_iterator(
                site,
                RegExpStringIteratorStage::ExecGet,
                Value::from_heap_ref(state.raw()),
                matcher,
            ))
            .map_err(Self::completion_stack_error)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(exec_state.raw()),
        )?;
        let outcome = self.regexp_builtin_exec(matcher, input, exec_state, index);
        let rooted_exec_state = self.read(site.caller_base, site.destination)?;
        let prepared = (|| -> Result<Value, ExecutionError> {
            let outcome = outcome?;
            if let Some(next) = outcome.last_index {
                self.set_own_data_property(matcher, last_index, next)?;
            }
            if outcome.value.as_immediate() == Some(Immediate::Null) {
                Ok(outcome.value)
            } else {
                let exec_state = self.native_call_state_reference(rooted_exec_state)?;
                Ok(self.native_call_state_snapshot(exec_state)?.values[REGEXP_EXEC_RESULT])
            }
        })();
        let parent = self.pop_native_continuation()?;
        let state = self.native_call_state_reference(parent.first())?;
        self.root_regexp_string_iterator_state(site, state)?;
        let result = prepared?;
        self.update_regexp_exec_state_value(state, ITERATOR_RESULT, result)?;
        self.finish_regexp_string_iterator_exec(site, state)
    }

    /// Handles null/non-global results or begins observable element-zero lookup.
    fn finish_regexp_string_iterator_exec(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let iterator_value = snapshot.values[ITERATOR_OBJECT];
        let result = snapshot.values[ITERATOR_RESULT];
        let iterator = self.regexp_string_iterator_reference(iterator_value)?;
        let slots = self.regexp_string_iterator_snapshot(iterator)?;
        if result.as_immediate() == Some(Immediate::Null) {
            self.finish_regexp_string_iterator(iterator)?;
            let output =
                self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true)?;
            return self.write(site.caller_base, site.destination, output);
        }
        if !slots.global {
            self.finish_regexp_string_iterator(iterator)?;
            return self.publish_regexp_string_iterator_result(site, state);
        }
        let zero = self.property_key_atom(Value::from_i32(0))?;
        self.dispatch_regexp_string_iterator_read(
            site,
            state,
            result,
            zero.into(),
            RegExpStringIteratorStage::MatchGet,
        )
    }

    /// Converts the first match to String and advances lastIndex only for an empty match.
    fn begin_regexp_string_iterator_match_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::RegExpStringIteratorMatch,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_regexp_string_iterator_match_conversion(site, state, value)
    }

    /// Checks the primitive match String and performs the required lastIndex Get.
    fn finish_regexp_string_iterator_match_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_symbol_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        let matched = self.primitive_string_value(Some(value))?;
        self.root_regexp_string_iterator_state(site, state)?;
        if !self.regexp_string_units(matched)?.is_empty() {
            return self.publish_regexp_string_iterator_result(site, state);
        }
        let matcher = self.native_call_state_snapshot(state)?.values[ITERATOR_MATCHER];
        let last_index = self.intern_intrinsic_name(b"lastIndex")?;
        self.dispatch_regexp_string_iterator_read(
            site,
            state,
            matcher,
            last_index.into(),
            RegExpStringIteratorStage::LastIndexGet,
        )
    }

    /// Converts an observed lastIndex with number hint before advancing the input cursor.
    fn begin_regexp_string_iterator_last_index_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::RegExpStringIteratorLastIndex,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_regexp_string_iterator_last_index(site, state, value)
    }

    /// Applies AdvanceStringIndex and performs the strict observable lastIndex Set.
    fn finish_regexp_string_iterator_last_index(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let index = usize::try_from(regexp_to_length(self.convert_to_number(value)?)?)
            .unwrap_or(usize::MAX);
        let snapshot = self.native_call_state_snapshot(state)?;
        let iterator = self.regexp_string_iterator_reference(snapshot.values[ITERATOR_OBJECT])?;
        let slots = self.regexp_string_iterator_snapshot(iterator)?;
        let units = self.regexp_string_units(snapshot.values[ITERATOR_INPUT])?;
        let next = advance_regexp_split_index(&units, index, slots.unicode);
        self.dispatch_regexp_string_iterator_write(
            site,
            state,
            snapshot.values[ITERATOR_MATCHER],
            crate::safe_integer_value(next as u64),
        )
    }

    /// Publishes the retained match as a fresh iterator result object.
    fn publish_regexp_string_iterator_result(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let result = self.native_call_state_snapshot(state)?.values[ITERATOR_RESULT];
        let output = self.create_iterator_result(result, false)?;
        self.write(site.caller_base, site.destination, output)
    }

    /// Performs one observable property read while retaining the full iterator step.
    fn dispatch_regexp_string_iterator_read(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        receiver: Value,
        key: crate::PropertyKey,
        stage: RegExpStringIteratorStage,
    ) -> Result<(), ExecutionError> {
        let continuation = NativeContinuation::regexp_string_iterator(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            receiver,
        );
        let depth = self.fiber.completions.len();
        let frames = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key) {
            if self.fiber.completions.len() > depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frames || self.fiber.completions.len() <= depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_regexp_string_iterator(continuation, stage, value)
    }

    /// Performs the strict lastIndex write needed after an empty custom match.
    fn dispatch_regexp_string_iterator_write(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        receiver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let stage = RegExpStringIteratorStage::LastIndexSet;
        let continuation = NativeContinuation::regexp_string_iterator(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            receiver,
        );
        let depth = self.fiber.completions.len();
        let frames = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let key = self.intern_intrinsic_name(b"lastIndex")?;
        if let Err(error) = self.dispatch_proxy_aware_property_write(
            site,
            receiver,
            receiver,
            key.into(),
            value,
            ProxySetMode::ObjectAssign,
        ) {
            if self.fiber.completions.len() > depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frames || self.fiber.completions.len() <= depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        self.resume_regexp_string_iterator(continuation, stage, value)
    }

    #[inline(always)]
    fn root_regexp_string_iterator_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    #[inline(always)]
    fn validate_regexp_string_iterator_exec_result(
        &self,
        result: Value,
    ) -> Result<(), ExecutionError> {
        if result.as_immediate() == Some(Immediate::Null) || self.is_object_value(result) {
            Ok(())
        } else {
            Err(ExecutionError::NotObject(result))
        }
    }

    /// Allocates a globally flagged matcher for a non-RegExp String pattern.
    pub(crate) fn create_global_regexp_for_match_all(
        &mut self,
        site: NativeContinuationSite,
        pattern: Value,
    ) -> Result<Value, ExecutionError> {
        let source = if self.is_regexp_value(pattern) {
            self.regexp_data(pattern)?.0
        } else if pattern.as_immediate() == Some(Immediate::Undefined) {
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
        let regexp = self.read(site.caller_base, site.destination)?;
        self.define_fresh_data_property(
            regexp,
            last_index,
            Value::from_i32(0),
            PropertyAttributes::data(true, false, false),
        )?;
        self.read(site.caller_base, site.destination)
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
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                inactive_realms: &mut self.inactive_realms,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
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
