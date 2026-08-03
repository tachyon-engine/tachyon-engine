//! Resumable TypedArray map/filter callback, species, and element-write state machines.

use super::*;

const TRANSFORM_SOURCE: usize = 0;
const TRANSFORM_CALLBACK: usize = 1;
const TRANSFORM_OUTPUT: usize = 2;
const TRANSFORM_LENGTH: usize = 3;
const TRANSFORM_CURSOR: usize = 4;

const OUTPUT_TARGET: usize = 0;
const OUTPUT_THIS_ARGUMENT: usize = 1;
const OUTPUT_SELECTED: usize = 2;
const OUTPUT_AUXILIARY: usize = 3;
const OUTPUT_RETAINED: usize = 4;

const MAP_OUTPUT: u8 = 60;
const FILTER_SCAN_OUTPUT: u8 = 61;
const FILTER_COPY_OUTPUT: u8 = 62;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedArrayTransformKind {
    Map,
    FilterScan,
    FilterCopy,
}

struct TypedArrayTransformRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for TypedArrayTransformRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates inputs and publishes the two fixed states before species or callback execution.
    pub(crate) fn begin_typed_array_transform(
        &mut self,
        site: &CallSite,
        kind: TypedArrayCallbackKind,
    ) -> Result<(), ExecutionError> {
        let source = site.this_value;
        let snapshot = self.validated_typed_array_snapshot(source)?;
        let callback = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_callable_value(callback)? {
            return Err(ExecutionError::NonCallable(callback));
        }
        let this_argument = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let undefined = Value::from_immediate(Immediate::Undefined);
        let (selected, output_mode) = match kind {
            TypedArrayCallbackKind::Map => (undefined, MAP_OUTPUT),
            TypedArrayCallbackKind::Filter => {
                let prototype = self
                    .realm
                    .array_prototype
                    .expect("Array prototype initializes before TypedArray filter");
                (
                    self.create_array_object_with_prototype(prototype)?,
                    FILTER_SCAN_OUTPUT,
                )
            }
            _ => return Err(ExecutionError::MissingNativeContinuation),
        };
        let output = self.allocate_typed_array_transform_state(NativeCallState {
            values: [
                undefined,
                this_argument,
                selected,
                Value::from_i32(0),
                undefined,
            ],
            count: output_mode,
        })?;
        let state = self.allocate_typed_array_transform_state(NativeCallState {
            values: [
                source,
                callback,
                Value::from_heap_ref(output.raw()),
                safe_integer_value(snapshot.length as u64),
                Value::from_i32(0),
            ],
            count: 5,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.root_typed_array_transform_state(continuation_site, state)?;
        if kind == TypedArrayCallbackKind::Map {
            self.begin_typed_array_transform_species(continuation_site, state)
        } else {
            self.advance_typed_array_transform(continuation_site, state)
        }
    }

    /// Resumes one property read, species construction, or callback completion.
    pub(crate) fn resume_typed_array_transform(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: TypedArrayTransformStage,
        value: Value,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        match stage {
            TypedArrayTransformStage::Constructor => {
                self.resume_typed_array_transform_constructor(site, state, value)
            }
            TypedArrayTransformStage::Species => {
                self.finish_typed_array_transform_species(site, state, value, true)
            }
            TypedArrayTransformStage::Construct => {
                self.finish_typed_array_transform_construct(site, state, value)
            }
            TypedArrayTransformStage::Callback => {
                if self.finish_typed_array_transform_callback(site, state, value, retained)? {
                    Ok(())
                } else {
                    self.advance_typed_array_transform(site, state)
                }
            }
        }
    }

    /// Commits a mapped object after ToPrimitive and resumes the iterative driver.
    pub(crate) fn resume_typed_array_transform_element_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.finish_typed_array_transform_map_write(site, state, value)?;
        self.advance_typed_array_transform(site, state)
    }

    /// Runs synchronous reads/writes in a loop and yields only at observable JS boundaries.
    fn advance_typed_array_transform(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        loop {
            self.root_typed_array_transform_state(site, state)?;
            let pending = self.native_call_state_snapshot(state)?;
            let length = typed_array_transform_integer(pending.values[TRANSFORM_LENGTH])?;
            let cursor = typed_array_transform_integer(pending.values[TRANSFORM_CURSOR])?;
            let (output, kind) = self.typed_array_transform_output(state)?;
            if kind == TypedArrayTransformKind::FilterCopy {
                if cursor >= length {
                    let target = self.native_call_state_snapshot(output)?.values[OUTPUT_TARGET];
                    return self.write(site.caller_base, site.destination, target);
                }
                self.copy_typed_array_filter_element(site, state, cursor)?;
                self.set_typed_array_transform_number(state, TRANSFORM_CURSOR, cursor + 1)?;
                continue;
            }
            if cursor >= length {
                if kind == TypedArrayTransformKind::FilterScan {
                    return self.begin_typed_array_filter_species(site, state, output);
                }
                let target = self.native_call_state_snapshot(output)?.values[OUTPUT_TARGET];
                return self.write(site.caller_base, site.destination, target);
            }
            self.set_typed_array_transform_number(state, TRANSFORM_CURSOR, cursor + 1)?;
            let element =
                self.typed_array_transform_element(pending.values[TRANSFORM_SOURCE], cursor)?;
            let Some(returned) =
                self.call_typed_array_transform_callback(site, state, output, element, cursor)?
            else {
                return Ok(());
            };
            if self.finish_typed_array_transform_callback(site, state, returned, element)? {
                return Ok(());
            }
        }
    }

    /// Reads one source element while mapping post-callback detach/OOB to undefined.
    fn typed_array_transform_element(
        &mut self,
        source: Value,
        index: usize,
    ) -> Result<Value, ExecutionError> {
        let snapshot = self.typed_array_snapshot(source)?;
        if index >= snapshot.length {
            return Ok(Value::from_immediate(Immediate::Undefined));
        }
        match self.typed_array_read_element(snapshot, index) {
            Ok(value) => Ok(value),
            Err(ExecutionError::DetachedArrayBuffer) => {
                Ok(Value::from_immediate(Immediate::Undefined))
            }
            Err(error) => Err(error),
        }
    }

    /// Calls `(element, index, source)` while both states and the element remain traced.
    fn call_typed_array_transform_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        output: GcRef<NativeCallState>,
        element: Value,
        index: usize,
    ) -> Result<Option<Value>, ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let output = self.native_call_state_snapshot(output)?;
        self.push_typed_array_transform_parent(
            site,
            state,
            TypedArrayTransformStage::Callback,
            element,
        )?;
        let this_argument = output.values[OUTPUT_THIS_ARGUMENT];
        let prefix = match self.create_apply_argument_prefix(
            pending.values[TRANSFORM_CALLBACK],
            this_argument,
            vec![
                element,
                safe_integer_value(index as u64),
                pending.values[TRANSFORM_SOURCE],
            ],
        ) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: pending.values[TRANSFORM_CALLBACK],
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: 3,
            argument_count: 3,
            this_value: this_argument,
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
                .expect("TypedArray transform callback publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(None);
        }
        self.pop_native_continuation()?;
        self.read(site.caller_base, site.destination).map(Some)
    }

    /// Applies map conversion/write or appends a selected filter element.
    fn finish_typed_array_transform_callback(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        returned: Value,
        element: Value,
    ) -> Result<bool, ExecutionError> {
        let (output, kind) = self.typed_array_transform_output(state)?;
        match kind {
            TypedArrayTransformKind::Map => {
                self.begin_typed_array_transform_map_write(site, state, output, returned)
            }
            TypedArrayTransformKind::FilterScan => {
                if self.is_truthy_value(returned)? {
                    self.append_typed_array_filter_element(site, state, output, element)?;
                }
                Ok(false)
            }
            TypedArrayTransformKind::FilterCopy => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Converts an object-valued map result only when the current target index remains valid.
    fn begin_typed_array_transform_map_write(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        output: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<bool, ExecutionError> {
        self.set_typed_array_transform_value(output, OUTPUT_RETAINED, value)?;
        let index = self.typed_array_transform_current_index(state)?;
        let target = self.native_call_state_snapshot(output)?.values[OUTPUT_TARGET];
        if index >= self.typed_array_snapshot(target)?.length {
            return Ok(false);
        }
        if self.is_object_value(value) {
            self.dispatch_object_primitive_conversion(
                ConversionConsumer::TypedArrayTransformElement,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            )?;
            return Ok(true);
        }
        self.finish_typed_array_transform_map_write(site, state, value)?;
        Ok(false)
    }

    /// Revalidates the target after conversion and commits one mapped primitive if still valid.
    fn finish_typed_array_transform_map_write(
        &mut self,
        _site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let (output, kind) = self.typed_array_transform_output(state)?;
        if kind != TypedArrayTransformKind::Map {
            return Err(ExecutionError::MissingNativeContinuation);
        }
        let index = self.typed_array_transform_current_index(state)?;
        let target_value = self.native_call_state_snapshot(output)?.values[OUTPUT_TARGET];
        let target = self.typed_array_snapshot(target_value)?;
        if index < target.length {
            self.typed_array_write_value(target, index, value)?;
        }
        Ok(())
    }

    /// Stores one kept primitive in a genuine dense Array and advances its exact count.
    fn append_typed_array_filter_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        output: GcRef<NativeCallState>,
        element: Value,
    ) -> Result<(), ExecutionError> {
        self.root_typed_array_transform_state(site, state)?;
        self.set_typed_array_transform_value(output, OUTPUT_RETAINED, element)?;
        let snapshot = self.native_call_state_snapshot(output)?;
        let count = typed_array_transform_integer(snapshot.values[OUTPUT_AUXILIARY])?;
        let key = self.safe_integer_property_atom(count as u64)?;
        let rooted = self.read(site.caller_base, site.destination)?;
        let state = self.native_call_state_reference(rooted)?;
        let (output, kind) = self.typed_array_transform_output(state)?;
        if kind != TypedArrayTransformKind::FilterScan {
            return Err(ExecutionError::MissingNativeContinuation);
        }
        let snapshot = self.native_call_state_snapshot(output)?;
        self.define_data_property(
            snapshot.values[OUTPUT_SELECTED],
            key,
            DataPropertyDescriptor {
                value: Some(snapshot.values[OUTPUT_RETAINED]),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
            },
        )?;
        self.set_typed_array_transform_number(output, OUTPUT_AUXILIARY, count + 1)
    }

    /// Converts the captured count into the filter copy bounds before species lookup overwrites it.
    fn begin_typed_array_filter_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        output: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let count = typed_array_transform_integer(
            self.native_call_state_snapshot(output)?.values[OUTPUT_AUXILIARY],
        )?;
        self.set_typed_array_transform_number(state, TRANSFORM_LENGTH, count)?;
        self.set_typed_array_transform_number(state, TRANSFORM_CURSOR, 0)?;
        self.begin_typed_array_transform_species(site, state)
    }

    /// Begins the observable constructor lookup shared by map and post-scan filter.
    fn begin_typed_array_transform_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let source = self.native_call_state_snapshot(state)?.values[TRANSFORM_SOURCE];
        let constructor = self.constructor_atom()?;
        if let Some(value) = self.dispatch_typed_array_transform_get(
            site,
            state,
            TypedArrayTransformStage::Constructor,
            source,
            constructor.into(),
        )? {
            self.resume_typed_array_transform_constructor(site, state, value)?;
        }
        Ok(())
    }

    /// Observes `@@species` only for an object-valued constructor property.
    fn resume_typed_array_transform_constructor(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        if constructor.as_immediate() == Some(Immediate::Undefined) {
            return self.finish_typed_array_transform_species(site, state, constructor, false);
        }
        if !self.is_object_value(constructor) {
            return Err(ExecutionError::NotObject(constructor));
        }
        let (output, _) = self.typed_array_transform_output(state)?;
        self.set_typed_array_transform_value(output, OUTPUT_AUXILIARY, constructor)?;
        let species = self
            .agent
            .well_known_symbols
            .species
            .expect("Symbol.species initializes before TypedArray transform");
        let species = self.property_key(species)?;
        if let Some(value) = self.dispatch_typed_array_transform_get(
            site,
            state,
            TypedArrayTransformStage::Species,
            constructor,
            species,
        )? {
            self.finish_typed_array_transform_species(site, state, value, true)?;
        }
        Ok(())
    }

    /// Selects the source-kind intrinsic fallback or constructs the observed species.
    fn finish_typed_array_transform_species(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        observed: Value,
        from_species: bool,
    ) -> Result<(), ExecutionError> {
        let constructor = if observed.as_immediate() == Some(Immediate::Undefined)
            || (from_species && observed.as_immediate() == Some(Immediate::Null))
        {
            let source = self.native_call_state_snapshot(state)?.values[TRANSFORM_SOURCE];
            let kind = self.typed_array_snapshot(source)?.kind;
            self.realm.typed_array_constructors[kind.index()]
                .expect("concrete TypedArray constructor initializes before map/filter")
        } else {
            observed
        };
        if !self.is_constructor_value(constructor)? {
            return Err(ExecutionError::NonConstructor(constructor));
        }
        self.construct_typed_array_transform_result(site, state, constructor)
    }

    /// Roots the one-element count prefix across the selected species constructor.
    fn construct_typed_array_transform_result(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        constructor: Value,
    ) -> Result<(), ExecutionError> {
        let (output, _) = self.typed_array_transform_output(state)?;
        self.set_typed_array_transform_value(output, OUTPUT_AUXILIARY, constructor)?;
        let count = self.native_call_state_snapshot(state)?.values[TRANSFORM_LENGTH];
        let undefined = Value::from_immediate(Immediate::Undefined);
        self.push_typed_array_transform_parent(
            site,
            state,
            TypedArrayTransformStage::Construct,
            constructor,
        )?;
        let prefix = match self.create_apply_argument_prefix(constructor, undefined, vec![count]) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        self.pop_native_continuation()?;
        self.push_typed_array_transform_parent(
            site,
            state,
            TypedArrayTransformStage::Construct,
            Value::from_heap_ref(prefix.raw()),
        )?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.construct_site(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: constructor,
            argument_base: 0,
            argument_source: None,
            argument_prefix: Some(prefix),
            argument_prefix_offset: 0,
            argument_prefix_count: 1,
            argument_count: 1,
            this_value: undefined,
            new_target: constructor,
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
                .expect("TypedArray transform species publishes one frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        self.pop_native_continuation()?;
        let result = self.read(site.caller_base, site.destination)?;
        self.finish_typed_array_transform_construct(site, state, result)
    }

    /// Validates target length/content type and starts map scanning or filter copying.
    fn finish_typed_array_transform_construct(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        result: Value,
    ) -> Result<(), ExecutionError> {
        let target = self.validated_typed_array_snapshot(result)?;
        let pending = self.native_call_state_snapshot(state)?;
        let requested = typed_array_transform_integer(pending.values[TRANSFORM_LENGTH])?;
        if target.length < requested {
            return Err(ExecutionError::TypedArraySpeciesResultTooShort);
        }
        let source = self.typed_array_snapshot(pending.values[TRANSFORM_SOURCE])?;
        if source.kind.content_type() != target.kind.content_type() {
            return Err(ExecutionError::TypedArrayContentTypeMismatch);
        }
        let (output, kind) = self.typed_array_transform_output(state)?;
        self.set_typed_array_transform_value(output, OUTPUT_TARGET, result)?;
        if kind == TypedArrayTransformKind::FilterScan {
            self.set_typed_array_transform_mode(output, FILTER_COPY_OUTPUT)?;
        }
        self.advance_typed_array_transform(site, state)
    }

    /// Copies one captured filter primitive into the species target without a second callback.
    fn copy_typed_array_filter_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        index: usize,
    ) -> Result<(), ExecutionError> {
        self.root_typed_array_transform_state(site, state)?;
        let key = self.safe_integer_property_atom(index as u64)?;
        let rooted = self.read(site.caller_base, site.destination)?;
        let state = self.native_call_state_reference(rooted)?;
        let (output, kind) = self.typed_array_transform_output(state)?;
        if kind != TypedArrayTransformKind::FilterCopy {
            return Err(ExecutionError::MissingNativeContinuation);
        }
        let snapshot = self.native_call_state_snapshot(output)?;
        let selected = self
            .get_data_property(snapshot.values[OUTPUT_SELECTED], key)?
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let target_value = snapshot.values[OUTPUT_TARGET];
        let target = self.typed_array_snapshot(target_value)?;
        if index < target.length {
            self.typed_array_write_value(target, index, selected)?;
        }
        Ok(())
    }

    /// Dispatches one Proxy/accessor-aware constructor or species property read.
    fn dispatch_typed_array_transform_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: TypedArrayTransformStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<Value>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_typed_array_transform_parent(site, state, stage, receiver)?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key) {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() <= completion_depth
        {
            return Ok(None);
        }
        self.pop_native_continuation()?;
        self.read(site.caller_base, site.destination).map(Some)
    }

    /// Pushes the transform parent used by property callbacks, callbacks, and construction.
    fn push_typed_array_transform_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: TypedArrayTransformStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::typed_array_transform(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Allocates one fixed state while tracing all pending values through forced collection.
    fn allocate_typed_array_transform_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = TypedArrayTransformRoots {
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
                self.types.native_call_state,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Resolves the output side-state and its compact transform mode.
    fn typed_array_transform_output(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<(GcRef<NativeCallState>, TypedArrayTransformKind), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let raw = pending.values[TRANSFORM_OUTPUT]
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let output = self
            .heap
            .checked_reference(raw, self.types.native_call_state)
            .map_err(|_| ExecutionError::MissingNativeContinuation)?;
        let kind = match self.native_call_state_snapshot(output)?.count {
            MAP_OUTPUT => TypedArrayTransformKind::Map,
            FILTER_SCAN_OUTPUT => TypedArrayTransformKind::FilterScan,
            FILTER_COPY_OUTPUT => TypedArrayTransformKind::FilterCopy,
            _ => return Err(ExecutionError::MissingNativeContinuation),
        };
        Ok((output, kind))
    }

    /// Returns the index committed immediately before the current map callback.
    fn typed_array_transform_current_index(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<usize, ExecutionError> {
        typed_array_transform_integer(
            self.native_call_state_snapshot(state)?.values[TRANSFORM_CURSOR],
        )?
        .checked_sub(1)
        .ok_or(ExecutionError::InvalidArrayLength)
    }

    /// Keeps the movable primary state in the caller destination register.
    #[inline(always)]
    fn root_typed_array_transform_state(
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

    /// Updates one traced state value and publishes the corresponding generational barrier.
    fn set_typed_array_transform_value(
        &mut self,
        state: GcRef<NativeCallState>,
        slot: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .values[slot] = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Stores one exact cursor/count value without creating a managed edge.
    fn set_typed_array_transform_number(
        &mut self,
        state: GcRef<NativeCallState>,
        slot: usize,
        value: usize,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .values[slot] = safe_integer_value(value as u64);
                Ok(())
            })
        })
    }

    /// Switches the filter side-state from callback capture to dense copy.
    fn set_typed_array_transform_mode(
        &mut self,
        state: GcRef<NativeCallState>,
        mode: u8,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .count = mode;
                Ok(())
            })
        })
    }
}

#[inline(always)]
fn typed_array_transform_integer(value: Value) -> Result<usize, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::InvalidArrayLength)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > usize::MAX as f64 {
        return Err(ExecutionError::InvalidArrayLength);
    }
    Ok(number as usize)
}
