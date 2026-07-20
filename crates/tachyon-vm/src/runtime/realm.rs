//! Realm bindings, intrinsic roots, and stable global slot identities.

use super::super::*;

#[derive(Clone, Copy, Debug)]
pub(crate) struct GlobalBinding {
    pub(crate) name: AtomId,
    pub(crate) value: Value,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct IntrinsicBinding {
    pub(crate) name: AtomId,
    pub(crate) value: Value,
    pub(crate) writable: bool,
}

/// Stable isolate-local index into mandatory bindings excluded from the host user-binding quota.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(crate) struct IntrinsicSlotId(NonZeroU32);

impl IntrinsicSlotId {
    pub(crate) fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
    }

    pub(crate) const fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

const _: [(); 4] = [(); core::mem::size_of::<IntrinsicSlotId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<IntrinsicSlotId>>()];

/// A stable isolate-local index into one realm's global binding storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(crate) struct GlobalSlotId(NonZeroU32);

impl GlobalSlotId {
    pub(crate) fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
    }

    pub(crate) const fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

const _: [(); 4] = [(); core::mem::size_of::<GlobalSlotId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<GlobalSlotId>>()];

#[derive(Clone, Copy, Debug)]
pub(crate) struct GlobalLexicalBinding {
    pub(crate) name: AtomId,
    pub(crate) value: Value,
    pub(crate) mutable: bool,
    pub(crate) initialized: bool,
}

/// A stable isolate-local index into the declarative global environment record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(crate) struct GlobalLexicalSlotId(NonZeroU32);

impl GlobalLexicalSlotId {
    pub(crate) fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
    }

