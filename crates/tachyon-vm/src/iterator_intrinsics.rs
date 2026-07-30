//! Intrinsic `Iterator` constructor and proposal-defined prototype accessors.

use super::*;

struct WrapForValidIteratorAllocationRoots<'a> {
    vm: VmRoots<'a>,
    iterator: Value,
    next_method: Value,
    prototype: Value,
}

impl Trace for WrapForValidIteratorAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.iterator.trace(tracer);
        self.next_method.trace(tracer);
        self.prototype.trace(tracer);
    }
}

impl Isolate {
    /// Starts GetIteratorFlattenable in iterate-string-primitives mode.
    pub(crate) fn begin_iterator_from(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let source = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(source) && !self.is_string_value(source) {
            return Err(ExecutionError::NotObject(source));
        }
        let symbol = self
            .agent
            .well_known_symbols
            .iterator
            .expect("Symbol.iterator initializes before Iterator.from");
        let key = self.property_key(symbol)?;
        self.dispatch_iterator_from_get(
            Self::native_site(site),
            IteratorFromStage::IteratorMethodGet,
            source,
            Value::from_immediate(Immediate::Undefined),
            source,
            key,
        )
    }

    /// Resumes one observable step of Iterator.from without a Rust-stack callback chain.
    pub(crate) fn resume_iterator_from(
        &mut self,
        continuation: NativeContinuation,
        stage: IteratorFromStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        match stage {
            IteratorFromStage::IteratorMethodGet => {
                let source = continuation.first();
                if is_nullish(value) {
                    return self.continue_iterator_from_record(site, source);
                }
                self.resolve_function_object(value)?;
                self.dispatch_property_callback(
                    NativeContinuation::iterator_from(
                        site,
                        IteratorFromStage::IteratorMethodCall,
                        source,
                        value,
                    ),
                    value,
                )
                .map(|_| ())
            }
            IteratorFromStage::IteratorMethodCall => {
                self.continue_iterator_from_record(site, value)
            }
            IteratorFromStage::NextGet => {
                self.begin_iterator_from_has_instance(site, continuation.first(), value)
            }
            IteratorFromStage::HasInstance => {
                let iterator = continuation.first();
                if self.is_truthy_value(value)? {
                    return self.write(site.caller_base, site.destination, iterator);
                }
                let wrapper =
                    self.allocate_wrap_for_valid_iterator(iterator, continuation.second())?;
                self.write(site.caller_base, site.destination, wrapper)
            }
        }
    }

    /// Calls the cached next method of a branded valid-iterator wrapper.
    pub(crate) fn begin_wrap_for_valid_iterator_next(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.wrap_for_valid_iterator_snapshot(site.this_value)?;
        self.dispatch_property_callback(
            NativeContinuation::wrap_for_valid_iterator(
                Self::native_site(site),
                WrapForValidIteratorStage::NextCall,
                snapshot.iterator,
            ),
            snapshot.next_method,
        )
        .map(|_| ())
    }

    /// Looks up the underlying return method afresh for every wrapper return call.
    pub(crate) fn begin_wrap_for_valid_iterator_return(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.wrap_for_valid_iterator_snapshot(site.this_value)?;
        let return_atom = self.intern_intrinsic_name(b"return")?;
        self.dispatch_wrap_for_valid_iterator_get(
            Self::native_site(site),
            snapshot.iterator,
            return_atom.into(),
        )
    }

    /// Completes the wrapper next/return protocol after one callback boundary.
    pub(crate) fn resume_wrap_for_valid_iterator(
        &mut self,
        continuation: NativeContinuation,
        stage: WrapForValidIteratorStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        match stage {
            WrapForValidIteratorStage::NextCall | WrapForValidIteratorStage::ReturnCall => {
                self.write(site.caller_base, site.destination, value)
            }
            WrapForValidIteratorStage::ReturnGet => {
                if is_nullish(value) {
                    let result = self.create_iterator_result(
                        Value::from_immediate(Immediate::Undefined),
                        true,
                    )?;
                    return self.write(site.caller_base, site.destination, result);
                }
                self.resolve_function_object(value)?;
                self.dispatch_property_callback(
                    NativeContinuation::wrap_for_valid_iterator(
                        site,
                        WrapForValidIteratorStage::ReturnCall,
                        continuation.first(),
                    ),
                    value,
                )
                .map(|_| ())
            }
        }
    }

    /// Returns the constructor from the accessor function's defining Realm.
    pub(crate) fn iterator_constructor_getter(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let realm = self.realm_for_callable(site.callee)?;
        self.iterator_constructor_for_realm(realm)
            .ok_or(ExecutionError::RealmLimit { limit: u32::MAX })
    }

