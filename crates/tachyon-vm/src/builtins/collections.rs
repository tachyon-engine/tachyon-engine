//! Map and Set private-slot operations over fixed-capacity ordered storage.

use super::super::*;
use crate::collection::{
    CollectionEntry, PendingCollectionInitializerRoots, PendingCollectionInitializerSnapshot,
};

struct CollectionReplacementRoots<'a> {
    vm: VmRoots<'a>,
    receiver: Value,
    storage: GcRef<OrderedCollection>,
}

impl Trace for CollectionReplacementRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.receiver.trace(tracer);
        self.storage.trace(tracer);
    }
}

impl Isolate {
    #[inline(always)]
    pub(crate) fn is_map_value(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.map_object)
                .is_ok()
        })
    }

    #[inline(always)]
    pub(crate) fn is_set_value(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.set_object)
                .is_ok()
        })
    }

    /// Starts Map construction, retaining its initialization record across iterable callbacks.
    pub(crate) fn begin_map_from_site(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let target = self.allocate_map_object(
            self.realm
                .map_prototype
                .expect("Map prototype initializes before Map construction"),
        )?;
        self.begin_collection_initializer(site, target, CollectionInitializerKind::Map)
    }

    /// Starts Set construction, retaining its initialization record across iterable callbacks.
    pub(crate) fn begin_set_from_site(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let target = self.allocate_set_object(
            self.realm
                .set_prototype
                .expect("Set prototype initializes before Set construction"),
        )?;
        self.begin_collection_initializer(site, target, CollectionInitializerKind::Set)
    }

    /// Starts Object.fromEntries with the same resumable iterator protocol as Map.
    pub(crate) fn begin_object_from_entries(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let iterable = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if is_nullish(iterable) {
            return Err(ExecutionError::NotObject(iterable));
        }
        let target = self.create_ordinary_object()?;
        self.begin_collection_initializer(
            site,
            target,
            CollectionInitializerKind::ObjectFromEntries,
        )
    }

    /// Starts Object.groupBy with a null-prototype result and a resumable callback state.
    pub(crate) fn begin_object_group_by(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let items = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if is_nullish(items) {
            return Err(ExecutionError::NotObject(items));
        }
        let callback = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.resolve_function_object(callback)?;
        let iterable = if self.is_string_value(items) {
            let prototype = self
                .realm
                .array_prototype
                .expect("Array prototype initializes before string groupBy");
            let array = self.create_array_object_with_prototype(prototype)?;
            let length = self.string_value_length(items)?;
            let mut source_index = 0_usize;
            let mut output_index = 0_u64;
            while source_index < length {
                let (value, next_index) = self
                    .string_code_point_value_at(items, source_index)?
                    .expect("bounded String iterator index");
                let key = PropertyKey::Atom(self.safe_integer_property_atom(output_index)?);
                self.set_own_data_property(array, key, value)?;
                source_index = next_index;
                output_index = output_index
                    .checked_add(1)
                    .ok_or(ExecutionError::ArrayLengthOverflow)?;
            }
            self.set_array_length_value(array, safe_integer_value(output_index))?;
            array
        } else {
            items
        };
        let target =
            self.create_ordinary_object_with_prototype(Value::from_immediate(Immediate::Null))?;
        self.begin_collection_initializer_with_iterable(
            site,
            target,
            CollectionInitializerKind::ObjectGroupBy,
            iterable,
        )
    }

    /// Starts WeakMap construction with the same resumable iterable protocol as Map.
    pub(crate) fn begin_weak_map_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let target = self.allocate_weak_map_object(
            self.realm
                .weak_map_prototype
                .expect("WeakMap prototype initializes before construction"),
        )?;
        self.begin_collection_initializer(site, target, CollectionInitializerKind::WeakMap)
    }

    /// Starts WeakSet construction with the same resumable iterable protocol as Set.
    pub(crate) fn begin_weak_set_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let target = self.allocate_weak_set_object(
            self.realm
                .weak_set_prototype
                .expect("WeakSet prototype initializes before construction"),
        )?;
        self.begin_collection_initializer(site, target, CollectionInitializerKind::WeakSet)
    }

    /// Runs the Map/Set constructor protocol without representing JavaScript calls on the Rust stack.
    fn begin_collection_initializer(
        &mut self,
        site: &CallSite,
        target: Value,
        kind: CollectionInitializerKind,
    ) -> Result<(), ExecutionError> {
        let iterable = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.begin_collection_initializer_with_iterable(site, target, kind, iterable)
    }

    /// Starts a collection initializer after the host has normalized a primitive iterable.
    fn begin_collection_initializer_with_iterable(
        &mut self,
        site: &CallSite,
        target: Value,
        kind: CollectionInitializerKind,
        iterable: Value,
    ) -> Result<(), ExecutionError> {
        if is_nullish(iterable) {
            return self.write(site.caller_base, site.destination, target);
        }
        let group_by_callback = if kind == CollectionInitializerKind::ObjectGroupBy {
            self.call_argument(site, 1)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined))
        } else {
            Value::from_immediate(Immediate::Undefined)
        };
        let state = self.allocate_pending_collection_initializer(PendingCollectionInitializer {
            target,
            iterable,
            iterator: Value::from_immediate(Immediate::Undefined),
            next: Value::from_immediate(Immediate::Undefined),
            result: Value::from_immediate(Immediate::Undefined),
            key: Value::from_immediate(Immediate::Undefined),
            adder: group_by_callback,
            kind,
            stage: CollectionInitializerStage::Adder,
            index: 0,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if matches!(
            kind,
            CollectionInitializerKind::ObjectFromEntries | CollectionInitializerKind::ObjectGroupBy
        ) {
            return self.resume_collection_initializer(
                native_site,
                state,
                CollectionInitializerStage::Adder,
                Value::from_immediate(Immediate::Undefined),
            );
        }
        self.get_collection_initializer_property(
            native_site,
            state,
            CollectionInitializerStage::Adder,
            target,
            if matches!(
                kind,
                CollectionInitializerKind::Map | CollectionInitializerKind::WeakMap
            ) {
                b"set"
            } else {
                b"add"
            },
        )
    }

    /// Resumes one observable iterable-constructor protocol operation from its returned value.
    pub(crate) fn resume_collection_initializer(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionInitializer>,
        stage: CollectionInitializerStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        // The parent completion has been popped before this resume. Re-publish the state before
        // any allocation so local GcRef/Value temporaries never become the only owner.
        self.update_pending_collection_initializer(state, |pending| pending.stage = stage)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        match stage {
            CollectionInitializerStage::Adder => {
                let kind = self.pending_collection_initializer(state)?.kind;
                if kind != CollectionInitializerKind::ObjectGroupBy {
                    self.update_pending_collection_initializer(state, |pending| {
                        pending.adder = value
                    })?;
                }
                if !matches!(
                    kind,
                    CollectionInitializerKind::ObjectFromEntries
                        | CollectionInitializerKind::ObjectGroupBy
                ) {
                    self.resolve_function_object(value)?;
                }
                let iterable = self.pending_collection_initializer(state)?.iterable;
                let iterator_symbol = self
                    .realm
                    .well_known_symbols
                    .iterator
                    .expect("Symbol.iterator initializes before collection construction");
                let iterator_key = self.property_key(iterator_symbol)?;
                self.get_collection_initializer_key(
                    site,
                    state,
                    CollectionInitializerStage::IteratorMethod,
                    iterable,
                    iterator_key,
                )
            }
            CollectionInitializerStage::IteratorMethod => {
                self.resolve_function_object(value)?;
                self.update_pending_collection_initializer(state, |pending| pending.next = value)?;
                let pending = self.pending_collection_initializer(state)?;
                self.call_collection_initializer(
                    site,
                    state,
                    CollectionInitializerStage::IteratorCall,
                    pending.next,
                    pending.iterable,
                    &[],
                )
            }
            CollectionInitializerStage::IteratorCall => {
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                self.update_pending_collection_initializer(state, |pending| {
                    pending.iterator = value
                })?;
                self.get_collection_initializer_property(
                    site,
                    state,
                    CollectionInitializerStage::NextMethod,
                    value,
                    b"next",
                )
            }
            CollectionInitializerStage::NextMethod => {
                self.update_pending_collection_initializer(state, |pending| pending.next = value)?;
                self.resolve_function_object(value)?;
                self.call_collection_next(site, state)
            }
            CollectionInitializerStage::NextCall => {
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                self.update_pending_collection_initializer(state, |pending| {
                    pending.result = value
                })?;
                self.get_collection_initializer_property(
                    site,
                    state,
                    CollectionInitializerStage::ResultDone,
                    value,
                    b"done",
                )
            }
            CollectionInitializerStage::ResultDone => {
                if self.is_truthy_value(value)? {
                    let target = self.pending_collection_initializer(state)?.target;
                    return self.write(site.caller_base, site.destination, target);
                }
                let result = self.pending_collection_initializer(state)?.result;
                self.get_collection_initializer_property(
                    site,
                    state,
                    CollectionInitializerStage::ResultValue,
                    result,
                    b"value",
                )
            }
            CollectionInitializerStage::ResultValue => {
                let pending = self.pending_collection_initializer(state)?;
                if pending.kind == CollectionInitializerKind::ObjectGroupBy {
                    let index = pending.index;
                    self.update_pending_collection_initializer(state, |pending| {
                        pending.result = value;
                        pending.index = pending.index.saturating_add(1);
                    })?;
                    return self.call_collection_initializer(
                        site,
                        state,
                        CollectionInitializerStage::GroupByCallback,
                        pending.adder,
                        Value::from_immediate(Immediate::Undefined),
                        &[value, safe_integer_value(index)],
                    );
                }
                if matches!(
                    pending.kind,
                    CollectionInitializerKind::Set | CollectionInitializerKind::WeakSet
                ) {
                    self.update_pending_collection_initializer(state, |pending| {
                        pending.result = value
                    })?;
                    let value = self.pending_collection_initializer(state)?.result;
                    return self.call_collection_adder(site, state, &[value]);
                }
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                self.update_pending_collection_initializer(state, |pending| {
                    pending.result = value
                })?;
                self.get_collection_initializer_property(
                    site,
                    state,
                    CollectionInitializerStage::EntryKey,
                    value,
                    b"0",
                )
            }
            CollectionInitializerStage::EntryKey => {
                self.update_pending_collection_initializer(state, |pending| pending.key = value)?;
                let entry = self.pending_collection_initializer(state)?.result;
                self.get_collection_initializer_property(
                    site,
                    state,
                    CollectionInitializerStage::EntryValue,
                    entry,
                    b"1",
                )
            }
            CollectionInitializerStage::EntryValue => {
                self.update_pending_collection_initializer(state, |pending| {
                    pending.result = value
                })?;
                let pending = self.pending_collection_initializer(state)?;
                if pending.kind == CollectionInitializerKind::ObjectFromEntries {
                    return self.dispatch_builtin_property_key_native(
                        site,
                        BuiltinPropertyKeyConsumer::ObjectFromEntries,
                        pending.target,
                        pending.key,
                        pending.result,
                        Value::from_heap_ref(state.raw()),
                    );
                }
                self.call_collection_adder(site, state, &[pending.key, pending.result])
            }
            CollectionInitializerStage::GroupByCallback => {
                let pending = self.pending_collection_initializer(state)?;
                self.dispatch_builtin_property_key_native(
                    site,
                    BuiltinPropertyKeyConsumer::ObjectGroupBy,
                    pending.target,
                    value,
                    pending.result,
                    Value::from_heap_ref(state.raw()),
                )
            }
            CollectionInitializerStage::AdderCall => self.call_collection_next(site, state),
        }
    }

    /// Calls the cached iterator `next` without re-reading the method between iterations.
    fn call_collection_next(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionInitializer>,
    ) -> Result<(), ExecutionError> {
        self.update_pending_collection_initializer(state, |pending| {
            pending.stage = CollectionInitializerStage::NextCall
        })?;
        let pending = self.pending_collection_initializer(state)?;
        self.call_collection_initializer(
            site,
            state,
            CollectionInitializerStage::NextCall,
            pending.next,
            pending.iterator,
            &[],
        )
    }

    /// Calls the cached `set` or `add` exactly as the constructor's one-time adder lookup requires.
    fn call_collection_adder(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionInitializer>,
        arguments: &[Value],
    ) -> Result<(), ExecutionError> {
        let pending = self.pending_collection_initializer(state)?;
        self.call_collection_initializer(
            site,
            state,
            CollectionInitializerStage::AdderCall,
            pending.adder,
            pending.target,
            arguments,
        )
    }

    /// Reads one named protocol property and suspends only when it is an accessor callback.
    fn get_collection_initializer_property(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionInitializer>,
        stage: CollectionInitializerStage,
        receiver: Value,
        name: &[u8],
    ) -> Result<(), ExecutionError> {
        let key = PropertyKey::Atom(self.intern_intrinsic_name(name)?);
        self.get_collection_initializer_key(site, state, stage, receiver, key)
    }

    /// Reads one arbitrary protocol key, preserving ordinary accessor receiver semantics.
    fn get_collection_initializer_key(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionInitializer>,
        stage: CollectionInitializerStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        match self.resolve_property_read(receiver, key)? {
            PropertyRead::Data(value) => {
                self.resume_collection_initializer(site, state, stage, value)
            }
            PropertyRead::Missing => self.resume_collection_initializer(
                site,
                state,
                stage,
                Value::from_immediate(Immediate::Undefined),
            ),
            PropertyRead::Accessor(callee)
                if callee.as_immediate() == Some(Immediate::Undefined) =>
            {
                self.resume_collection_initializer(
                    site,
                    state,
                    stage,
                    Value::from_immediate(Immediate::Undefined),
                )
            }
            PropertyRead::Accessor(callee) => {
                self.call_collection_initializer(site, state, stage, callee, receiver, &[])
            }
        }
    }

    /// Publishes the collection state before a protocol call and handles immediate native results.
    fn call_collection_initializer(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingCollectionInitializer>,
        stage: CollectionInitializerStage,
        callee: Value,
        receiver: Value,
        arguments: &[Value],
    ) -> Result<(), ExecutionError> {
        self.update_pending_collection_initializer(state, |pending| pending.stage = stage)?;
        let mut copied_arguments = Vec::new();
        copied_arguments
            .try_reserve_exact(arguments.len())
            .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
        copied_arguments.extend_from_slice(arguments);
        let prefix = if copied_arguments.is_empty() {
            None
        } else {
            Some(self.create_apply_argument_prefix(callee, receiver, copied_arguments)?)
        };
        let continuation = NativeContinuation::collection_initializer(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            callee,
        );
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(|_| ExecutionError::CompletionAllocationFailed)?;
        let frame_depth = self.fiber.frames.len();
        let call_result = self.call(CallSite {
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
        });
        if let Err(error) = call_result {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("a collection protocol call publishes its callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let returned = self.read(site.caller_base, site.destination)?;
        let _continuation = self.pop_native_continuation()?;
        self.resume_collection_initializer(site, state, stage, returned)
    }

    /// Allocates the small traced protocol record used by resumable collection construction.
    fn allocate_pending_collection_initializer(
        &mut self,
        pending: PendingCollectionInitializer,
    ) -> Result<GcRef<PendingCollectionInitializer>, ExecutionError> {
        let mut roots = PendingCollectionInitializerRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            pending,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_collection_initializer,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_collection_initializer_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingCollectionInitializer>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_collection_initializer)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Defines one converted fromEntries key and resumes the cached iterator.
    pub(crate) fn finish_object_from_entries_key(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        key: PropertyKey,
        value: Value,
        state: Value,
    ) -> Result<(), ExecutionError> {
        self.set_own_data_property(target, key, value)?;
        let state = self.pending_collection_initializer_reference(state)?;
        self.call_collection_next(site, state)
    }

    /// Appends one value to the null-prototype group's array and advances the iterable.
    pub(crate) fn finish_object_group_by_key(
        &mut self,
        site: NativeContinuationSite,
        target: Value,
        key: PropertyKey,
        value: Value,
        state: Value,
    ) -> Result<(), ExecutionError> {
        let group = match self.resolve_property_read(target, key)? {
            PropertyRead::Data(group) => group,
            PropertyRead::Missing => {
                let prototype = self
                    .realm
                    .array_prototype
                    .expect("Array prototype initializes before groupBy");
                let group = self.create_array_object_with_prototype(prototype)?;
                self.set_own_data_property(target, key, group)?;
                group
            }
            PropertyRead::Accessor(_) => return Err(ExecutionError::UnsupportedAccessorDescriptor),
        };
        if !self.is_array_value(group)? {
            return Err(ExecutionError::InvalidPropertyDescriptor(group));
        }
        let length_key = PropertyKey::Atom(self.length_atom()?);
        let length = self
            .get_data_property(group, length_key)?
            .and_then(numeric_value)
            .ok_or(ExecutionError::ArrayLengthOverflow)?;
        let index_key = PropertyKey::Atom(self.safe_integer_property_atom(length as u64)?);
        self.set_own_data_property(group, index_key, value)?;
        self.set_array_length_value(group, Value::from_f64(length + 1.0))?;
        let state = self.pending_collection_initializer_reference(state)?;
        self.call_collection_next(site, state)
    }

    /// Reports whether an abrupt iterable-consumer stage requires IteratorClose.
    pub(crate) fn should_close_collection_initializer(
        &mut self,
        state: Value,
        _continuation_stage: CollectionInitializerStage,
    ) -> Result<bool, ExecutionError> {
        let state = self.pending_collection_initializer_reference(state)?;
        let pending = self.pending_collection_initializer(state)?;
        let stage = pending.stage;
        Ok(match pending.kind {
            CollectionInitializerKind::ObjectFromEntries => matches!(
                stage,
                CollectionInitializerStage::ResultValue
                    | CollectionInitializerStage::EntryKey
                    | CollectionInitializerStage::EntryValue
            ),
            CollectionInitializerKind::ObjectGroupBy => {
                stage == CollectionInitializerStage::GroupByCallback
            }
            CollectionInitializerKind::Map
            | CollectionInitializerKind::Set
            | CollectionInitializerKind::WeakMap
            | CollectionInitializerKind::WeakSet => false,
        })
    }

    /// Starts IteratorClose while retaining the original abrupt completion outside Rust's stack.
    pub(crate) fn begin_collection_iterator_close(
        &mut self,
        site: NativeContinuationSite,
        state: Value,
        original_throw: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let reference = self.pending_collection_initializer_reference(state)?;
        let iterator = self.pending_collection_initializer(reference)?.iterator;
        self.begin_iterator_close(site, iterator, original_throw)
    }

    /// Starts IteratorClose for any native iterable consumer with a rooted iterator identity.
    pub(crate) fn begin_iterator_close(
        &mut self,
        site: NativeContinuationSite,
        iterator: Value,
        original_throw: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let return_key = PropertyKey::Atom(self.intern_intrinsic_name(b"return")?);
        match self.resolve_property_read(iterator, return_key)? {
            PropertyRead::Missing => self.throw_value(original_throw, site.call_site),
            PropertyRead::Data(callee)
                if matches!(
                    callee.as_immediate(),
                    Some(Immediate::Undefined | Immediate::Null)
                ) =>
            {
                self.throw_value(original_throw, site.call_site)
            }
            PropertyRead::Accessor(getter)
                if getter.as_immediate() == Some(Immediate::Undefined) =>
            {
                self.throw_value(original_throw, site.call_site)
            }
            PropertyRead::Accessor(getter) => self.call_collection_iterator_close(
                site,
                CollectionIteratorCloseStage::ReturnGetter,
                iterator,
                original_throw,
                getter,
                iterator,
            ),
            PropertyRead::Data(callee) => self.call_collection_iterator_close(
                site,
                CollectionIteratorCloseStage::ReturnCall,
                iterator,
                original_throw,
                callee,
                iterator,
            ),
        }
    }

    /// Resumes either the `return` getter or call, restoring the original throw after the call.
    pub(crate) fn resume_collection_iterator_close(
        &mut self,
        continuation: NativeContinuation,
        stage: CollectionIteratorCloseStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let site = continuation.site();
        let original_throw = continuation.second();
        match stage {
            CollectionIteratorCloseStage::ReturnGetter => {
                if matches!(
                    value.as_immediate(),
                    Some(Immediate::Undefined | Immediate::Null)
                ) {
                    return self.throw_value(original_throw, site.call_site);
                }
                let iterator = continuation.first();
                self.call_collection_iterator_close(
                    site,
                    CollectionIteratorCloseStage::ReturnCall,
                    iterator,
                    original_throw,
                    value,
                    iterator,
                )
            }
            CollectionIteratorCloseStage::ReturnCall => {
                self.throw_value(original_throw, site.call_site)
            }
        }
    }

    /// Calls one IteratorClose callback and marks JavaScript frames for continuation return.
    fn call_collection_iterator_close(
        &mut self,
        site: NativeContinuationSite,
        stage: CollectionIteratorCloseStage,
        iterator: Value,
        original_throw: Value,
        callee: Value,
        receiver: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.resolve_function_object(callee)?;
        self.fiber
            .completions
            .push_native(NativeContinuation::collection_iterator_close(
                site,
                stage,
                iterator,
                original_throw,
            ))
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee,
            argument_base: 0,
            argument_source: None,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 0,
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
                .expect("IteratorClose callback publishes its callee frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(None);
        }
        let returned = self.read(site.caller_base, site.destination)?;
        let continuation = self.pop_native_continuation()?;
        self.resume_collection_iterator_close(continuation, stage, returned)
    }

    fn pending_collection_initializer(
        &mut self,
        state: GcRef<PendingCollectionInitializer>,
    ) -> Result<PendingCollectionInitializerSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_collection_initializer)
                    .map(|pending| PendingCollectionInitializerSnapshot {
                        target: pending.target,
                        iterable: pending.iterable,
                        iterator: pending.iterator,
                        next: pending.next,
                        result: pending.result,
                        key: pending.key,
                        adder: pending.adder,
                        kind: pending.kind,
                        stage: pending.stage,
                        index: pending.index,
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn update_pending_collection_initializer(
        &mut self,
        state: GcRef<PendingCollectionInitializer>,
        update: impl FnOnce(&mut PendingCollectionInitializer),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_collection_initializer)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Implements Map.prototype.get using SameValueZero over the exotic's private insertion list.
    pub(crate) fn map_get(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let key = self.collection_key(argument);
        let storage = self.map_storage(site.this_value)?;
        let value = match self.collection_find(storage, key)? {
            Some(index) => self
                .collection_entry(storage, index)?
                .map(|entry| entry.value),
            None => None,
        };
        Ok(value.unwrap_or(Value::from_immediate(Immediate::Undefined)))
    }

    /// Returns an existing Map value or inserts the supplied default with one SameValueZero probe.
    pub(crate) fn map_get_or_insert(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let key = self.call_argument(site, 0)?;
        let key = self.collection_key(key);
        let default = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let storage = self.map_storage(site.this_value)?;
        if let Some(index) = self.collection_find(storage, key)? {
            return self
                .collection_entry(storage, index)?
                .map(|entry| entry.value)
                .ok_or(ExecutionError::CollectionStorageAllocationFailed);
        }
        let storage = self.ensure_map_capacity(site.this_value, storage)?;
        self.collection_append(storage, key, default)?;
        Ok(default)
    }

    /// Implements Map.prototype.set, including canonical -0 keys and replacement backing growth.
    pub(crate) fn map_set(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let key = self.collection_key(argument);
        let value = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let storage = self.map_storage(site.this_value)?;
        if let Some(index) = self.collection_find(storage, key)? {
            self.collection_update(storage, index, value)?;
        } else {
            let storage = self.ensure_map_capacity(site.this_value, storage)?;
            self.collection_append(storage, key, value)?;
        }
        Ok(site.this_value)
    }

    /// Implements Map.prototype.has without materializing a result-array or iterator.
    pub(crate) fn map_has(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let key = self.collection_key(argument);
        let storage = self.map_storage(site.this_value)?;
        self.collection_find(storage, key)
            .map(|entry| entry.is_some())
    }

    /// Implements Map.prototype.delete while retaining a tombstone for live iterator cursors.
    pub(crate) fn map_delete(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let key = self.collection_key(argument);
        let storage = self.map_storage(site.this_value)?;
        let Some(index) = self.collection_find(storage, key)? else {
            return Ok(false);
        };
        self.collection_delete(storage, index)?;
        Ok(true)
    }

    /// Implements Map.prototype.clear without shortening the physical cursor list.
    pub(crate) fn map_clear(&mut self, receiver: Value) -> Result<(), ExecutionError> {
        let storage = self.map_storage(receiver)?;
        self.collection_clear(storage)
    }

    /// Reads Map.prototype.size from the private live-entry count.
    pub(crate) fn map_size(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        let storage = self.map_storage(receiver)?;
        Ok(safe_integer_value(u64::from(self.collection_len(storage)?)))
    }

    /// Creates a Map keys iterator retaining live insertion-order state.
    pub(crate) fn map_keys(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.map_storage(receiver)?;
        self.create_collection_iterator(receiver, CollectionIterationKind::Key, true)
    }

    /// Creates a Map values iterator retaining live insertion-order state.
    pub(crate) fn map_values(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.map_storage(receiver)?;
        self.create_collection_iterator(receiver, CollectionIterationKind::Value, true)
    }

    /// Creates a Map entries iterator retaining live insertion-order state.
    pub(crate) fn map_entries(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.map_storage(receiver)?;
        self.create_collection_iterator(receiver, CollectionIterationKind::KeyAndValue, true)
    }

    /// Implements Set.prototype.add by storing each member as both ordered key and value.
    pub(crate) fn set_add(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let value = self.collection_key(argument);
        let storage = self.set_storage(site.this_value)?;
        if self.collection_find(storage, value)?.is_none() {
            let storage = self.ensure_set_capacity(site.this_value, storage)?;
            self.collection_append(storage, value, value)?;
        }
        Ok(site.this_value)
    }

    /// Implements Set.prototype.has with the same canonicalized SameValueZero key path as add.
    pub(crate) fn set_has(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let value = self.collection_key(argument);
        let storage = self.set_storage(site.this_value)?;
        self.collection_find(storage, value)
            .map(|entry| entry.is_some())
    }

    /// Implements Set.prototype.delete while preserving physical positions for future iterators.
    pub(crate) fn set_delete(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let argument = self.call_argument(site, 0)?;
        let value = self.collection_key(argument);
        let storage = self.set_storage(site.this_value)?;
        let Some(index) = self.collection_find(storage, value)? else {
            return Ok(false);
        };
        self.collection_delete(storage, index)?;
        Ok(true)
    }

    /// Implements Set.prototype.clear without shrinking the fixed GC-accounted backing.
    pub(crate) fn set_clear(&mut self, receiver: Value) -> Result<(), ExecutionError> {
        let storage = self.set_storage(receiver)?;
        self.collection_clear(storage)
    }

    /// Reads Set.prototype.size from the private live-entry count.
    pub(crate) fn set_size(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        let storage = self.set_storage(receiver)?;
        Ok(safe_integer_value(u64::from(self.collection_len(storage)?)))
    }

    /// Creates a Set values iterator retaining live insertion-order state.
    pub(crate) fn set_values(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.set_storage(receiver)?;
        self.create_collection_iterator(receiver, CollectionIterationKind::Value, false)
    }

    /// Creates a Set entries iterator yielding `[value, value]` pairs.
    pub(crate) fn set_entries(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.set_storage(receiver)?;
        self.create_collection_iterator(receiver, CollectionIterationKind::KeyAndValue, false)
    }

    #[inline(always)]
    pub(crate) fn collection_key(&self, value: Option<Value>) -> Value {
        let value = value.unwrap_or(Value::from_immediate(Immediate::Undefined));
        if numeric_value(value).is_some_and(|number| number == 0.0) {
            Value::from_f64(0.0)
        } else {
            value
        }
    }

    pub(crate) fn map_storage(
        &mut self,
        receiver: Value,
    ) -> Result<GcRef<OrderedCollection>, ExecutionError> {
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::IncompatibleCollectionReceiver(receiver))?;
        let map = self
            .heap
            .checked_reference(raw, self.types.map_object)
            .map_err(|_| ExecutionError::IncompatibleCollectionReceiver(receiver))?;
        self.heap.with_running_scope(|scope| {
            let map = scope.root(map).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(map, self.types.map_object)
                    .map(|map| map.storage)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(crate) fn set_storage(
        &mut self,
        receiver: Value,
    ) -> Result<GcRef<OrderedCollection>, ExecutionError> {
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::IncompatibleCollectionReceiver(receiver))?;
        let set = self
            .heap
            .checked_reference(raw, self.types.set_object)
            .map_err(|_| ExecutionError::IncompatibleCollectionReceiver(receiver))?;
        self.heap.with_running_scope(|scope| {
            let set = scope.root(set).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(set, self.types.set_object)
                    .map(|set| set.storage)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Scans physical entries with borrow-free SameValueZero comparisons between reads.
    pub(crate) fn collection_find(
        &mut self,
        storage: GcRef<OrderedCollection>,
        key: Value,
    ) -> Result<Option<u32>, ExecutionError> {
        let used = self.collection_used(storage)?;
        for index in 0..used {
            if let Some(entry) = self.collection_entry(storage, index)?
                && self.same_value_zero(entry.key, key)?
            {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    pub(crate) fn collection_entry(
        &mut self,
        storage: GcRef<OrderedCollection>,
        index: u32,
    ) -> Result<Option<CollectionEntry>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.ordered_collection)
                    .map(|storage| storage.entry_at(index))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(crate) fn collection_used(
        &mut self,
        storage: GcRef<OrderedCollection>,
    ) -> Result<u32, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.ordered_collection)
                    .map(|storage| storage.used())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn collection_len(&mut self, storage: GcRef<OrderedCollection>) -> Result<u32, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.ordered_collection)
                    .map(|storage| storage.len())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(crate) fn collection_update(
        &mut self,
        storage: GcRef<OrderedCollection>,
        index: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(storage, self.types.ordered_collection)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .update_at(index, value)
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)
            })?;
            scope
                .write_value_barrier(storage, value)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    pub(crate) fn collection_append(
        &mut self,
        storage: GcRef<OrderedCollection>,
        key: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(storage, self.types.ordered_collection)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .append(key, value)
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)
            })?;
            scope
                .write_value_barrier(storage, key)
                .map_err(ExecutionError::HeapReference)?;
            scope
                .write_value_barrier(storage, value)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    fn collection_delete(
        &mut self,
        storage: GcRef<OrderedCollection>,
        index: u32,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let storage = no_gc
                    .borrow_mut(storage, self.types.ordered_collection)
                    .map_err(ExecutionError::NoGcBorrow)?;
                storage
                    .delete_at(index)
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)
            })
        })
    }

    fn collection_clear(
        &mut self,
        storage: GcRef<OrderedCollection>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(storage, self.types.ordered_collection)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .clear();
                Ok(())
            })
        })
    }

    pub(crate) fn ensure_map_capacity(
        &mut self,
        receiver: Value,
        storage: GcRef<OrderedCollection>,
    ) -> Result<GcRef<OrderedCollection>, ExecutionError> {
        self.ensure_collection_capacity(receiver, storage, true)
    }

    fn ensure_set_capacity(
        &mut self,
        receiver: Value,
        storage: GcRef<OrderedCollection>,
    ) -> Result<GcRef<OrderedCollection>, ExecutionError> {
        self.ensure_collection_capacity(receiver, storage, false)
    }

    /// Allocates an exactly charged replacement backing and publishes it into the owning exotic.
    fn ensure_collection_capacity(
        &mut self,
        receiver: Value,
        storage: GcRef<OrderedCollection>,
        map: bool,
    ) -> Result<GcRef<OrderedCollection>, ExecutionError> {
        let (used, capacity) = self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.ordered_collection)
                    .map(|storage| (storage.used(), storage.capacity()))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        if (used as usize) < capacity {
            return Ok(storage);
        }
        let capacity = tuning::collections::grown_entry_capacity(capacity)
            .ok_or(ExecutionError::CollectionStorageAllocationFailed)?;
        let replacement = self.heap.with_running_scope(|scope| {
            let storage = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(storage, self.types.ordered_collection)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .grow_copy(capacity)
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)
            })
        })?;
        let mut roots = CollectionReplacementRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            receiver,
            storage,
        };
        let replacement = self
            .heap
            .try_allocate_external_with_gc(
                self.types.ordered_collection,
                0,
                replacement,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::IncompatibleCollectionReceiver(receiver))?;
        if map {
            let object = self
                .heap
                .checked_reference(raw, self.types.map_object)
                .map_err(|_| ExecutionError::IncompatibleCollectionReceiver(receiver))?;
            self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                let replacement = scope.root(replacement).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(object, self.types.map_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .storage = replacement.as_gc_ref();
                    Ok(())
                })?;
                scope
                    .write_barrier(object, replacement)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            })?;
        } else {
            let object = self
                .heap
                .checked_reference(raw, self.types.set_object)
                .map_err(|_| ExecutionError::IncompatibleCollectionReceiver(receiver))?;
            self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                let replacement = scope.root(replacement).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow_mut(object, self.types.set_object)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .storage = replacement.as_gc_ref();
                    Ok(())
                })?;
                scope
                    .write_barrier(object, replacement)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            })?;
        }
        Ok(replacement)
    }
}