    pub(crate) const fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

const _: [(); 4] = [(); core::mem::size_of::<GlobalLexicalSlotId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<GlobalLexicalSlotId>>()];

#[derive(Debug)]
pub(crate) struct Realm {
    pub(crate) intrinsic_bindings: Vec<IntrinsicBinding>,
    pub(crate) intrinsic_slots_by_atom: Vec<Option<IntrinsicSlotId>>,
    pub(crate) global_lexicals: Vec<GlobalLexicalBinding>,
    pub(crate) global_lexical_slots_by_atom: Vec<Option<GlobalLexicalSlotId>>,
    pub(crate) global_bindings: Vec<GlobalBinding>,
    pub(crate) global_slots_by_atom: Vec<Option<GlobalSlotId>>,
    pub(crate) global_object: Option<Value>,
    pub(crate) function_prototype: Option<Value>,
    pub(crate) function_prototype_call: Option<Value>,
    pub(crate) function_prototype_bind: Option<Value>,
    pub(crate) array_constructor: Option<Value>,
    pub(crate) array_prototype: Option<Value>,
    pub(crate) array_is_array: Option<Value>,
    pub(crate) array_concat: Option<Value>,
    pub(crate) array_push: Option<Value>,
    pub(crate) array_join: Option<Value>,
    pub(crate) array_at: Option<Value>,
    pub(crate) array_index_of: Option<Value>,
    pub(crate) array_includes: Option<Value>,
    pub(crate) array_pop: Option<Value>,
    pub(crate) array_slice: Option<Value>,
    pub(crate) array_shift: Option<Value>,
    pub(crate) array_unshift: Option<Value>,
    pub(crate) array_reverse: Option<Value>,
    pub(crate) array_fill: Option<Value>,
    pub(crate) array_last_index_of: Option<Value>,
    pub(crate) array_copy_within: Option<Value>,
    pub(crate) array_flat: Option<Value>,
    pub(crate) array_sort: Option<Value>,
    pub(crate) array_to_string: Option<Value>,
    pub(crate) array_values: Option<Value>,
    pub(crate) array_iterator_prototype: Option<Value>,
    pub(crate) array_iterator_next: Option<Value>,
    pub(crate) iterator_identity: Option<Value>,
    pub(crate) object_constructor: Option<Value>,
    pub(crate) object_prototype: Option<Value>,
    pub(crate) object_define_property: Option<Value>,
    pub(crate) object_get_own_property_descriptor: Option<Value>,
    pub(crate) object_get_own_property_names: Option<Value>,
    pub(crate) object_has_own_property: Option<Value>,
    pub(crate) object_property_is_enumerable: Option<Value>,
    pub(crate) object_to_string: Option<Value>,
    pub(crate) object_assign: Option<Value>,
    pub(crate) object_keys: Option<Value>,
    pub(crate) object_values: Option<Value>,
    pub(crate) object_entries: Option<Value>,
    pub(crate) object_has_own: Option<Value>,
    pub(crate) object_is: Option<Value>,
    pub(crate) object_get_prototype_of: Option<Value>,
    pub(crate) object_create: Option<Value>,
    pub(crate) object_is_prototype_of: Option<Value>,
    pub(crate) object_is_extensible: Option<Value>,
    pub(crate) object_prevent_extensions: Option<Value>,
    pub(crate) string_constructor: Option<Value>,
    pub(crate) string_prototype: Option<Value>,
    pub(crate) regexp_constructor: Option<Value>,
    pub(crate) regexp_prototype: Option<Value>,
    pub(crate) symbol_constructor: Option<Value>,
    pub(crate) number_constructor: Option<Value>,
    pub(crate) number_prototype: Option<Value>,
    pub(crate) number_is_nan: Option<Value>,
    pub(crate) number_is_finite: Option<Value>,
    pub(crate) number_is_integer: Option<Value>,
    pub(crate) number_is_safe_integer: Option<Value>,
    pub(crate) number_to_exponential: Option<Value>,
    pub(crate) number_to_fixed: Option<Value>,
    pub(crate) number_to_precision: Option<Value>,
    pub(crate) number_to_string: Option<Value>,
    pub(crate) number_value_of: Option<Value>,
    pub(crate) boolean_constructor: Option<Value>,
    pub(crate) function_constructor: Option<Value>,
    pub(crate) math_object: Option<Value>,
    pub(crate) json_object: Option<Value>,
    pub(crate) json_parse: Option<Value>,
    pub(crate) json_stringify: Option<Value>,
    pub(crate) math_pow: Option<Value>,
    pub(crate) math_functions: [Option<Value>; MathFunction::ALL.len()],
    pub(crate) global_number_functions: [Option<Value>; GlobalNumberFunction::ALL.len()],
    pub(crate) error_intrinsics: ErrorIntrinsics,
    pub(crate) well_known_symbols: WellKnownSymbols,
    pub(crate) primitive_hint_strings: PrimitiveHintStrings,
    pub(crate) typeof_strings: TypeofStrings,
    pub(crate) limits: RealmLimits,
}

impl Realm {
    pub(crate) fn new(
        limits: RealmLimits,
        typeof_strings: TypeofStrings,
        primitive_hint_strings: PrimitiveHintStrings,
    ) -> Self {
        Self {
            intrinsic_bindings: Vec::new(),
            intrinsic_slots_by_atom: Vec::new(),
            global_lexicals: Vec::new(),
            global_lexical_slots_by_atom: Vec::new(),
            global_bindings: Vec::new(),
            global_slots_by_atom: Vec::new(),
            global_object: None,
            function_prototype: None,
            function_prototype_call: None,
            function_prototype_bind: None,
            array_constructor: None,
            array_prototype: None,
            array_is_array: None,
            array_concat: None,
            array_push: None,
            array_join: None,
            array_at: None,
            array_index_of: None,
            array_includes: None,
            array_pop: None,
            array_slice: None,
            array_shift: None,
            array_unshift: None,
            array_reverse: None,
            array_fill: None,
            array_last_index_of: None,
            array_copy_within: None,
            array_flat: None,
            array_sort: None,
            array_to_string: None,
            array_values: None,
            array_iterator_prototype: None,
            array_iterator_next: None,
            iterator_identity: None,
            object_constructor: None,
            object_prototype: None,
            object_define_property: None,
            object_get_own_property_descriptor: None,
            object_get_own_property_names: None,
            object_has_own_property: None,
            object_property_is_enumerable: None,
            object_to_string: None,
            object_assign: None,
            object_keys: None,
            object_values: None,
            object_entries: None,
            object_has_own: None,
            object_is: None,
            object_get_prototype_of: None,
            object_create: None,
            object_is_prototype_of: None,
            object_is_extensible: None,
            object_prevent_extensions: None,
            string_constructor: None,
            string_prototype: None,
            regexp_constructor: None,
            regexp_prototype: None,
            symbol_constructor: None,
            number_constructor: None,
            number_prototype: None,
            number_is_nan: None,
            number_is_finite: None,
            number_is_integer: None,
            number_is_safe_integer: None,
            number_to_exponential: None,
            number_to_fixed: None,
            number_to_precision: None,
            number_to_string: None,
            number_value_of: None,
            boolean_constructor: None,
            function_constructor: None,
            math_object: None,
            json_object: None,
            json_parse: None,
            json_stringify: None,
            math_pow: None,
            math_functions: [None; MathFunction::ALL.len()],
            global_number_functions: [None; GlobalNumberFunction::ALL.len()],
            error_intrinsics: ErrorIntrinsics::default(),
            well_known_symbols: WellKnownSymbols::default(),
            primitive_hint_strings,
            typeof_strings,
            limits,
        }
    }

