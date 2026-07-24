//! `Array.from` iterable and array-like branches.

use super::*;

impl Isolate {
    /// Returns the iterator only for abrupt stages that require IteratorClose.
    pub(crate) fn array_static_close_iterator(
        &mut self,
        mut continuation: NativeContinuation,
    ) -> Result<Option<Value>, ExecutionError> {
        let stage = match continuation.kind() {
            NativeContinuationKind::ArrayStatic(stage) => stage,
            _ => {
                let Some(parent) = self.fiber.completions.last_native() else {
                    return Ok(None);
                };
                let NativeContinuationKind::ArrayStatic(stage) = parent.kind() else {
                    return Ok(None);
                };
                continuation = parent;
                stage
            }
        };
        let state = self.pending_array_static_reference(continuation.first())?;
        let snapshot = self.array_static_snapshot(state)?;
        let close = matches!(
            stage,
            ArrayStaticStage::MapperCall | ArrayStaticStage::Define
        ) || snapshot.close_on_abrupt;
        Ok((snapshot.kind == ArrayStaticKind::FromIterable && close).then_some(snapshot.iterator))
    }

    /// Captures the source and mapper before the first observable iterator lookup.
    pub(crate) fn begin_array_from(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let source = self.call_argument(site, 0)?.unwrap_or(undefined);
        let mapper = self.call_argument(site, 1)?.unwrap_or(undefined);
        let mapping = mapper.as_immediate() != Some(Immediate::Undefined);
        if mapping {
            self.resolve_function_object(mapper)?;
        }
        let this_argument = self.call_argument(site, 2)?.unwrap_or(undefined);
        let state = self.allocate_array_static_state(PendingArrayStatic {
            result: undefined,
            constructor: site.this_value,
            retained: undefined,
            source,
            mapper,
            this_argument,
            iterator: undefined,
            next_method: undefined,
            iterator_result: undefined,
            arguments: Box::new([]),
            kind: ArrayStaticKind::FromArrayLike,
            cursor: 0,
            length: 0,
            mapping,
            close_on_abrupt: false,
            require_iterable: false,
        })?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_array_static_state(native_site, state)?;
        if is_nullish(source) {
            return Err(ExecutionError::NotObject(source));
        }
        let iterator = self
            .realm
            .well_known_symbols
            .iterator
            .expect("Symbol.iterator initializes before Array.from");
        let key = self.property_key(iterator)?;
        self.get_array_static_property(
            native_site,
            state,
            ArrayStaticStage::IteratorMethod,
            source,
            key,
        )
    }

    /// Collects a required synchronous iterable into an intrinsic Array for another builtin.
    pub(crate) fn begin_iterable_to_list(
        &mut self,
        site: NativeContinuationSite,
        source: Value,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let state = self.allocate_array_static_state(PendingArrayStatic {
            result: undefined,
            constructor: undefined,
            retained: undefined,
            source,
            mapper: undefined,
            this_argument: undefined,
            iterator: undefined,
            next_method: undefined,
            iterator_result: undefined,
            arguments: Box::new([]),
            kind: ArrayStaticKind::FromIterable,
            cursor: 0,
            length: 0,
            mapping: false,
            close_on_abrupt: false,
            require_iterable: true,
        })?;
        self.root_array_static_state(site, state)?;
        if is_nullish(source) {
            return Err(ExecutionError::NotObject(source));
        }
        let iterator = self
            .realm
            .well_known_symbols
            .iterator
            .expect("Symbol.iterator initializes before IterableToList");
        let key = self.property_key(iterator)?;
        self.get_array_static_property(site, state, ArrayStaticStage::IteratorMethod, source, key)
    }

