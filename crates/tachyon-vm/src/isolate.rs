//! Isolate construction and allocation-oriented runtime orchestration.

use core::mem;

use super::*;

struct CollectionAllocationRoots<'a> {
    vm: VmRoots<'a>,
    prototype: Value,
    storage: Option<GcRef<OrderedCollection>>,
}

impl Trace for CollectionAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.prototype.trace(tracer);
        self.storage.trace(tracer);
    }
}

struct WeakCollectionAllocationRoots<'a> {
    vm: VmRoots<'a>,
    prototype: Value,
    storage: Option<GcRef<WeakCollection>>,
}

impl Trace for WeakCollectionAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.prototype.trace(tracer);
        self.storage.trace(tracer);
    }
}

/// A single-thread-owned ECMAScript execution state; `Cell` intentionally makes it `!Sync`.
pub struct Isolate {
    pub(crate) fiber: Fiber,
    pub(crate) finalization_jobs: finalization::FinalizationJobs,
    pub(crate) promise_jobs: PromiseJobQueue,
    pub(crate) atoms: AtomTable,
    pub(crate) shapes: ShapeTable,
    pub(crate) realm: Realm,
    pub(crate) inactive_realms: Vec<(RealmId, Realm)>,
    pub(crate) active_realm: RealmId,
    pub(crate) next_realm_serial: NonZeroU32,
    pub(crate) eval_script_callback: Option<EvalScriptCallback>,
    pub(crate) dynamic_function_callback: Option<DynamicFunctionCallback>,
    pub(crate) suspended_fibers: Vec<Fiber>,
    pub(crate) loaded_code: Vec<LoadedCode>,
    pub(crate) heap: Heap,
    pub(crate) types: VmTypes,
    pub(crate) intrinsic_property_atoms: IntrinsicPropertyAtoms,
    pub(crate) next_symbol_serial: NonZeroU32,
    pub(crate) math_random_state: u64,
    pub(crate) stack_limits: StackLimits,
    #[cfg(feature = "opcode-profile")]
    pub(crate) execution_profile: ExecutionProfile,
    pub(crate) _not_sync: Cell<()>,
}

impl Isolate {
    /// Returns one Realm-local intrinsic without changing the active execution context.
    pub(crate) fn realm_intrinsic_prototype(
        &self,
        realm: RealmId,
        kind: IntrinsicPrototypeKind,
    ) -> Option<Value> {
        let lookup = |realm: &Realm| match kind {
            IntrinsicPrototypeKind::Object => realm.object_prototype,
            IntrinsicPrototypeKind::Array => realm.array_prototype,
            IntrinsicPrototypeKind::Boolean => realm.boolean_prototype,
            IntrinsicPrototypeKind::Date => realm.date_prototype,
            IntrinsicPrototypeKind::String => realm.string_prototype,
        };
        if realm == self.active_realm {
            return lookup(&self.realm);
        }
        self.inactive_realms
            .iter()
            .find(|(id, _)| *id == realm)
            .and_then(|(_, realm)| lookup(realm))
    }

    /// Returns one Realm's intrinsic Array constructor without switching execution contexts.
    pub(crate) fn realm_array_constructor(&self, realm: RealmId) -> Option<Value> {
        if realm == self.active_realm {
            return self.realm.array_constructor;
        }
        self.inactive_realms
            .iter()
            .find(|(id, _)| *id == realm)
            .and_then(|(_, realm)| realm.array_constructor)
    }

    /// Creates an independent Realm in the same GC heap and returns its global object identity.
    pub fn create_realm(&mut self) -> Result<(RealmId, Value), ExecutionError> {
        let id = RealmId::from_non_zero(self.next_realm_serial);
        self.next_realm_serial = NonZeroU32::new(
            self.next_realm_serial
                .get()
                .checked_add(1)
                .ok_or(ExecutionError::RealmLimit { limit: u32::MAX })?,
        )
        .expect("checked realm serial remains non-zero");
        let limits = self.realm.limits;
        let typeof_strings = self.realm.typeof_strings;
        let primitive_hint_strings = self.realm.primitive_hint_strings;
        let current_id = self.active_realm;
        let current = mem::replace(
            &mut self.realm,
            Realm::new(limits, typeof_strings, primitive_hint_strings),
        );
        self.inactive_realms.push((current_id, current));
        self.active_realm = id;
        let initialized = self.initialize_realm_intrinsics();
        if let Err(error) = initialized {
            let (_, current) = self
                .inactive_realms
                .pop()
                .expect("realm swap retains the previous active realm");
            self.realm = current;
            self.active_realm = current_id;
            return Err(error);
        }
        if self.eval_script_callback.is_some() {
            self.install_realm_hooks_current()?;
        }
        let child = mem::replace(
            &mut self.realm,
            self.inactive_realms
                .pop()
                .expect("realm swap retains the previous active realm")
                .1,
        );
        let global = child
            .global_object
            .expect("initialized realm publishes a global object");
        self.inactive_realms.push((id, child));
        self.active_realm = current_id;
        Ok((id, global))
    }

    /// Installs the host-only `$262` realm hooks on the currently active global object.
    pub fn install_realm_hooks(
        &mut self,
        eval_script: EvalScriptCallback,
        dynamic_function: DynamicFunctionCallback,
    ) -> Result<(), ExecutionError> {
        self.eval_script_callback = Some(eval_script);
        self.dynamic_function_callback = Some(dynamic_function);
        self.install_realm_hooks_current()
    }