    /// Reserves the complete mandatory binding set before any intrinsic becomes observable.
    pub(crate) fn reserve_intrinsics(
        &mut self,
        binding_count: usize,
        atom_upper_bound: usize,
    ) -> Result<(), ExecutionError> {
        self.intrinsic_bindings
            .try_reserve_exact(binding_count)
            .map_err(|_| ExecutionError::IntrinsicBindingAllocationFailed)?;
        self.intrinsic_slots_by_atom
            .try_reserve_exact(atom_upper_bound)
            .map_err(|_| ExecutionError::IntrinsicBindingIndexAllocationFailed)?;
        self.intrinsic_slots_by_atom.resize(atom_upper_bound, None);
        Ok(())
    }

    /// Publishes one pre-reserved intrinsic with stable identity and explicit writability.
    pub(crate) fn publish_intrinsic(
        &mut self,
        name: AtomId,
        value: Value,
        writable: bool,
    ) -> Result<(), ExecutionError> {
        let slot = IntrinsicSlotId::from_index(self.intrinsic_bindings.len())
            .ok_or(ExecutionError::IntrinsicBindingAllocationFailed)?;
        let target = self
            .intrinsic_slots_by_atom
            .get_mut(name.index() as usize)
            .ok_or(ExecutionError::IntrinsicBindingIndexAllocationFailed)?;
        debug_assert!(target.is_none());
        self.intrinsic_bindings.push(IntrinsicBinding {
            name,
            value,
            writable,
        });
        *target = Some(slot);
        Ok(())
    }

    #[inline(always)]
    pub(crate) fn resolve_intrinsic(&self, name: AtomId) -> Option<IntrinsicSlotId> {
        let slot = self
            .intrinsic_slots_by_atom
            .get(name.index() as usize)
            .copied()
            .flatten()?;
        debug_assert_eq!(self.intrinsic_bindings[slot.index()].name, name);
        Some(slot)
    }

    #[inline(always)]
    pub(crate) fn intrinsic_value(&self, slot: IntrinsicSlotId) -> Value {
        self.intrinsic_bindings[slot.index()].value
    }

    #[inline(always)]
    pub(crate) fn set_intrinsic(
        &mut self,
        slot: IntrinsicSlotId,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let binding = &mut self.intrinsic_bindings[slot.index()];
        if !binding.writable {
            return Err(ExecutionError::ReadOnlyBinding(binding.name));
        }
        binding.value = value;
        Ok(())
    }

    #[inline(always)]
    pub(crate) fn resolve_lexical(&self, name: AtomId) -> Option<GlobalLexicalSlotId> {
        let slot = self
            .global_lexical_slots_by_atom
            .get(name.index() as usize)
            .copied()
            .flatten()?;
        debug_assert_eq!(self.global_lexicals[slot.index()].name, name);
        Some(slot)
    }