    /// Routes each observable Array.from operation to its next protocol stage.
    pub(super) fn resume_array_from(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        stage: ArrayStaticStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match stage {
            ArrayStaticStage::IteratorMethod => {
                self.resume_array_from_iterator_method(site, state, value)
            }
            ArrayStaticStage::IteratorCall => {
                self.resume_array_from_iterator_call(site, state, value)
            }
            ArrayStaticStage::NextMethod => {
                self.set_array_static_value(state, |pending| &mut pending.next_method, value)?;
                self.resolve_function_object(value)?;
                self.advance_array_from_iterable(site, state)
            }
            ArrayStaticStage::NextCall => self.resume_array_from_next(site, state, value),
            ArrayStaticStage::ResultDone => self.resume_array_from_done(site, state, value),
            ArrayStaticStage::ResultValue | ArrayStaticStage::SourceValue => {
                self.resume_array_from_value(site, state, value)
            }
            ArrayStaticStage::MapperCall => self.define_array_from_value(site, state, value),
            ArrayStaticStage::Length => self.resume_array_from_length(site, state, value),
            ArrayStaticStage::Construct
            | ArrayStaticStage::Define
            | ArrayStaticStage::FinalLength => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Chooses iterable or array-like construction after GetMethod(items, @@iterator).
    fn resume_array_from_iterator_method(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        method: Value,
    ) -> Result<(), ExecutionError> {
        if matches!(
            method.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            if self.array_static_snapshot(state)?.require_iterable {
                return Err(ExecutionError::NonCallable(method));
            }
            let source = self.array_static_snapshot(state)?.source;
            let object = self.coerce_to_object(source)?;
            self.set_array_static_value(state, |pending| &mut pending.source, object)?;
            let length = self.length_atom()?;
            return self.get_array_static_property(
                site,
                state,
                ArrayStaticStage::Length,
                object,
                length.into(),
            );
        }
        self.resolve_function_object(method)?;
        self.set_array_static_value(state, |pending| &mut pending.next_method, method)?;
        self.update_array_static_scalars(state, |pending| {
            pending.kind = ArrayStaticKind::FromIterable;
            pending.length = 0;
        })?;
        self.create_or_construct_array_from_result(site, state)
    }

    /// Converts LengthOfArrayLike, including an observable object-to-primitive conversion.
    fn resume_array_from_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::ArrayStaticLength,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_array_from_length(site, state, value)
    }

    /// Resumes ToLength after an object length has produced its primitive value.
    pub(crate) fn resume_array_from_length_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.root_array_static_state(site, state)?;
        self.finish_array_from_length(site, state, value)
    }

