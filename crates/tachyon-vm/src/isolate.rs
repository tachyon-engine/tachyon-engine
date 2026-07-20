//! Isolate construction and allocation-oriented runtime orchestration.

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

/// A single-thread-owned ECMAScript execution state; `Cell` intentionally makes it `!Sync`.
pub struct Isolate {
    pub(crate) fiber: Fiber,
    pub(crate) finalization_jobs: finalization::FinalizationJobs,
    pub(crate) atoms: AtomTable,
    pub(crate) shapes: ShapeTable,
    pub(crate) realm: Realm,
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
    /// Registers VM payload descriptors before constructing an otherwise empty isolate heap.
    pub fn new(config: IsolateConfig) -> Result<Self, IsolateCreationError> {
        let mut registry = TypeRegistry::new();
        let types = VmTypes {
            accessor_pair: registry
                .try_register("AccessorPair")
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
            function: registry
                .try_register("FunctionObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            number_object: registry
                .try_register("NumberObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            string_object: registry
                .try_register("StringObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            ordinary_object: registry
                .try_register("OrdinaryObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_property_descriptor: registry
                .try_register("PendingPropertyDescriptor")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_native_property_key: registry
                .try_register("PendingNativePropertyKey")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_copy_data_properties: registry
                .try_register("PendingCopyDataProperties")
                .map_err(IsolateCreationError::TypeRegistration)?,
            pending_collection_initializer: registry
                .try_register("PendingCollectionInitializer")
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
            atoms: AtomTable::new(config.atom_table),
            shapes,
            realm: Realm::new(config.realm_limits, typeof_strings, primitive_hint_strings),
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

    /// Classifies a managed error through its intrinsic prototype chain without exposing heap IDs.
    pub fn native_error_kind(
        &mut self,
        value: Value,
    ) -> Result<Option<NativeErrorKind>, ExecutionError> {
        let mut current = value;
        loop {
            for kind in NativeErrorKind::ALL {
                if self.realm.error_intrinsics.get(kind).prototype == Some(current) {
                    return Ok(Some(kind));
                }
            }
            if !self.is_object_value(current) {
                return Ok(None);
            }
            let (_, snapshot) = self.object_snapshot(current)?;
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return Ok(None);
            }
            current = snapshot.prototype;
        }
    }

    pub(crate) fn allocate_intrinsic_ordinary_object(
        &mut self,
        ordinary: OrdinaryObject,
    ) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
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
                    function_prototype: None,
                    ordinary,
                },
                AllocationSpace::Old,
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
                    function_prototype: None,
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
        self.resolve_function_object(target)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
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
    pub(crate) fn for_in_keys(&mut self, source: Value) -> Result<Box<[AtomId]>, ExecutionError> {
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
        let upper_bound = self.for_in_object_key_upper_bound(source)?;
        let mut keys = ForInKeySet::with_upper_bound(upper_bound)
            .map_err(|_: ForInAllocationError| ExecutionError::ForInKeyAllocationFailed)?;
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
            let virtual_count = match self.resolve_function_object(current) {
                Ok(function) => match function.executable {
                    FunctionExecutable::Native(_) => 3,
                    FunctionExecutable::Bound(_) => 2,
                    FunctionExecutable::Bytecode { .. } => 3,
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

    /// Allocates one ordinary native error and defines a string message only when supplied.
    pub(crate) fn create_native_error(
        &mut self,
        kind: NativeErrorKind,
        message: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .error_intrinsics
            .get(kind)
            .prototype
            .expect("native Error prototypes initialize before execution");
        let error = self.create_ordinary_object_with_prototype(prototype)?;
        let Some(message) =
            message.filter(|value| value.as_immediate() != Some(Immediate::Undefined))
        else {
            return Ok(error);
        };
        let raw = message
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedErrorMessage(message))?;
        self.heap
            .checked_reference(raw, self.types.string)
            .map_err(|_| ExecutionError::UnsupportedErrorMessage(message))?;
        let message_atom = self.message_atom()?;
        self.set_own_data_property(error, message_atom, message)?;
        Ok(error)
    }
}

impl Trace for Isolate {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.trace_roots(tracer);
    }
}