    pub(crate) fn lexical_value(&self, slot: GlobalLexicalSlotId) -> Result<Value, ExecutionError> {
        let binding = &self.global_lexicals[slot.index()];
        if binding.initialized {
            Ok(binding.value)
        } else {
            Err(ExecutionError::UninitializedBinding(binding.name))
        }
    }

    /// Publishes an uninitialized declarative binding after reserving both stable-index tables.
    pub(crate) fn declare_lexical(
        &mut self,
        name: AtomId,
        mutable: bool,
    ) -> Result<(), ExecutionError> {
        if self.resolve_lexical(name).is_some()
            || self.resolve_intrinsic(name).is_some()
            || self.resolve(name).is_some()
        {
            return Err(ExecutionError::GlobalLexicalRedeclaration(name));
        }
        if self
            .global_lexicals
            .len()
            .saturating_add(self.global_bindings.len())
            >= self.limits.max_global_bindings as usize
        {
            return Err(ExecutionError::GlobalBindingLimit {
                limit: self.limits.max_global_bindings,
            });
        }
        let required_slots = (name.index() as usize)
            .checked_add(1)
            .ok_or(ExecutionError::GlobalBindingIndexAllocationFailed)?;
        let additional_slots =
            required_slots.saturating_sub(self.global_lexical_slots_by_atom.len());
        self.global_lexical_slots_by_atom
            .try_reserve_exact(additional_slots)
            .map_err(|_| ExecutionError::GlobalBindingIndexAllocationFailed)?;
        self.global_lexicals
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::GlobalBindingAllocationFailed)?;
        self.global_lexical_slots_by_atom
            .resize(required_slots, None);
        let slot = GlobalLexicalSlotId::from_index(self.global_lexicals.len())
            .ok_or(ExecutionError::GlobalBindingLimit { limit: u32::MAX })?;
        self.global_lexicals.push(GlobalLexicalBinding {
            name,
            value: Value::from_immediate(Immediate::Undefined),
            mutable,
            initialized: false,
        });
        self.global_lexical_slots_by_atom[name.index() as usize] = Some(slot);
        Ok(())
    }

    pub(crate) fn initialize_lexical(
        &mut self,
        slot: GlobalLexicalSlotId,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let binding = &mut self.global_lexicals[slot.index()];
        if binding.initialized {
            return Err(ExecutionError::GlobalLexicalAlreadyInitialized(
                binding.name,
            ));
        }
        binding.value = value;
        binding.initialized = true;
        Ok(())
    }

    pub(crate) fn set_lexical(
        &mut self,
        slot: GlobalLexicalSlotId,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let binding = &mut self.global_lexicals[slot.index()];
        if !binding.initialized {
            return Err(ExecutionError::UninitializedBinding(binding.name));
        }
        if !binding.mutable {
            return Err(ExecutionError::ImmutableBinding(binding.name));
        }
        binding.value = value;
        Ok(())
    }

    #[inline(always)]
    pub(crate) fn get_slot(&self, slot: GlobalSlotId) -> Option<Value> {
        self.global_bindings
            .get(slot.index())
            .map(|binding| binding.value)
    }

    #[inline(always)]
    pub(crate) fn set_slot(&mut self, slot: GlobalSlotId, value: Value) {
        self.global_bindings[slot.index()].value = value;
    }