    /// Returns the proposal's fixed `%IteratorPrototype%` brand string.
    pub(crate) fn iterator_to_string_tag_getter(&mut self) -> Result<Value, ExecutionError> {
        self.allocate_runtime_string(
            JsString::try_from_latin1(b"Iterator").map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Begins SetterThatIgnoresPrototypeProperties for either special property.
    pub(crate) fn begin_iterator_prototype_setter(
        &mut self,
        site: &CallSite,
        key: IteratorPrototypeSetterKey,
    ) -> Result<(), ExecutionError> {
        let home_realm = self.realm_for_callable(site.callee)?;
        let receiver = site.this_value;
        if !self.is_object_value(receiver) {
            return Err(self.iterator_type_error(home_realm)?);
        }
        let home = self
            .iterator_prototype_for_realm(home_realm)
            .ok_or(ExecutionError::RealmLimit { limit: u32::MAX })?;
        if receiver == home {
            return Err(self.iterator_type_error(home_realm)?);
        }
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let property_key = self.iterator_setter_property_key(key)?;
        if self.is_proxy_value(receiver) {
            return self.dispatch_iterator_get_own(continuation_site, key, receiver, value);
        }
        if self
            .complete_own_property_descriptor(receiver, property_key)?
            .is_some()
        {
            self.dispatch_iterator_set(continuation_site, key, receiver, value, property_key)
        } else {
            self.define_iterator_data(receiver, value, property_key)?;
            self.write(
                continuation_site.caller_base,
                continuation_site.destination,
                Value::from_immediate(Immediate::Undefined),
            )
        }
    }

    /// Resumes a Proxy-aware get-own, define, or set step of the special setter.
    pub(crate) fn resume_iterator_prototype_setter(
        &mut self,
        continuation: NativeContinuation,
        key: IteratorPrototypeSetterKey,
        stage: IteratorPrototypeSetterStage,
        result: Value,
    ) -> Result<(), ExecutionError> {
        let receiver = continuation.first();
        let value = continuation.second();
        let property_key = self.iterator_setter_property_key(key)?;
        match stage {
            IteratorPrototypeSetterStage::GetOwn => {
                if self.is_truthy_value(result)? {
                    self.dispatch_iterator_set(
                        continuation.site(),
                        key,
                        receiver,
                        value,
                        property_key,
                    )
                } else {
                    self.dispatch_iterator_define(
                        continuation.site(),
                        key,
                        receiver,
                        value,
                        property_key,
                    )
                }
            }
            IteratorPrototypeSetterStage::Define | IteratorPrototypeSetterStage::Set => self.write(
                continuation.site().caller_base,
                continuation.site().destination,
                Value::from_immediate(Immediate::Undefined),
            ),
        }
    }

    /// Implements GetPrototypeFromConstructor with the newTarget Realm's Iterator fallback.
    pub(crate) fn construct_iterator_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let prototype_atom = self.prototype_atom()?;
        let explicit = self
            .constructor_prototype_value(site.new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value));
        let prototype = if let Some(prototype) = explicit {
            prototype
        } else {
            let realm = self.realm_for_callable(site.new_target)?;
            self.iterator_prototype_for_realm(realm)
                .ok_or(ExecutionError::RealmLimit { limit: u32::MAX })?
        };
        self.create_ordinary_object_with_prototype(prototype)
    }

    /// Creates a TypeError in the intrinsic accessor's defining Realm.
    pub(crate) fn iterator_type_error(
        &mut self,
        realm: RealmId,
    ) -> Result<ExecutionError, ExecutionError> {
        self.create_native_error_in_realm(NativeErrorKind::Type, None, realm)
            .map(ExecutionError::HostThrown)
    }

    /// Looks up one Realm's `%Iterator%` constructor without changing the active Realm.
    fn iterator_constructor_for_realm(&self, realm: RealmId) -> Option<Value> {
        self.realm_record(realm)?.iterator_constructor
    }

    /// Looks up one Realm's `%IteratorPrototype%` without extending isolate-wide kinds.
    fn iterator_prototype_for_realm(&self, realm: RealmId) -> Option<Value> {
        self.realm_record(realm)?.iterator_prototype
    }

    /// Continues GetIteratorFlattenable after selecting or calling @@iterator.
    fn continue_iterator_from_record(
        &mut self,
        site: NativeContinuationSite,
        iterator: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(iterator) {
            return Err(ExecutionError::NotObject(iterator));
        }
        let next = self.intern_intrinsic_name(b"next")?;
        self.dispatch_iterator_from_get(
            site,
            IteratorFromStage::NextGet,
            iterator,
            Value::from_immediate(Immediate::Undefined),
            iterator,
            next.into(),
        )
    }