    /// Installs host hooks without changing the callback already owned by this isolate.
    fn install_realm_hooks_current(&mut self) -> Result<(), ExecutionError> {
        let global = self
            .realm
            .global_object
            .expect("initialized realm publishes a global object");
        let hooks = self.create_ordinary_object()?;
        let harness_atom = self.intern_intrinsic_name(b"$262")?;
        self.set_own_data_property(global, harness_atom, hooks)?;
        self.realm.set(harness_atom, hooks)?;
        let prototype = self
            .realm
            .function_prototype
            .expect("initialized realm publishes a function prototype");
        let create = self.allocate_native_function(
            NativeFunction::HostCreateRealm,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype,
            },
        )?;
        let eval = self.allocate_native_function(
            NativeFunction::HostEvalScript,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype,
            },
        )?;
        let create_atom = self.intern_intrinsic_name(b"createRealm")?;
        let eval_atom = self.intern_intrinsic_name(b"evalScript")?;
        self.set_own_data_property(hooks, create_atom, create)?;
        self.set_own_data_property(hooks, eval_atom, eval)?;
        let eval_global_atom = self.intern_intrinsic_name(b"eval")?;
        self.set_own_data_property(global, eval_global_atom, eval)?;
        self.realm.set(eval_global_atom, eval)
    }

    /// Executes one compiled module with a selected Realm as the active execution context.
    pub fn execute_in_realm(
        &mut self,
        realm: RealmId,
        module: &CompiledModule,
        budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        if realm == self.active_realm && self.fiber.frames.is_empty() {
            return self.execute(module, budget);
        }
        if realm == self.active_realm {
            let suspended_fiber = mem::take(&mut self.fiber);
            self.suspended_fibers.push(suspended_fiber);
            let outcome = self.execute(module, budget);
            let suspended_fiber = self
                .suspended_fibers
                .pop()
                .expect("nested same-realm execution retains its suspended fiber");
            let child_fiber = mem::replace(&mut self.fiber, suspended_fiber);
            debug_assert!(child_fiber.frames.is_empty());
            return outcome;
        }
        let position = self
            .inactive_realms
            .iter()
            .position(|(id, _)| *id == realm)
            .ok_or(ExecutionError::RealmLimit { limit: u32::MAX })?;
        let (_, selected) = self.inactive_realms.swap_remove(position);
        let current_id = self.active_realm;
        let current = mem::replace(&mut self.realm, selected);
        self.inactive_realms.push((current_id, current));
        self.active_realm = realm;
        let suspended_fiber = mem::take(&mut self.fiber);
        self.suspended_fibers.push(suspended_fiber);
        let outcome = self.execute(module, budget);
        let suspended_fiber = self
            .suspended_fibers
            .pop()
            .expect("nested realm execution retains its suspended fiber");
        let child_fiber = mem::replace(&mut self.fiber, suspended_fiber);
        debug_assert!(child_fiber.frames.is_empty());
        let position = self
            .inactive_realms
            .iter()
            .position(|(id, _)| *id == current_id)
            .expect("active realm swap retains the previous realm");
        let (_, current) = self.inactive_realms.swap_remove(position);
        let selected = mem::replace(&mut self.realm, current);
        self.inactive_realms.push((realm, selected));
        self.active_realm = current_id;
        outcome
    }

    /// Executes syntactic direct eval with the active frame's lexical environment as its parent.
    pub fn execute_direct_eval_in_realm(
        &mut self,
        realm: RealmId,
        module: &CompiledModule,
        budget: ExecutionBudget,
        strict_eval: bool,
    ) -> Result<RunOutcome, ExecutionError> {
        if realm != self.active_realm || self.fiber.frames.is_empty() {
            return Err(ExecutionError::UnsupportedDynamicFunctionConstructor);
        }
        let parent = self.fiber.frames.last().and_then(|frame| frame.environment);
        let code = self.load_module(module)?;
        let entry = self
            .loaded_code(code)?
            .module
            .function(module.entry_function())
            .ok_or(ExecutionError::MissingEntryFunction(
                module.entry_function(),
            ))?;
        let strict_eval = strict_eval || entry.strictness() == FunctionStrictness::Strict;
        let eval_var_environment = self.prepare_direct_eval_var_environment(code, strict_eval)?;
        let suspended_fiber = mem::take(&mut self.fiber);
        self.suspended_fibers.push(suspended_fiber);
        let outcome = self.execute_loaded_with_parent(code, budget, parent, eval_var_environment);
        let suspended_fiber = self
            .suspended_fibers
            .pop()
            .expect("direct eval retains its suspended caller fiber");
        let child_fiber = mem::replace(&mut self.fiber, suspended_fiber);
        debug_assert!(child_fiber.frames.is_empty());
        outcome
    }

    /// Copies one class constructor header without retaining a borrow across VM work.
    pub(crate) fn class_constructor_snapshot(
        &mut self,
        data: GcRef<ClassConstructorData>,
    ) -> Result<ClassConstructorData, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(data, self.types.class_constructor_data)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Copies one fixed native argument state through a checked no-GC borrow.
    pub(crate) fn native_call_state_snapshot(
        &mut self,
        state: GcRef<NativeCallState>,
    ) -> Result<NativeCallState, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.native_call_state)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Registers VM payload descriptors before constructing an otherwise empty isolate heap.
    pub fn new(config: IsolateConfig) -> Result<Self, IsolateCreationError> {
        let mut registry = TypeRegistry::new();
        let types = VmTypes {
            accessor_pair: registry
                .try_register("AccessorPair")
                .map_err(IsolateCreationError::TypeRegistration)?,
            array_buffer_data: registry
                .try_register("ArrayBufferData")
                .map_err(IsolateCreationError::TypeRegistration)?,
            array_buffer_object: registry
                .try_register("ArrayBufferObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            arguments_object: registry
                .try_register("ArgumentsObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            array: registry
                .try_register("ArrayObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            array_iterator: registry
                .try_register("ArrayIteratorObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            collection_iterator: registry
                .try_register("CollectionIteratorObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            bound_function: registry
                .try_register("BoundFunctionData")
                .map_err(IsolateCreationError::TypeRegistration)?,
            class_constructor_data: registry
                .try_register("ClassConstructorData")
                .map_err(IsolateCreationError::TypeRegistration)?,
            class_instance_element_plan: registry
                .try_register("ClassInstanceElementPlan")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_instance_elements: registry
                .try_register("PendingInstanceElements")
                .map_err(IsolateCreationError::TypeRegistration)?,
            environment: registry
                .try_register("Environment")
                .map_err(IsolateCreationError::TypeRegistration)?,
            exclusion_list: registry
                .try_register("ExclusionList")
                .map_err(IsolateCreationError::TypeRegistration)?,
            for_in_iterator: registry
                .try_register("ForInIterator")
                .map_err(IsolateCreationError::TypeRegistration)?,
            map_object: registry
                .try_register("MapObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            ordered_collection: registry
                .try_register("OrderedCollection")
                .map_err(IsolateCreationError::TypeRegistration)?,
            weak_collection: registry
                .try_register("WeakCollection")
                .map_err(IsolateCreationError::TypeRegistration)?,
            weak_map_object: registry
                .try_register("WeakMapObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            weak_set_object: registry
                .try_register("WeakSetObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            function: registry
                .try_register("FunctionObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            error_object: registry
                .try_register("ErrorObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            date_object: registry
                .try_register("DateObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            proxy_object: registry
                .try_register("ProxyObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            number_object: registry
                .try_register("NumberObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            boolean_object: registry
                .try_register("BooleanObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            string_object: registry
                .try_register("StringObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            symbol_object: registry
                .try_register("SymbolObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            ordinary_object: registry
                .try_register("OrdinaryObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_property_descriptor: registry
                .try_register("PendingPropertyDescriptor")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_define_properties: registry
                .try_register("PendingDefineProperties")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_get_own_property_descriptors: registry
                .try_register("PendingGetOwnPropertyDescriptors")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_proxy_define: registry
                .try_register("PendingProxyDefine")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_proxy_own_keys: registry
                .try_register("PendingProxyOwnKeys")
                .map_err(IsolateCreationError::TypeRegistration)?,
            promise_object: registry
                .try_register("PromiseObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            promise_capability: registry
                .try_register("PromiseCapability")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_promise_combinator: registry
                .try_register("PendingPromiseCombinator")
                .map_err(IsolateCreationError::TypeRegistration)?,
            promise_combinator_element: registry
                .try_register("PromiseCombinatorElement")
                .map_err(IsolateCreationError::TypeRegistration)?,
            promise_resolution_cell: registry
                .try_register("PromiseResolutionCell")
                .map_err(IsolateCreationError::TypeRegistration)?,
            promise_reaction: registry
                .try_register("PromiseReaction")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_argument_list: registry
                .try_register("PendingArgumentList")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_native_property_key: registry
                .try_register("PendingNativePropertyKey")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_date_numeric_arguments: registry
                .try_register("PendingDateNumericArguments")
                .map_err(IsolateCreationError::TypeRegistration)?,
            native_call_state: registry
                .try_register("NativeCallState")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_concat: registry
                .try_register("PendingArrayConcat")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_copy: registry
                .try_register("PendingArrayCopy")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_copy_within: registry
                .try_register("PendingArrayCopyWithin")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_fill: registry
                .try_register("PendingArrayFill")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_join: registry
                .try_register("PendingArrayJoin")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_flat: registry
                .try_register("PendingArrayFlat")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_flat_map: registry
                .try_register("PendingArrayFlatMap")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_slice: registry
                .try_register("PendingArraySlice")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_splice: registry
                .try_register("PendingArraySplice")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_remove: registry
                .try_register("PendingArrayRemove")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_reverse: registry
                .try_register("PendingArrayReverse")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_insert: registry
                .try_register("PendingArrayInsert")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_static: registry
                .try_register("PendingArrayStatic")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_array_to_sorted: registry
                .try_register("PendingArrayToSorted")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_copy_data_properties: registry
                .try_register("PendingCopyDataProperties")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_object_assign: registry
                .try_register("PendingObjectAssign")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_collection_initializer: registry
                .try_register("PendingCollectionInitializer")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_collection_for_each: registry
                .try_register("PendingCollectionForEach")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_map_get_or_insert_computed: registry
                .try_register("PendingMapGetOrInsertComputed")
                .map_err(IsolateCreationError::TypeRegistration)?,
            regexp_object: registry
                .try_register("RegExpObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            set_object: registry
                .try_register("SetObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            property_storage: registry
                .try_register("PropertyStorage")
                .map_err(IsolateCreationError::TypeRegistration)?,
            string: registry
                .try_register("JsString")
                .map_err(IsolateCreationError::TypeRegistration)?,
            symbol: registry
                .try_register("SymbolValue")
                .map_err(IsolateCreationError::TypeRegistration)?,
        };
        let shapes =
            ShapeTable::new(config.realm_limits.max_shapes).map_err(IsolateCreationError::Shape)?;
        let mut heap = Heap::new(config.heap_limit, registry);
        let typeof_strings = TypeofStrings::allocate(&mut heap, types.string)?;
        let primitive_hint_strings = PrimitiveHintStrings::allocate(&mut heap, types.string)?;
        let mut isolate = Self {
            fiber: Fiber::default(),
            finalization_jobs: finalization::FinalizationJobs::new(),
            promise_jobs: PromiseJobQueue::new(),
            atoms: AtomTable::new(config.atom_table),
            shapes,
            realm: Realm::new(config.realm_limits, typeof_strings, primitive_hint_strings),
            inactive_realms: Vec::new(),
            active_realm: RealmId::MAIN,
            next_realm_serial: NonZeroU32::new(2).expect("two is non-zero"),
            eval_script_callback: None,
            dynamic_function_callback: None,
            suspended_fibers: Vec::new(),
            loaded_code: Vec::new(),
            heap,
            types,
            intrinsic_property_atoms: IntrinsicPropertyAtoms::default(),
            next_symbol_serial: NonZeroU32::MIN,
            math_random_state: 0x6a09_e667_f3bc_c909,
            stack_limits: config.stack_limits,
            #[cfg(feature = "opcode-profile")]
            execution_profile: ExecutionProfile::default(),
            _not_sync: Cell::new(()),
        };
        isolate
            .initialize_realm_intrinsics()
            .map_err(IsolateCreationError::IntrinsicInitialization)?;
        Ok(isolate)
    }

    #[must_use]
    pub const fn atoms(&self) -> &AtomTable {
        &self.atoms
    }

    pub const fn atoms_mut(&mut self) -> &mut AtomTable {
        &mut self.atoms
    }

    /// Returns the opt-in interpreter profile accumulated by this isolate.
    #[cfg(feature = "opcode-profile")]
    #[must_use]
    pub const fn execution_profile(&self) -> &ExecutionProfile {
        &self.execution_profile
    }

    /// Clears every opt-in interpreter counter without changing executable state.
    #[cfg(feature = "opcode-profile")]
    pub fn reset_execution_profile(&mut self) {
        self.execution_profile = ExecutionProfile::default();
    }

    /// Classifies an unforgeably branded managed Error without exposing heap identities.
    pub fn native_error_kind(
        &mut self,
        value: Value,
    ) -> Result<Option<NativeErrorKind>, ExecutionError> {
        let Some(raw) = value.as_heap_ref() else {
            return Ok(None);
        };
        let Ok(error) = self.heap.checked_reference(raw, self.types.error_object) else {
            return Ok(None);
        };
        self.heap.with_running_scope(|scope| {
            let error = scope.root(error).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(error, self.types.error_object)
                    .map(|error| Some(error.kind))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(crate) fn allocate_intrinsic_ordinary_object(
        &mut self,
        ordinary: OrdinaryObject,
    ) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.ordinary_object,
                0,
                0,
                ordinary,
                AllocationSpace::Old,
                roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn intern_intrinsic_name(&mut self, name: &[u8]) -> Result<AtomId, ExecutionError> {
        let string = JsString::try_from_latin1(name).map_err(ExecutionError::PropertyKeyString)?;
        self.atoms
            .try_intern(string)
            .map_err(ExecutionError::PropertyKeyAtom)
    }

    /// Allocates one native callable through the same managed function descriptor as bytecode code.
    pub(crate) fn allocate_native_function(
        &mut self,
        native: NativeFunction,
        ordinary: OrdinaryObject,
    ) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::Native(native),
                    prototype_or_home_object: None,
                    ordinary,
                },
                AllocationSpace::Old,
                roots,
            )
            .map(|function| Value::from_heap_ref(function.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates a traced reaction wrapper that preserves a finally argument across callback calls.
    pub(crate) fn allocate_promise_finally_handler(
        &mut self,
        callback: Value,
        constructor: Value,
        rejected: bool,
    ) -> Result<Value, ExecutionError> {
        let function_prototype = self
            .realm
            .function_prototype
            .expect("Function prototype initializes before Promise.finally");
        let state = self.allocate_promise_then_state(NativeCallState {
            values: [
                callback,
                constructor,
                Value::from_immediate(Immediate::Undefined),
                Value::from_immediate(Immediate::Undefined),
                Value::from_immediate(Immediate::Undefined),
            ],
            count: 2,
        })?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::PromiseFinallyHandler { state, rejected },
                    prototype_or_home_object: None,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: function_prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|function| Value::from_heap_ref(function.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates the thunk that restores or throws the original finally settlement argument.
    pub(crate) fn allocate_promise_finally_result_handler(
        &mut self,
        value: Value,
        rejected: bool,
    ) -> Result<Value, ExecutionError> {
        let function_prototype = self
            .realm
            .function_prototype
            .expect("Function prototype initializes before Promise.finally");
        let state = self.allocate_promise_then_state(NativeCallState {
            values: [
                value,
                Value::from_immediate(Immediate::Undefined),
                Value::from_immediate(Immediate::Undefined),
                Value::from_immediate(Immediate::Undefined),
                Value::from_immediate(Immediate::Undefined),
            ],
            count: 1,
        })?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::PromiseFinallyResultHandler { state, rejected },
                    prototype_or_home_object: None,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: function_prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|function| Value::from_heap_ref(function.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Creates one bound exotic while flattening nested wrappers into one immutable argument prefix.
    pub(crate) fn create_bound_function(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let bound_target = site.this_value;
        self.resolve_function_object(bound_target)?;
        let length =
            self.bound_function_length(bound_target, site.argument_count.saturating_sub(1))?;
        let name = self.allocate_bound_function_name(bound_target)?;
        self.write(site.caller_base, site.destination, name)?;
        let supplied_this = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let target_object = self.resolve_function_object(bound_target)?;
        let (call_target, bound_this, existing_arguments) = match target_object.executable {
            FunctionExecutable::Bound(data) => {
                let snapshot = self.bound_function_snapshot(data)?;
                (snapshot.call_target, snapshot.bound_this, Some(data))
            }
            _ => (bound_target, supplied_this, None),
        };
        let existing_count = existing_arguments
            .map(|data| {
                self.bound_function_snapshot(data)
                    .map(|data| data.argument_count)
            })
            .transpose()?
            .unwrap_or(0);
        let supplied_count = site.argument_count.saturating_sub(1);
        let argument_count = existing_count
            .checked_add(supplied_count)
            .ok_or(ExecutionError::BoundArgumentCountOverflow)?;
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(argument_count as usize)
            .map_err(|_| ExecutionError::BoundArgumentAllocationFailed)?;
        if let Some(data) = existing_arguments {
            self.append_bound_arguments(data, &mut arguments)?;
        }
        for index in 0..supplied_count {
            arguments.push(
                self.call_argument(site, index + 1)?
                    .expect("supplied bound argument is within the call window"),
            );
        }
        let data = {
            let roots = &mut VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            };
            self.heap
                .try_allocate_external_with_gc(
                    self.types.bound_function,
                    0,
                    BoundFunctionData {
                        bound_target,
                        call_target,
                        bound_this,
                        arguments: arguments.into_boxed_slice(),
                        length,
                        name,
                    },
                    AllocationSpace::Young,
                    roots,
                )
                .map_err(ExecutionError::HeapAllocation)?
        };
        let internal_prototype = self
            .resolve_function_object(site.this_value)?
            .ordinary
            .prototype;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let function = self
            .heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::Bound(data),
                    prototype_or_home_object: None,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: internal_prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        Ok(Value::from_heap_ref(function.raw()))
    }

    /// Stores an apply argument list in the existing immutable bound-prefix representation.
    pub(crate) fn create_apply_argument_prefix(
        &mut self,
        target: Value,
        this_value: Value,
        arguments: Vec<Value>,
    ) -> Result<GcRef<BoundFunctionData>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.bound_function,
                0,
                BoundFunctionData {
                    bound_target: target,
                    call_target: target,
                    bound_this: this_value,
                    arguments: arguments.into_boxed_slice(),
                    length: Value::from_i32(0),
                    name: Value::from_immediate(Immediate::Undefined),
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Computes the configurable bound length from the target's own numeric length property.
    pub(crate) fn bound_function_length(
        &mut self,
        target: Value,
        supplied_arguments: u32,
    ) -> Result<Value, ExecutionError> {
        let length_atom = self.length_atom()?;
        let Some((length, _)) = self.own_data_property_with_attributes(target, length_atom)? else {
            return Ok(Value::from_i32(0));
        };
        let Some(length) = numeric_value(length) else {
            return Ok(Value::from_i32(0));
        };
        if length == f64::INFINITY {
            return Ok(Value::from_f64(f64::INFINITY));
        }
        let length = length.trunc().max(0.0) - f64::from(supplied_arguments);
        Ok(Value::from_f64(length.max(0.0)))
    }

    /// Materializes `"bound " + targetName` with one exact UTF-16 reserve before GC allocation.
    pub(crate) fn allocate_bound_function_name(
        &mut self,
        target: Value,
    ) -> Result<Value, ExecutionError> {
        const PREFIX: &[u8] = b"bound ";
        let name_atom = self.name_atom()?;
        let target_name = self
            .get_data_property(target, name_atom)?
            .filter(|value| self.is_string_value(*value));
        let target_length = target_name
            .map(|value| self.string_value_length(value))
            .transpose()?
            .unwrap_or(0);
        let capacity = PREFIX
            .len()
            .checked_add(target_length)
            .ok_or(ExecutionError::BoundNameAllocationFailed)?;
        let mut units = Vec::new();
        units
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::BoundNameAllocationFailed)?;
        units.extend(PREFIX.iter().map(|&byte| u16::from(byte)));
        if let Some(target_name) = target_name {
            self.append_primitive_string_units(target_name, &mut units)?;
        }
        let name = JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?;
        self.allocate_runtime_string(name)
    }

    #[inline(always)]
    pub(crate) fn is_string_value(&self, value: Value) -> bool {
        value
            .as_heap_ref()
            .is_some_and(|raw| self.heap.checked_reference(raw, self.types.string).is_ok())
    }

    #[inline(always)]
    pub(crate) fn is_string_wrapper(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.string_object)
                .is_ok()
        })
    }

    #[inline(always)]
    pub(crate) fn is_symbol_value(&self, value: Value) -> bool {
        value
            .as_heap_ref()
            .is_some_and(|raw| self.heap.checked_reference(raw, self.types.symbol).is_ok())
    }

    pub(crate) fn string_value_length(&mut self, value: Value) -> Result<usize, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedStringValue(value))?;
        if let Ok(wrapper) = self.heap.checked_reference(raw, self.types.string_object) {
            let string_data = self.heap.with_running_scope(|scope| {
                let wrapper = scope.root(wrapper).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(wrapper, self.types.string_object)
                        .map(|wrapper| wrapper.string_data)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return self.string_value_length(string_data);
        }
        let string = self
            .heap
            .checked_reference(raw, self.types.string)
            .map_err(|_| ExecutionError::UnsupportedStringValue(value))?;
        self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(string, self.types.string)
                    .map(JsString::len)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Copies a primitive or boxed ECMAScript string for an embedding callback boundary.
    pub fn string_value_to_utf16(&mut self, value: Value) -> Result<Vec<u16>, ExecutionError> {
        let string_data = if let Some(raw) = value.as_heap_ref()
            && let Ok(wrapper) = self.heap.checked_reference(raw, self.types.string_object)
        {
            self.heap.with_running_scope(|scope| {
                let wrapper = scope.root(wrapper).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(wrapper, self.types.string_object)
                        .map(|wrapper| wrapper.string_data)
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?
        } else {
            value
        };
        let raw = string_data
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedStringValue(value))?;
        let string = self
            .heap
            .checked_reference(raw, self.types.string)
            .map_err(|_| ExecutionError::UnsupportedStringValue(value))?;
        self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let string = no_gc
                    .borrow(string, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(match string.as_view() {
                    JsStringView::Latin1(bytes) => {
                        bytes.iter().map(|byte| u16::from(*byte)).collect()
                    }
                    JsStringView::Utf16(units) => units.to_vec(),
                })
            })
        })
    }

    pub(crate) fn bound_function_snapshot(
        &mut self,
        data: GcRef<BoundFunctionData>,
    ) -> Result<BoundFunctionSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow(data, self.types.bound_function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(BoundFunctionSnapshot {
                    bound_target: data.bound_target,
                    call_target: data.call_target,
                    bound_this: data.bound_this,
                    argument_count: u32::try_from(data.arguments.len())
                        .map_err(|_| ExecutionError::BoundArgumentCountOverflow)?,
                    length: data.length,
                    name: data.name,
                })
            })
        })
    }

    pub(crate) fn bound_function_argument(
        &mut self,
        data: GcRef<BoundFunctionData>,
        index: u32,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(data, self.types.bound_function)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .arguments
                    .get(index as usize)
                    .copied()
                    .ok_or(ExecutionError::BoundArgumentCountOverflow)
            })
        })
    }

    pub(crate) fn append_bound_arguments(
        &mut self,
        data: GcRef<BoundFunctionData>,
        output: &mut Vec<Value>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let data = scope.root(data).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let data = no_gc
                    .borrow(data, self.types.bound_function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                output.extend_from_slice(&data.arguments);
                Ok(())
            })
        })
    }

    /// Allocates an empty ordinary object with a caller-selected prototype through managed GC.
    pub(crate) fn create_ordinary_object(&mut self) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before ordinary objects");
        self.create_ordinary_object_with_prototype(prototype)
    }

    /// Keeps a prototype edge in the pending payload so pre-allocation collection can rewrite it.
    pub(crate) fn create_ordinary_object_with_prototype(
        &mut self,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let object = self
            .heap
            .try_allocate_with_gc(
                self.types.ordinary_object,
                0,
                0,
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype,
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        Ok(Value::from_heap_ref(object.raw()))
    }

    /// Allocates one arguments exotic object and records a lazy simple-parameter alias when valid.
    pub(crate) fn allocate_arguments_object(
        &mut self,
        mapped: Option<(u32, u32, u32, CodeId, FunctionId)>,
        strict: bool,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before arguments objects");
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.arguments_object,
                0,
                0,
                ArgumentsObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                    mapped_frame_depth: mapped.map_or(u32::MAX, |mapping| mapping.0),
                    mapped_base: mapped.map_or(0, |mapping| mapping.1),
                    mapped_parameter_count: mapped.map_or(0, |mapping| mapping.2),
                    mapped_code: mapped.map(|mapping| mapping.3),
                    mapped_function: mapped.map(|mapping| mapping.4),
                    strict_restricted_properties: strict,
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates one boxed Number while keeping its data and prototype live across collection.
    pub(crate) fn allocate_number_object(
        &mut self,
        number_data: Value,
        prototype: Value,
        space: AllocationSpace,
    ) -> Result<Value, ExecutionError> {
        debug_assert!(numeric_value(number_data).is_some());
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.number_object,
                0,
                0,
                NumberObject {
                    number_data,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                space,
                roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates one branded Date object while retaining its Realm-local prototype.
    pub(crate) fn allocate_date_object(
        &mut self,
        date_value: f64,
        prototype: Value,
        space: AllocationSpace,
    ) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.date_object,
                0,
                0,
                DateObject {
                    date_value,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                space,
                roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates one boxed Boolean while keeping its ordinary prototype rooted.
    pub(crate) fn allocate_boolean_object(
        &mut self,
        boolean_data: Value,
        prototype: Value,
        space: AllocationSpace,
    ) -> Result<Value, ExecutionError> {
        debug_assert!(matches!(
            boolean_data.as_immediate(),
            Some(Immediate::True | Immediate::False)
        ));
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.boolean_object,
                0,
                0,
                BooleanObject {
                    boolean_data,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                space,
                roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates one boxed String and roots its primitive data and ordinary prototype together.
    pub(crate) fn allocate_string_object(
        &mut self,
        string_data: Value,
        prototype: Value,
        space: AllocationSpace,
    ) -> Result<Value, ExecutionError> {
        debug_assert!(self.is_string_value(string_data));
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.string_object,
                0,
                0,
                StringObject {
                    string_data,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                space,
                roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates a boxed Symbol while tracing its optional primitive data and object prototype.
    pub(crate) fn allocate_symbol_object(
        &mut self,
        symbol_data: Option<Value>,
        prototype: Value,
        space: AllocationSpace,
    ) -> Result<Value, ExecutionError> {
        debug_assert!(symbol_data.is_none_or(|value| self.is_symbol_value(value)));
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.symbol_object,
                0,
                0,
                SymbolObject {
                    symbol_data,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                space,
                roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates a RegExp object after pattern validation has produced source and flag strings.
    pub(crate) fn allocate_regexp_object(
        &mut self,
        source: Value,
        flags: Value,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.regexp_object,
                0,
                0,
                RegExpObject {
                    source,
                    flags,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates a Map exotic and its externally accounted insertion-ordered backing together.
    pub(crate) fn allocate_map_object(
        &mut self,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let mut roots = CollectionAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            prototype,
            storage: None,
        };
        let storage = self
            .heap
            .try_allocate_external_with_gc(
                self.types.ordered_collection,
                0,
                OrderedCollection::with_capacity(tuning::collections::INITIAL_ENTRY_CAPACITY)
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)?,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.storage = Some(storage);
        self.heap
            .try_allocate_with_gc(
                self.types.map_object,
                0,
                0,
                MapObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                    storage,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates a Set exotic and its externally accounted insertion-ordered backing together.
    pub(crate) fn allocate_set_object(
        &mut self,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let mut roots = CollectionAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            prototype,
            storage: None,
        };
        let storage = self
            .heap
            .try_allocate_external_with_gc(
                self.types.ordered_collection,
                0,
                OrderedCollection::with_capacity(tuning::collections::INITIAL_ENTRY_CAPACITY)
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)?,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.storage = Some(storage);
        self.heap
            .try_allocate_with_gc(
                self.types.set_object,
                0,
                0,
                SetObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                    storage,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates an ephemeron-backed WeakMap and its exact external backing together.
    pub(crate) fn allocate_weak_map_object(
        &mut self,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let mut roots = WeakCollectionAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            prototype,
            storage: None,
        };
        let storage = self
            .heap
            .try_allocate_external_with_gc(
                self.types.weak_collection,
                0,
                WeakCollection::with_capacity(tuning::collections::INITIAL_ENTRY_CAPACITY)
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)?,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.storage = Some(storage);
        self.heap
            .try_allocate_with_gc(
                self.types.weak_map_object,
                0,
                0,
                WeakMapObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                    storage,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates an ephemeron-backed WeakSet and its exact external backing together.
    pub(crate) fn allocate_weak_set_object(
        &mut self,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let mut roots = WeakCollectionAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            prototype,
            storage: None,
        };
        let storage = self
            .heap
            .try_allocate_external_with_gc(
                self.types.weak_collection,
                0,
                WeakCollection::with_capacity(tuning::collections::INITIAL_ENTRY_CAPACITY)
                    .map_err(|_| ExecutionError::CollectionStorageAllocationFailed)?,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.storage = Some(storage);
        self.heap
            .try_allocate_with_gc(
                self.types.weak_set_object,
                0,
                0,
                WeakSetObject {
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: roots.prototype,
                    },
                    storage,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|object| Value::from_heap_ref(object.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates one Array exotic while keeping its ordinary prototype edge in the pending payload.
    pub(crate) fn create_array_object_with_prototype(
        &mut self,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        self.allocate_array_object(prototype, AllocationSpace::Young)
    }

    /// Publishes the mandatory length slot before exposing one Array exotic identity.
    pub(crate) fn allocate_array_object(
        &mut self,
        prototype: Value,
        space: AllocationSpace,
    ) -> Result<Value, ExecutionError> {
        let length = self.length_atom()?;
        let shape = self
            .shapes
            .transition_add(
                ShapeId::EMPTY,
                length,
                PropertyAttributes::data(true, false, false),
            )
            .map_err(ExecutionError::Shape)?;
        let mut roots = ArrayAllocationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            prototype,
        };
        let storage = self
            .heap
            .try_allocate_external_with_gc(
                self.types.property_storage,
                0,
                PropertyStorage::new(Box::new([Value::from_i32(0)])),
                space,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let array = self
            .heap
            .try_allocate_with_gc(
                self.types.array,
                0,
                0,
                ArrayObject {
                    ordinary: OrdinaryObject {
                        shape,
                        extensible: true,
                        storage: Some(storage),
                        prototype: roots.prototype,
                    },
                },
                space,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        Ok(Value::from_heap_ref(array.raw()))
    }

    /// Snapshots the currently visible enumerable string keys into one managed iterator payload.
    pub(crate) fn create_for_in_iterator(
        &mut self,
        source: Value,
    ) -> Result<Value, ExecutionError> {
        let keys = self.for_in_keys(source)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.for_in_iterator,
                0,
                ForInIterator::new(keys),
                AllocationSpace::Young,
                roots,
            )
            .map(|iterator| Value::from_heap_ref(iterator.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Applies ordinary `for-in` shadowing: every present own key suppresses prototypes.
    pub(crate) fn for_in_keys(
        &mut self,
        mut source: Value,
    ) -> Result<Box<[AtomId]>, ExecutionError> {
        if matches!(
            source.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Ok(Box::default());
        }
        if let Some(raw) = source.as_heap_ref()
            && let Ok(string) = self.heap.checked_reference(raw, self.types.string)
        {
            return self.for_in_string_keys(string);
        }
        if !self.is_object_value(source) {
            return Ok(Box::default());
        }
        // `for-in` observes Proxy `[[OwnPropertyKeys]]`; when the handler does not
        // provide that trap the operation forwards through every Proxy layer.
        // Keep this fast path synchronous so the common transparent-Proxy case
        // remains compatible with the existing iterator snapshot API.
        while self.is_proxy_value(source) {
            let snapshot = self.proxy_snapshot(source)?;
            if snapshot.handler.as_immediate() == Some(Immediate::Null) {
                return Err(ExecutionError::ProxyRevoked);
            }
            let trap_name = self.intern_intrinsic_name(b"ownKeys")?;
            let has_trap = match self.resolve_property_read(snapshot.handler, trap_name.into())? {
                PropertyRead::Missing => false,
                PropertyRead::Data(value)
                    if matches!(
                        value.as_immediate(),
                        Some(Immediate::Undefined | Immediate::Null)
                    ) =>
                {
                    false
                }
                PropertyRead::Accessor(getter)
                    if getter.as_immediate() == Some(Immediate::Undefined) =>
                {
                    false
                }
                PropertyRead::Data(_) | PropertyRead::Accessor(_) => true,
            };
            if has_trap {
                return Err(ExecutionError::NotObject(source));
            }
            source = snapshot.target;
        }
        let upper_bound = self.for_in_object_key_upper_bound(source)?;
        let mut keys = ForInKeySet::with_upper_bound(upper_bound)
            .map_err(|_: ForInAllocationError| ExecutionError::ForInKeyAllocationFailed)?;
        if self.is_string_wrapper(source) {
            let length = self.string_value_length(source)?;
            for index in 0..length {
                let atom = self.property_key_atom(Value::from_i32(
                    i32::try_from(index).map_err(|_| ExecutionError::ForInKeyAllocationFailed)?,
                ))?;
                keys.insert(atom);
                keys.push_enumerable(atom);
            }
        }
        let mut current = source;
        loop {
            let (_, snapshot) = self.object_snapshot(current)?;
            let mut own_keys = self.ordinary_own_property_keys(current, snapshot)?;
            while let Some(entry) = own_keys.next_entry() {
                let Some(key) = entry.key.atom() else {
                    continue;
                };
                let Some(property) = entry.property else {
                    keys.insert(key);
                    continue;
                };
                if keys.insert(key) && property.attributes.enumerable() {
                    keys.push_enumerable(key);
                }
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                break;
            }
            if !self.is_object_value(snapshot.prototype) {
                return Err(ExecutionError::NotObject(snapshot.prototype));
            }
            current = snapshot.prototype;
        }
        Ok(keys.finish())
    }

    /// Counts shape and virtual function keys before collection so snapshot vectors never grow.
    pub(crate) fn for_in_object_key_upper_bound(
        &mut self,
        source: Value,
    ) -> Result<usize, ExecutionError> {
        let mut count = 0_usize;
        let mut current = source;
        loop {
            if self.is_string_wrapper(current) {
                count = count
                    .checked_add(self.string_value_length(current)?)
                    .ok_or(ExecutionError::ForInKeyAllocationFailed)?;
            }
            let virtual_count = match self.resolve_function_object(current) {
                Ok(function) => match function.executable {
                    FunctionExecutable::Native(_) => 3,
                    FunctionExecutable::Bound(_)
                    | FunctionExecutable::ProxyRevoker(_)
                    | FunctionExecutable::PromiseResolver { .. }
                    | FunctionExecutable::PromiseCapabilityExecutor(_)
                    | FunctionExecutable::PromiseFinallyHandler { .. }
                    | FunctionExecutable::PromiseFinallyResultHandler { .. }
                    | FunctionExecutable::PromiseCombinatorHandler { .. } => 2,
                    FunctionExecutable::Bytecode { .. } | FunctionExecutable::ClassBytecode(_) => 3,
                },
                Err(_) => 0,
            };
            let (_, snapshot) = self.object_snapshot(current)?;
            count = count
                .checked_add(virtual_count)
                .and_then(|count| {
                    usize::try_from(self.shapes.property_count(snapshot.shape))
                        .ok()
                        .and_then(|properties| count.checked_add(properties))
                })
                .ok_or(ExecutionError::ForInKeyAllocationFailed)?;
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return Ok(count);
            }
            if !self.is_object_value(snapshot.prototype) {
                return Err(ExecutionError::NotObject(snapshot.prototype));
            }
            current = snapshot.prototype;
        }
    }

    /// Enumerates primitive string indices without retaining copies of their character values.
    pub(crate) fn for_in_string_keys(
        &mut self,
        string: GcRef<JsString>,
    ) -> Result<Box<[AtomId]>, ExecutionError> {
        let length = self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(string, self.types.string)
                    .map(|string| string.len())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(length)
            .map_err(|_| ExecutionError::ForInKeyAllocationFailed)?;
        for index in 0..length {
            let index =
                i32::try_from(index).map_err(|_| ExecutionError::ForInKeyAllocationFailed)?;
            keys.push(self.property_key_atom(Value::from_i32(index))?);
        }
        Ok(keys.into_boxed_slice())
    }

    /// Advances one verified internal iterator and materializes only the returned atom string.
    pub(crate) fn for_in_next(&mut self, iterator: Value) -> Result<Value, ExecutionError> {
        let raw = iterator
            .as_heap_ref()
            .ok_or(ExecutionError::InvalidForInIterator(iterator))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.for_in_iterator)
            .map_err(|_| ExecutionError::InvalidForInIterator(iterator))?;
        let key = self.heap.with_running_scope(|scope| {
            let iterator = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(iterator, self.types.for_in_iterator)
                    .map(ForInIterator::next)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        key.map_or_else(
            || Ok(Value::from_immediate(Immediate::Undefined)),
            |key| self.atom_string_value(key),
        )
    }
}

impl Trace for Isolate {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.trace_roots(tracer);
    }
}