    /// Updates an existing slot or atomically publishes one after both backing reserves succeed.
    pub(crate) fn set(&mut self, name: AtomId, value: Value) -> Result<(), ExecutionError> {
        if self.resolve_lexical(name).is_some() {
            return Err(ExecutionError::GlobalLexicalRedeclaration(name));
        }
        if let Some(slot) = self.resolve_intrinsic(name) {
            return self.set_intrinsic(slot, value);
        }
        if let Some(slot) = self.resolve(name) {
            self.set_slot(slot, value);
            return Ok(());
        }
        if self
            .global_lexicals
            .len()
            .saturating_add(self.global_bindings.len())
            >= self.limits.max_global_bindings as usize
        {
            return Err(ExecutionError::GlobalBindingLimit {
                limit: self.limits.max_global_bindings,
            });
        }
        let required_slots = (name.index() as usize)
            .checked_add(1)
            .ok_or(ExecutionError::GlobalBindingIndexAllocationFailed)?;
        let additional_slots = required_slots.saturating_sub(self.global_slots_by_atom.len());
        self.global_slots_by_atom
            .try_reserve_exact(additional_slots)
            .map_err(|_| ExecutionError::GlobalBindingIndexAllocationFailed)?;
        self.global_bindings
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::GlobalBindingAllocationFailed)?;
        self.global_slots_by_atom.resize(required_slots, None);
        let slot = GlobalSlotId::from_index(self.global_bindings.len())
            .ok_or(ExecutionError::GlobalBindingLimit { limit: u32::MAX })?;
        self.global_bindings.push(GlobalBinding { name, value });
        self.global_slots_by_atom[name.index() as usize] = Some(slot);
        Ok(())
    }

    #[inline(always)]
    pub(crate) fn resolve(&self, name: AtomId) -> Option<GlobalSlotId> {
        let slot = self
            .global_slots_by_atom
            .get(name.index() as usize)
            .copied()
            .flatten()?;
        debug_assert_eq!(self.global_bindings[slot.index()].name, name);
        Some(slot)
    }
}

impl Trace for Realm {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for binding in &mut self.intrinsic_bindings {
            binding.value.trace(tracer);
        }
        for binding in &mut self.global_lexicals {
            binding.value.trace(tracer);
        }
        for binding in &mut self.global_bindings {
            binding.value.trace(tracer);
        }
        self.global_object.trace(tracer);
        self.function_prototype.trace(tracer);
        self.function_prototype_call.trace(tracer);
        self.function_prototype_bind.trace(tracer);
        self.array_constructor.trace(tracer);
        self.array_prototype.trace(tracer);
        self.array_is_array.trace(tracer);
        self.array_concat.trace(tracer);
        self.array_push.trace(tracer);
        self.array_join.trace(tracer);
        self.array_at.trace(tracer);
        self.array_index_of.trace(tracer);
        self.array_includes.trace(tracer);
        self.array_pop.trace(tracer);
        self.array_slice.trace(tracer);
        self.array_shift.trace(tracer);
        self.array_unshift.trace(tracer);
        self.array_reverse.trace(tracer);
        self.array_fill.trace(tracer);
        self.array_last_index_of.trace(tracer);
        self.array_copy_within.trace(tracer);
        self.array_flat.trace(tracer);
        self.array_sort.trace(tracer);
        self.array_to_string.trace(tracer);
        self.array_values.trace(tracer);
        self.array_iterator_prototype.trace(tracer);
        self.array_iterator_next.trace(tracer);
        self.iterator_identity.trace(tracer);
        self.object_constructor.trace(tracer);
        self.object_prototype.trace(tracer);
        self.object_define_property.trace(tracer);
        self.object_get_own_property_descriptor.trace(tracer);
        self.object_get_own_property_names.trace(tracer);
        self.object_has_own_property.trace(tracer);
        self.object_property_is_enumerable.trace(tracer);
        self.object_to_string.trace(tracer);
        self.object_assign.trace(tracer);
        self.object_keys.trace(tracer);
        self.object_values.trace(tracer);
        self.object_entries.trace(tracer);
        self.object_has_own.trace(tracer);
        self.object_is.trace(tracer);
        self.object_get_prototype_of.trace(tracer);
        self.object_create.trace(tracer);
        self.object_is_prototype_of.trace(tracer);
        self.object_is_extensible.trace(tracer);
        self.object_prevent_extensions.trace(tracer);
        self.string_constructor.trace(tracer);
        self.regexp_constructor.trace(tracer);
        self.regexp_prototype.trace(tracer);
        self.symbol_constructor.trace(tracer);
        self.number_constructor.trace(tracer);
        self.number_prototype.trace(tracer);
        self.number_is_nan.trace(tracer);
        self.number_is_finite.trace(tracer);
        self.number_is_integer.trace(tracer);
        self.number_is_safe_integer.trace(tracer);
        self.number_to_exponential.trace(tracer);
        self.number_to_fixed.trace(tracer);
        self.number_to_precision.trace(tracer);
        self.number_to_string.trace(tracer);
        self.number_value_of.trace(tracer);
        self.boolean_constructor.trace(tracer);
        self.function_constructor.trace(tracer);
        self.math_object.trace(tracer);
        self.math_pow.trace(tracer);
        self.math_functions.trace(tracer);
        self.global_number_functions.trace(tracer);
        self.error_intrinsics.trace(tracer);
        self.well_known_symbols.trace(tracer);
        self.primitive_hint_strings.trace(tracer);
        self.typeof_strings.trace(tracer);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WellKnownSymbols {
    pub(crate) to_primitive: Option<Value>,
    pub(crate) iterator: Option<Value>,
}

impl Trace for WellKnownSymbols {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.to_primitive.trace(tracer);
        self.iterator.trace(tracer);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PrimitiveHintStrings {
    pub(crate) default: Value,
    pub(crate) string: Value,
    pub(crate) number: Value,
}

impl PrimitiveHintStrings {
    /// Allocates the complete ToPrimitive hint vocabulary before realm publication.
    pub(crate) fn allocate(
        heap: &mut Heap,
        string_type: GcType<JsString>,
    ) -> Result<Self, IsolateCreationError> {
        Ok(Self {
            default: allocate_initial_string(heap, string_type, b"default")?,
            string: allocate_initial_string(heap, string_type, b"string")?,
            number: allocate_initial_string(heap, string_type, b"number")?,
        })
    }