    /// Stores the normalized array-like length before result construction.
    fn finish_array_from_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let converted = self.convert_to_number(value)?;
        let length = array_from_to_length(converted)?;
        self.update_array_static_scalars(state, |pending| {
            pending.kind = ArrayStaticKind::FromArrayLike;
            pending.length = length;
        })?;
        self.create_or_construct_array_from_result(site, state)
    }

    /// Allocates the intrinsic Array or dispatches the selected custom constructor.
    fn create_or_construct_array_from_result(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_static_snapshot(state)?;
        if self.is_constructor_value(snapshot.constructor)? {
            return self.construct_array_static(site, state);
        }
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before Array.from");
        let result = self.create_array_object_with_prototype(prototype)?;
        let state =
            self.pending_array_static_reference(self.read(site.caller_base, site.destination)?)?;
        self.set_array_static_value(state, |pending| &mut pending.result, result)?;
        let snapshot = self.array_static_snapshot(state)?;
        if snapshot.kind == ArrayStaticKind::FromArrayLike {
            self.set_array_length_value(result, safe_integer_value(snapshot.length))?;
            self.advance_array_from_array_like(site, state)
        } else {
            self.advance_array_from_iterable(site, state)
        }
    }

    /// Calls the iterator method once, then repeatedly invokes its cached `next` method.
    pub(super) fn advance_array_from_iterable(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_static_snapshot(state)?;
        if snapshot.iterator.as_immediate() == Some(Immediate::Undefined) {
            return self.call_array_static(
                site,
                state,
                ArrayStaticStage::IteratorCall,
                snapshot.next_method,
                snapshot.source,
                &[],
            );
        }
        if snapshot.next_method.as_immediate() == Some(Immediate::Undefined) {
            return self.get_array_static_named_property(
                site,
                state,
                ArrayStaticStage::NextMethod,
                snapshot.iterator,
                b"next",
            );
        }
        if snapshot.cursor >= MAX_SAFE_INTEGER {
            return Err(ExecutionError::ArrayLengthOverflow);
        }
        self.call_array_static(
            site,
            state,
            ArrayStaticStage::NextCall,
            snapshot.next_method,
            snapshot.iterator,
            &[],
        )
    }

    /// Caches the iterator object and clears the temporary iterator-method slot.
    fn resume_array_from_iterator_call(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        iterator: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(iterator) {
            return Err(ExecutionError::NotObject(iterator));
        }
        self.set_array_static_value(state, |pending| &mut pending.iterator, iterator)?;
        self.set_array_static_value(
            state,
            |pending| &mut pending.next_method,
            Value::from_immediate(Immediate::Undefined),
        )?;
        self.advance_array_from_iterable(site, state)
    }

    /// Validates one iterator result before reading its `done` property.
    fn resume_array_from_next(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        result: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(result) {
            return Err(ExecutionError::NotObject(result));
        }
        self.set_array_static_value(state, |pending| &mut pending.iterator_result, result)?;
        self.get_array_static_named_property(
            site,
            state,
            ArrayStaticStage::ResultDone,
            result,
            b"done",
        )
    }

    /// Finishes iteration or reads the current iterator result's `value` property.
    fn resume_array_from_done(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        done: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_truthy_value(done)? {
            let snapshot = self.array_static_snapshot(state)?;
            let length = self.length_atom()?;
            return self.dispatch_array_static_set(
                site,
                state,
                snapshot.result,
                length.into(),
                safe_integer_value(snapshot.cursor),
            );
        }
        let result = self.array_static_snapshot(state)?.iterator_result;
        self.get_array_static_named_property(
            site,
            state,
            ArrayStaticStage::ResultValue,
            result,
            b"value",
        )
    }

    /// Reads the next array-like index and observes mutations between mapper calls.
    pub(super) fn advance_array_from_array_like(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.array_static_snapshot(state)?;
        if snapshot.cursor >= snapshot.length {
            let length = self.length_atom()?;
            return self.dispatch_array_static_set(
                site,
                state,
                snapshot.result,
                length.into(),
                safe_integer_value(snapshot.length),
            );
        }
        let key = self.safe_integer_property_atom(snapshot.cursor)?;
        self.get_array_static_property(
            site,
            state,
            ArrayStaticStage::SourceValue,
            snapshot.source,
            key.into(),
        )
    }

    /// Applies the optional mapper before defining the current output property.
    fn resume_array_from_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_static_value(state, |pending| &mut pending.retained, value)?;
        let snapshot = self.array_static_snapshot(state)?;
        if snapshot.mapping {
            self.update_array_static_scalars(state, |pending| pending.close_on_abrupt = true)?;
            return self.call_array_static(
                site,
                state,
                ArrayStaticStage::MapperCall,
                snapshot.mapper,
                snapshot.this_argument,
                &[value, safe_integer_value(snapshot.cursor)],
            );
        }
        self.define_array_from_value(site, state, value)
    }

    /// Performs CreateDataPropertyOrThrow and commits the cursor only after success.
    fn define_array_from_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_array_static_value(state, |pending| &mut pending.retained, value)?;
        let snapshot = self.array_static_snapshot(state)?;
        let key = self.safe_integer_property_atom(snapshot.cursor)?;
        let descriptor = DataPropertyDescriptor {
            value: Some(value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(true),
        };
        self.update_array_static_scalars(state, |pending| pending.close_on_abrupt = true)?;
        if self.is_proxy_value(snapshot.result) {
            return self.dispatch_array_static_define(
                site,
                state,
                snapshot.result,
                key.into(),
                descriptor.into(),
            );
        }
        self.define_data_property(snapshot.result, key, descriptor)?;
        self.update_array_static_scalars(state, |pending| pending.close_on_abrupt = false)?;
        self.increment_array_static_cursor(state)?;
        self.advance_array_static_after_define(site, state)
    }

    /// Reads one protocol property through ordinary, accessor, and Proxy paths.
    fn get_array_static_named_property(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        stage: ArrayStaticStage,
        receiver: Value,
        name: &[u8],
    ) -> Result<(), ExecutionError> {
        let key = PropertyKey::Atom(self.intern_intrinsic_name(name)?);
        self.get_array_static_property(site, state, stage, receiver, key)
    }

    /// Publishes a typed parent around one Proxy/accessor-aware property Get.
    fn get_array_static_property(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        stage: ArrayStaticStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_array_static_parent(site, state, stage, receiver)?;
        let outcome = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            return Ok(());
        }
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_static_reference(rooted.first())?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_array_static(site, state, stage, value)
    }

    /// Calls one protocol method or mapper without growing the Rust interpreter stack.
    fn call_array_static(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingArrayStatic>,
        stage: ArrayStaticStage,
        callee: Value,
        receiver: Value,
        arguments: &[Value],
    ) -> Result<(), ExecutionError> {
        self.resolve_function_object(callee)?;
        let prefix = if arguments.is_empty() {
            None
        } else {
            let mut copied = Vec::new();
            copied
                .try_reserve_exact(arguments.len())
                .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
            copied.extend_from_slice(arguments);
            Some(self.create_apply_argument_prefix(callee, receiver, copied)?)
        };
        self.push_array_static_parent(site, state, stage, callee)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee,
            argument_base: 0,
            argument_source: None,
            argument_prefix: prefix,
            argument_prefix_offset: 0,
            argument_prefix_count: u32::try_from(arguments.len())
                .map_err(|_| ExecutionError::BoundArgumentCountOverflow)?,
            argument_count: u32::try_from(arguments.len())
                .map_err(|_| ExecutionError::BoundArgumentCountOverflow)?,
            this_value: receiver,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("Array.from protocol call publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let rooted = self.pop_native_continuation()?;
        let state = self.pending_array_static_reference(rooted.first())?;
        let returned = self.read(site.caller_base, site.destination)?;
        self.resume_array_static(site, state, stage, returned)
    }
}

#[inline(always)]
fn array_from_to_length(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if number.is_nan() || number <= 0.0 {
        return Ok(0);
    }
    if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
        return Ok(MAX_SAFE_INTEGER);
    }
    Ok(number.floor() as u64)
}