    /// Runs OrdinaryHasInstance while preserving the cached iterator record as its parent.
    fn begin_iterator_from_has_instance(
        &mut self,
        site: NativeContinuationSite,
        iterator: Value,
        next_method: Value,
    ) -> Result<(), ExecutionError> {
        let constructor = self
            .realm
            .iterator_constructor
            .expect("Iterator constructor initializes before Iterator.from");
        let depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::iterator_from(
                site,
                IteratorFromStage::HasInstance,
                iterator,
                next_method,
            ))
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        let outcome = self.begin_ordinary_has_instance(site, constructor, iterator);
        self.finish_immediate_iterator_from_dispatch(depth, frame_depth, outcome)
    }

    /// Performs a Proxy/accessor-aware property Get below an Iterator.from parent.
    fn dispatch_iterator_from_get(
        &mut self,
        site: NativeContinuationSite,
        stage: IteratorFromStage,
        first: Value,
        second: Value,
        target: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::iterator_from(
                site, stage, first, second,
            ))
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        let outcome = self.dispatch_proxy_aware_property_read(site, target, target, key);
        self.finish_immediate_iterator_from_dispatch(depth, frame_depth, outcome)
    }

    /// Performs wrapper return's observable GetMethod lookup below its typed parent.
    fn dispatch_wrap_for_valid_iterator_get(
        &mut self,
        site: NativeContinuationSite,
        iterator: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::wrap_for_valid_iterator(
                site,
                WrapForValidIteratorStage::ReturnGet,
                iterator,
            ))
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        let outcome = self.dispatch_proxy_aware_property_read(site, iterator, iterator, key);
        self.finish_immediate_iterator_from_dispatch(depth, frame_depth, outcome)
    }

    /// Resumes a parent immediately when its nested operation did not create a JS frame.
    fn finish_immediate_iterator_from_dispatch(
        &mut self,
        completion_depth: usize,
        frame_depth: usize,
        outcome: Result<Option<RunOutcome>, ExecutionError>,
    ) -> Result<(), ExecutionError> {
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                if self.fiber.completions.len() > completion_depth {
                    self.pop_native_continuation()?;
                }
                return Err(error);
            }
        };
        if self.fiber.completions.len() == completion_depth
            || self.fiber.frames.len() != frame_depth
        {
            return Ok(());
        }
        debug_assert!(outcome.is_none());
        let continuation = self.pop_native_continuation()?;
        let value = self.read(
            continuation.site().caller_base,
            continuation.site().destination,
        )?;
        match continuation.kind() {
            NativeContinuationKind::IteratorFrom(stage) => {
                self.resume_iterator_from(continuation, stage, value)
            }
            NativeContinuationKind::WrapForValidIterator(stage) => {
                self.resume_wrap_for_valid_iterator(continuation, stage, value)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Allocates the compact wrapper only after all observable Iterator.from steps finish.
    fn allocate_wrap_for_valid_iterator(
        &mut self,
        iterator: Value,
        next_method: Value,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .wrap_for_valid_iterator_prototype
            .expect("valid-iterator wrapper prototype initializes before Iterator.from");
        let mut roots = WrapForValidIteratorAllocationRoots {
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
            iterator,
            next_method,
            prototype,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.wrap_for_valid_iterator,
                0,
                0,
                WrapForValidIteratorObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                    iterator: roots.iterator,
                    next_method: roots.next_method,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|wrapper| Value::from_heap_ref(wrapper.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Reads a wrapper payload by value so no heap borrow crosses a callback boundary.
    fn wrap_for_valid_iterator_snapshot(
        &mut self,
        value: Value,
    ) -> Result<WrapForValidIteratorObject, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.wrap_for_valid_iterator)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let wrapper = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(wrapper, self.types.wrap_for_valid_iterator)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    #[inline(always)]
    const fn native_site(site: &CallSite) -> NativeContinuationSite {
        NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        }
    }

    /// Returns a Realm record from either the active slot or inactive Realm table.
    fn realm_record(&self, realm: RealmId) -> Option<&Realm> {
        if realm == self.active_realm {
            Some(&self.realm)
        } else {
            self.inactive_realms
                .iter()
                .find_map(|(id, candidate)| (*id == realm).then_some(candidate))
        }
    }

    /// Resolves the compact setter key to the corresponding property identity.
    fn iterator_setter_property_key(
        &mut self,
        key: IteratorPrototypeSetterKey,
    ) -> Result<PropertyKey, ExecutionError> {
        match key {
            IteratorPrototypeSetterKey::Constructor => {
                Ok(PropertyKey::Atom(self.constructor_atom()?))
            }
            IteratorPrototypeSetterKey::ToStringTag => {
                let symbol = self
                    .agent
                    .well_known_symbols
                    .to_string_tag
                    .expect("Symbol.toStringTag initializes before Iterator");
                self.property_key(symbol)
            }
        }
    }

    /// Converts a property key into the Value form required by Proxy [[GetOwnProperty]].
    fn iterator_setter_key_value(
        &mut self,
        key: IteratorPrototypeSetterKey,
    ) -> Result<Value, ExecutionError> {
        match key {
            IteratorPrototypeSetterKey::Constructor => {
                let atom = self.constructor_atom()?;
                self.atom_string_value(atom)
            }
            IteratorPrototypeSetterKey::ToStringTag => Ok(self
                .agent
                .well_known_symbols
                .to_string_tag
                .expect("Symbol.toStringTag initializes before Iterator")),
        }
    }

    /// Runs Proxy [[GetOwnProperty]] while retaining both setter operands.
    fn dispatch_iterator_get_own(
        &mut self,
        site: NativeContinuationSite,
        key: IteratorPrototypeSetterKey,
        receiver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let key_value = self.iterator_setter_key_value(key)?;
        self.push_iterator_setter_parent(
            site,
            key,
            IteratorPrototypeSetterStage::GetOwn,
            receiver,
            value,
        )?;
        let frame_depth = self.fiber.frames.len();
        let outcome =
            self.dispatch_proxy_get_own(site, receiver, key_value, ProxyGetOwnMode::HasOwn);
        self.finish_immediate_iterator_dispatch(frame_depth, outcome)
    }

    /// Runs CreateDataPropertyOrThrow through Proxy [[DefineOwnProperty]].
    fn dispatch_iterator_define(
        &mut self,
        site: NativeContinuationSite,
        key: IteratorPrototypeSetterKey,
        receiver: Value,
        value: Value,
        property_key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let descriptor = PropertyDescriptor::Data(DataPropertyDescriptor {
            value: Some(value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(true),
        });
        self.push_iterator_setter_parent(
            site,
            key,
            IteratorPrototypeSetterStage::Define,
            receiver,
            value,
        )?;
        let frame_depth = self.fiber.frames.len();
        let outcome = self.dispatch_proxy_define(
            site,
            receiver,
            property_key,
            descriptor,
            ProxyDefineMode::Object,
        );
        self.finish_immediate_iterator_dispatch(frame_depth, outcome)
    }

    /// Runs Set with Throw=true on a receiver known to own the special property.
    fn dispatch_iterator_set(
        &mut self,
        site: NativeContinuationSite,
        key: IteratorPrototypeSetterKey,
        receiver: Value,
        value: Value,
        property_key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        self.push_iterator_setter_parent(
            site,
            key,
            IteratorPrototypeSetterStage::Set,
            receiver,
            value,
        )?;
        let frame_depth = self.fiber.frames.len();
        let outcome = self.dispatch_proxy_aware_property_write(
            site,
            receiver,
            receiver,
            property_key,
            value,
            ProxySetMode::ObjectAssign,
        );
        self.finish_immediate_iterator_dispatch(frame_depth, outcome)
    }

    /// Pops and resumes the parent when a nested dispatcher completed synchronously.
    fn finish_immediate_iterator_dispatch(
        &mut self,
        frame_depth: usize,
        outcome: Result<Option<RunOutcome>, ExecutionError>,
    ) -> Result<(), ExecutionError> {
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                self.pop_native_continuation()?;
                return Err(error);
            }
        };
        if self.fiber.frames.len() != frame_depth {
            return Ok(());
        }
        debug_assert!(outcome.is_none());
        let continuation = self.pop_native_continuation()?;
        let result = self.read(
            continuation.site().caller_base,
            continuation.site().destination,
        )?;
        let NativeContinuationKind::IteratorPrototypeSetter { key, stage } = continuation.kind()
        else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_iterator_prototype_setter(continuation, key, stage, result)
    }

    /// Publishes setter operands as precise roots before entering user code.
    fn push_iterator_setter_parent(
        &mut self,
        site: NativeContinuationSite,
        key: IteratorPrototypeSetterKey,
        stage: IteratorPrototypeSetterStage,
        receiver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::iterator_prototype_setter(
                site, key, stage, receiver, value,
            ))
            .map_err(Self::completion_stack_error)
    }

    /// Defines the mandated writable, enumerable, configurable data property.
    fn define_iterator_data(
        &mut self,
        receiver: Value,
        value: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        self.define_data_property(
            receiver,
            key,
            DataPropertyDescriptor {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
            },
        )
    }
}