    #[inline]
    pub(crate) const fn get(self, preferred_type: PreferredType) -> Value {
        match preferred_type {
            PreferredType::Default => self.default,
            PreferredType::String => self.string,
            PreferredType::Number => self.number,
        }
    }
}

impl Trace for PrimitiveHintStrings {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.default.trace(tracer);
        self.string.trace(tracer);
        self.number.trace(tracer);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TypeofStrings {
    pub(crate) undefined: Value,
    pub(crate) object: Value,
    pub(crate) boolean: Value,
    pub(crate) number: Value,
    pub(crate) string: Value,
    pub(crate) function: Value,
    pub(crate) symbol: Value,
    pub(crate) bigint: Value,
}

impl Trace for TypeofStrings {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.undefined.trace(tracer);
        self.object.trace(tracer);
        self.boolean.trace(tracer);
        self.number.trace(tracer);
        self.string.trace(tracer);
        self.function.trace(tracer);
        self.symbol.trace(tracer);
        self.bigint.trace(tracer);
    }
}

impl TypeofStrings {
    /// Allocates the complete spec-fixed typeof vocabulary once before the isolate becomes visible.
    pub(crate) fn allocate(
        heap: &mut Heap,
        string_type: GcType<JsString>,
    ) -> Result<Self, IsolateCreationError> {
        Ok(Self {
            undefined: allocate_initial_string(heap, string_type, b"undefined")?,
            object: allocate_initial_string(heap, string_type, b"object")?,
            boolean: allocate_initial_string(heap, string_type, b"boolean")?,
            number: allocate_initial_string(heap, string_type, b"number")?,
            string: allocate_initial_string(heap, string_type, b"string")?,
            function: allocate_initial_string(heap, string_type, b"function")?,
            symbol: allocate_initial_string(heap, string_type, b"symbol")?,
            bigint: allocate_initial_string(heap, string_type, b"bigint")?,
        })
    }
}

fn allocate_initial_string(
    heap: &mut Heap,
    string_type: GcType<JsString>,
    bytes: &[u8],
) -> Result<Value, IsolateCreationError> {
    let string = JsString::try_from_latin1(bytes).map_err(IsolateCreationError::String)?;
    let reference = heap
        .try_allocate_external(string_type, 0, string, AllocationSpace::Old)
        .map_err(IsolateCreationError::HeapAllocation)?;
    Ok(Value::from_heap_ref(reference.raw()))
}
