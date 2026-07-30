//! Object constructor, static method, and prototype slow paths.

use super::super::*;

/// GC-managed state for observable Proxy ownKeys/descriptor enumeration.
#[derive(Debug)]
pub(crate) struct PendingGetOwnPropertyDescriptors {
    pub(crate) result: Value,
    pub(crate) source: Value,
    pub(crate) keys: Box<[PropertyKey]>,
    pub(crate) index: usize,
}

impl Trace for PendingGetOwnPropertyDescriptors {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.result.trace(tracer);
        self.source.trace(tracer);
        self.keys.trace(tracer);
    }
}

impl GcExternalMemory for PendingGetOwnPropertyDescriptors {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.keys.len() * core::mem::size_of::<PropertyKey>()
    }
}

#[derive(Clone, Copy)]
struct GetOwnPropertyDescriptorsSnapshot {
    result: Value,
    source: Value,
    key: Option<PropertyKey>,
}

/// Compact spec fallback selected before the observable `@@toStringTag` lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum ObjectBuiltinTag {
    Object,
    Array,
    Arguments,
    Function,
    Error,
    Boolean,
    Number,
    String,
    Date,
    RegExp,
}

impl ObjectBuiltinTag {
    #[inline(always)]
    const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Object),
            1 => Some(Self::Array),
            2 => Some(Self::Arguments),
            3 => Some(Self::Function),
            4 => Some(Self::Error),
            5 => Some(Self::Boolean),
            6 => Some(Self::Number),
            7 => Some(Self::String),
            8 => Some(Self::Date),
            9 => Some(Self::RegExp),
            _ => None,
        }
    }

    #[inline(always)]
    const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Object => b"Object",
            Self::Array => b"Array",
            Self::Arguments => b"Arguments",
            Self::Function => b"Function",
            Self::Error => b"Error",
            Self::Boolean => b"Boolean",
            Self::Number => b"Number",
            Self::String => b"String",
            Self::Date => b"Date",
            Self::RegExp => b"RegExp",
        }
    }
}

impl Isolate {
    /// Begins the observable Get/Call sequence for Object.prototype.toLocaleString.
    pub(crate) fn begin_object_to_locale_string(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let receiver = site.this_value;
        if matches!(
            receiver.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(receiver));
        }
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let continuation = NativeContinuation::object_to_locale_string(
            native_site,
            ObjectToLocaleStringStage::Get,
            receiver,
        );
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        let to_string = self.intern_intrinsic_name(b"toString")?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(
            native_site,
            receiver,
            receiver,
            to_string.into(),
        ) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let callee = self.read(native_site.caller_base, native_site.destination)?;
        self.resume_object_to_locale_string(continuation, ObjectToLocaleStringStage::Get, callee)
    }

    /// Resumes either the toString lookup or its receiver-preserving zero-argument call.
    pub(crate) fn resume_object_to_locale_string(
        &mut self,
        continuation: NativeContinuation,
        stage: ObjectToLocaleStringStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match stage {
            ObjectToLocaleStringStage::Get => {
                self.resolve_function_object(value)?;
                self.dispatch_property_callback(
                    NativeContinuation::object_to_locale_string(
                        continuation.site(),
                        ObjectToLocaleStringStage::Call,
                        continuation.first(),
                    ),
                    value,
                )?;
                Ok(())
            }
            ObjectToLocaleStringStage::Call => self.write(
                continuation.site().caller_base,
                continuation.site().destination,
                value,
            ),
        }
    }

    /// Applies the ordinary-object portion of SetIntegrityLevel without rebuilding storage.
    pub(crate) fn object_set_integrity_level(
        &mut self,
        value: Value,
        freeze: bool,
    ) -> Result<Value, ExecutionError> {
        if !self.is_object_value(value) {
            return Ok(value);
        }
        let (_, snapshot) = self.object_snapshot(value)?;
        let keys = self.ordinary_own_property_keys(value, snapshot)?;
        for key in keys {
            let Some(descriptor) = self.complete_own_property_descriptor(value, key)? else {
                continue;
            };
            let descriptor = match descriptor {
                PropertyDescriptor::Data(_) => PropertyDescriptor::Data(DataPropertyDescriptor {
                    value: None,
                    writable: freeze.then_some(false),
                    enumerable: None,
                    configurable: Some(false),
                }),
                PropertyDescriptor::Accessor(_) => {
                    PropertyDescriptor::Generic(GenericPropertyDescriptor {
                        enumerable: None,
                        configurable: Some(false),
                    })
                }
                PropertyDescriptor::Generic(_) => continue,
            };
            self.define_property(value, key, descriptor)?;
        }
        let (receiver, _) = self.object_snapshot(value)?;
        self.set_object_extensible(receiver, false)?;
        Ok(value)
    }

    /// Implements TestIntegrityLevel for ordinary objects using complete own descriptors.
    pub(crate) fn object_test_integrity_level(
        &mut self,
        value: Value,
        freeze: bool,
    ) -> Result<bool, ExecutionError> {
        if !self.is_object_value(value) {
            return Ok(true);
        }
        let (_, snapshot) = self.object_snapshot(value)?;
        if snapshot.extensible {
            return Ok(false);
        }
        let keys = self.ordinary_own_property_keys(value, snapshot)?;
        for key in keys {
            let Some(descriptor) = self.complete_own_property_descriptor(value, key)? else {
                continue;
            };
            if descriptor.configurable() == Some(true)
                || (freeze
                    && matches!(
                        descriptor,
                        PropertyDescriptor::Data(DataPropertyDescriptor {
                            writable: Some(true),
                            ..
                        })
                    ))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Materializes every own String and Symbol key in the specified ordinary order.
    pub(crate) fn reflect_own_keys(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        let result = self.create_array_from_site(&CallSite {
            argument_count: 0,
            ..*site
        })?;
        let (_, snapshot) = self.object_snapshot(target)?;
        let keys = self.ordinary_own_property_keys(target, snapshot)?;
        let mut index = 0_u64;
        for key in keys {
            let value = match key {
                PropertyKey::Atom(atom) => self.atom_string_value(atom)?,
                PropertyKey::Symbol(symbol) => symbol.value(),
                PropertyKey::Private(_) => {
                    return Err(ExecutionError::PrivatePropertyKeyEscaped);
                }
            };
            let output = self.safe_integer_property_atom(index)?;
            self.set_own_data_property(result, output, value)?;
            index = index
                .checked_add(1)
                .ok_or(ExecutionError::ArrayLengthOverflow)?;
        }
        let length = self.length_atom()?;
        self.set_own_data_property(result, length, safe_integer_value(index))?;
        Ok(result)
    }

    /// Returns an ordinary target's current prototype and rejects primitive Reflect targets.
    pub(crate) fn reflect_get_prototype_of(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        Ok(self.object_snapshot(target)?.1.prototype)
    }

    /// Reports ordinary extensibility while preserving Reflect's object-only input boundary.
    pub(crate) fn reflect_is_extensible(
        &mut self,
        site: &CallSite,
    ) -> Result<bool, ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        Ok(self.object_snapshot(target)?.1.extensible)
    }

    /// Makes an ordinary object non-extensible and returns the internal-method boolean result.
    pub(crate) fn reflect_prevent_extensions(
        &mut self,
        site: &CallSite,
    ) -> Result<bool, ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        let (receiver, _) = self.object_snapshot(target)?;
        self.set_object_extensible(receiver, false)?;
        Ok(true)
    }

    /// Starts Reflect.defineProperty after its strict target check and resumable key conversion.
    pub(crate) fn reflect_define_property(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let descriptor = self
            .call_argument(site, 2)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        self.dispatch_builtin_property_key(
            BuiltinPropertyKeyConsumer::ReflectDefineProperty,
            site,
            target,
            key,
            descriptor,
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Starts Reflect.deleteProperty after its strict target check and resumable key conversion.
    pub(crate) fn reflect_delete_property(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        self.dispatch_builtin_property_key(
            BuiltinPropertyKeyConsumer::ReflectDeleteProperty,
            site,
            target,
            key,
            Value::from_immediate(Immediate::Undefined),
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Starts Reflect.getOwnPropertyDescriptor with its object-only target boundary.
    pub(crate) fn reflect_get_own_property_descriptor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        self.dispatch_builtin_property_key(
            BuiltinPropertyKeyConsumer::ReflectGetOwnPropertyDescriptor,
            site,
            target,
            key,
            Value::from_immediate(Immediate::Undefined),
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Starts Reflect.has after its strict target check and resumable key conversion.
    pub(crate) fn reflect_has(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        self.dispatch_builtin_property_key(
            BuiltinPropertyKeyConsumer::ReflectHas,
            site,
            target,
            key,
            Value::from_immediate(Immediate::Undefined),
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Starts Reflect.get while keeping its target lookup and accessor receiver distinct.
    pub(crate) fn reflect_get(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let receiver = self.call_argument(site, 2)?.unwrap_or(target);
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        self.dispatch_builtin_property_key(
            BuiltinPropertyKeyConsumer::ReflectGet,
            site,
            target,
            key,
            receiver,
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Starts Reflect.set while retaining value and receiver through object-key conversion.
    pub(crate) fn reflect_set(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let value = self
            .call_argument(site, 2)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let receiver = self.call_argument(site, 3)?.unwrap_or(target);
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        self.dispatch_builtin_property_key(
            BuiltinPropertyKeyConsumer::ReflectSet,
            site,
            target,
            key,
            value,
            receiver,
        )
    }

    /// Implements Reflect.setPrototypeOf through the ordinary prototype mutation contract.
    pub(crate) fn reflect_set_prototype_of(
        &mut self,
        site: &CallSite,
    ) -> Result<bool, ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let prototype = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(target) {
            return Err(ExecutionError::NotObject(target));
        }
        self.ordinary_set_prototype_of(target, prototype)
    }

    /// Implements Object.setPrototypeOf validation, primitive return, and throw-on-false semantics.
    pub(crate) fn object_set_prototype_of(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if matches!(
            target.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(target));
        }
        let prototype = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if prototype.as_immediate() != Some(Immediate::Null) && !self.is_object_value(prototype) {
            return Err(ExecutionError::NotObject(prototype));
        }
        if !self.is_object_value(target) {
            return Ok(target);
        }
        if !self.ordinary_set_prototype_of(target, prototype)? {
            return Err(ExecutionError::NonExtensibleObject(target));
        }
        Ok(target)
    }

    /// Implements the ordinary Object constructor for object values and primitive fallback values.
    pub(crate) fn create_object_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        if let Some(value) = self.call_argument(site, 0)? {
            if self.is_object_value(value) {
                return Ok(value);
            }
            if !matches!(
                value.as_immediate(),
                Some(Immediate::Undefined | Immediate::Null)
            ) {
                return self.box_object_primitive(value);
            }
        }
        let object = self.create_ordinary_object()?;
        self.write(site.caller_base, site.destination, object)?;
        Ok(object)
    }

    /// Implements Object.prototype.valueOf by applying the specification's ToObject operation.
    pub(crate) fn object_value_of(&mut self, value: Value) -> Result<Value, ExecutionError> {
        self.coerce_to_object(value)
    }

    /// Applies the shared ECMAScript ToObject operation without exposing host allocation policy.
    pub(crate) fn coerce_to_object(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if self.is_object_value(value) {
            return Ok(value);
        }
        if matches!(
            value.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(value));
        }
        self.box_object_primitive(value)
    }

    /// Boxes one non-nullish primitive using an existing truthful wrapper representation.
    fn box_object_primitive(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if self.is_string_value(value) {
            let prototype = self
                .realm
                .string_prototype
                .expect("String prototype initializes before primitive boxing");
            return self.allocate_string_object(value, prototype, AllocationSpace::Young);
        }
        if numeric_value(value).is_some() {
            let prototype = self
                .realm
                .number_prototype
                .expect("Number prototype initializes before primitive boxing");
            return self.allocate_number_object(value, prototype, AllocationSpace::Young);
        }
        if self.is_bigint_value(value) {
            return self.box_bigint(value);
        }
        if self.is_symbol_value(value) {
            return self.box_symbol(value);
        }
        if matches!(
            value.as_immediate(),
            Some(Immediate::True | Immediate::False)
        ) {
            let prototype = self
                .realm
                .boolean_prototype
                .expect("Boolean prototype initializes before primitive boxing");
            return self.allocate_boolean_object(value, prototype, AllocationSpace::Young);
        }
        self.create_ordinary_object()
    }

    /// Starts Object.defineProperty and leaves any observable descriptor getters in the VM loop.
    pub(crate) fn object_define_property(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let object = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let descriptor = self
            .call_argument(site, 2)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_object_value(object) {
            return Err(ExecutionError::NotObject(object));
        }
        self.dispatch_builtin_property_key(
            BuiltinPropertyKeyConsumer::DefineProperty,
            site,
            object,
            key,
            descriptor,
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Materializes one own data or accessor descriptor, or undefined when the key is absent.
    pub(crate) fn object_get_own_property_descriptor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let object = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if matches!(
            object.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(object));
        }
        let object = self.object_value_of(object)?;
        let key = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.dispatch_builtin_property_key(
            BuiltinPropertyKeyConsumer::GetOwnPropertyDescriptor,
            site,
            object,
            key,
            Value::from_immediate(Immediate::Undefined),
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Starts ordinary or Proxy-aware Object.getOwnPropertyDescriptors enumeration.
    pub(crate) fn begin_object_get_own_property_descriptors(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let source = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if is_nullish(source) {
            return Err(ExecutionError::NotObject(source));
        }
        let source = self.object_value_of(source)?;
        let result = self.create_ordinary_object()?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.is_proxy_value(source) {
            let state =
                self.allocate_get_own_property_descriptors_state(result, source, Vec::new())?;
            self.write(
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
            )?;
            return self
                .dispatch_get_own_property_descriptors_own_keys(native_site, state, source)
                .map(|_| ());
        }
        self.materialize_ordinary_own_property_descriptors(source, result)?;
        self.write(site.caller_base, site.destination, result)
    }

    fn materialize_ordinary_own_property_descriptors(
        &mut self,
        source: Value,
        result: Value,
    ) -> Result<(), ExecutionError> {
        let (_, snapshot) = self.object_snapshot(source)?;
        let keys = self.ordinary_own_property_keys(source, snapshot)?;
        for key in keys {
            let Some(descriptor) = self.complete_own_property_descriptor(source, key)? else {
                continue;
            };
            let descriptor_object = self.create_ordinary_object()?;
            self.materialize_property_descriptor(descriptor_object, descriptor)?;
            self.set_own_data_property(result, key, descriptor_object)?;
        }
        Ok(())
    }

    /// Resumes Proxy ownKeys or one materialized getOwnPropertyDescriptor result.
    pub(crate) fn resume_get_own_property_descriptors(
        &mut self,
        continuation: NativeContinuation,
        stage: GetOwnPropertyDescriptorsStage,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let site = continuation.site();
        let state = self.get_own_property_descriptors_reference(continuation.first())?;
        match stage {
            GetOwnPropertyDescriptorsStage::OwnKeys => {
                self.resume_get_own_property_descriptors_own_keys(site, state, value)
            }
            GetOwnPropertyDescriptorsStage::Descriptor => {
                let key = self.property_key(continuation.second())?;
                if value.as_immediate() != Some(Immediate::Undefined) {
                    let result = self.get_own_property_descriptors_snapshot(state)?.result;
                    self.set_own_data_property(result, key, value)?;
                }
                self.advance_get_own_property_descriptors_index(state)?;
                self.advance_get_own_property_descriptors(site, state)
            }
        }
    }

    fn resume_get_own_property_descriptors_own_keys(
        &mut self,
        site: NativeContinuationSite,
        old_state: GcRef<PendingGetOwnPropertyDescriptors>,
        keys_array: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let length_key = PropertyKey::Atom(self.length_atom()?);
        let length = self
            .get_data_property(keys_array, length_key)?
            .and_then(numeric_value)
            .ok_or(ExecutionError::ArrayLengthOverflow)? as usize;
        let mut keys = Vec::new();
        keys.try_reserve_exact(length)
            .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
        for index in 0..length {
            let index_key = PropertyKey::Atom(self.safe_integer_property_atom(index as u64)?);
            let key = self
                .get_data_property(keys_array, index_key)?
                .ok_or(ExecutionError::ProxyInvariantViolation)?;
            keys.push(self.property_key(key)?);
        }
        let pending = self.get_own_property_descriptors_snapshot(old_state)?;
        let state =
            self.allocate_get_own_property_descriptors_state(pending.result, pending.source, keys)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.advance_get_own_property_descriptors(site, state)
    }

    fn advance_get_own_property_descriptors(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingGetOwnPropertyDescriptors>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let pending = self.get_own_property_descriptors_snapshot(state)?;
        let Some(key) = pending.key else {
            self.write(site.caller_base, site.destination, pending.result)?;
            return Ok(None);
        };
        let key_value = self.object_descriptor_key_value(key)?;
        let continuation = NativeContinuation::get_own_property_descriptors_stage(
            site,
            GetOwnPropertyDescriptorsStage::Descriptor,
            Value::from_heap_ref(state.raw()),
            key_value,
        );
        self.dispatch_get_own_property_descriptors_proxy_operation(continuation, |isolate| {
            isolate.dispatch_proxy_get_own(
                site,
                pending.source,
                key_value,
                ProxyGetOwnMode::Descriptor,
            )
        })
    }

    fn dispatch_get_own_property_descriptors_own_keys(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingGetOwnPropertyDescriptors>,
        source: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let continuation = NativeContinuation::get_own_property_descriptors_stage(
            site,
            GetOwnPropertyDescriptorsStage::OwnKeys,
            Value::from_heap_ref(state.raw()),
            Value::from_immediate(Immediate::Undefined),
        );
        self.dispatch_get_own_property_descriptors_proxy_operation(continuation, |isolate| {
            isolate.dispatch_proxy_own_keys(site, source, ProxyOwnKeysMode::Internal)
        })
    }

    /// Runs one Proxy operation and drains its parent on synchronous completion.
    fn dispatch_get_own_property_descriptors_proxy_operation(
        &mut self,
        continuation: NativeContinuation,
        operation: impl FnOnce(&mut Self) -> Result<Option<RunOutcome>, ExecutionError>,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let outcome = operation(self);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() == completion_depth
        {
            return outcome;
        }
        let continuation = self.pop_native_continuation()?;
        let site = continuation.site();
        let value = self.read(site.caller_base, site.destination)?;
        let NativeContinuationKind::GetOwnPropertyDescriptors(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_get_own_property_descriptors(continuation, stage, value)
    }

    fn object_descriptor_key_value(&mut self, key: PropertyKey) -> Result<Value, ExecutionError> {
        match key {
            PropertyKey::Atom(atom) => self.atom_string_value(atom),
            PropertyKey::Symbol(symbol) => Ok(symbol.value()),
            PropertyKey::Private(_) => Err(ExecutionError::PrivatePropertyKeyEscaped),
        }
    }

    fn allocate_get_own_property_descriptors_state(
        &mut self,
        result: Value,
        source: Value,
        keys: Vec<PropertyKey>,
    ) -> Result<GcRef<PendingGetOwnPropertyDescriptors>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_get_own_property_descriptors,
                0,
                PendingGetOwnPropertyDescriptors {
                    result,
                    source,
                    keys: keys.into_boxed_slice(),
                    index: 0,
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn get_own_property_descriptors_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingGetOwnPropertyDescriptors>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_get_own_property_descriptors)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    fn get_own_property_descriptors_snapshot(
        &mut self,
        state: GcRef<PendingGetOwnPropertyDescriptors>,
    ) -> Result<GetOwnPropertyDescriptorsSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_get_own_property_descriptors)
                    .map(|pending| GetOwnPropertyDescriptorsSnapshot {
                        result: pending.result,
                        source: pending.source,
                        key: pending.keys.get(pending.index).copied(),
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn advance_get_own_property_descriptors_index(
        &mut self,
        state: GcRef<PendingGetOwnPropertyDescriptors>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_get_own_property_descriptors)
                    .map_err(ExecutionError::NoGcBorrow)?;
                pending.index = pending.index.saturating_add(1);
                Ok(())
            })
        })
    }

    /// Implements Object.prototype.hasOwnProperty for the currently supported ordinary properties.
    pub(crate) fn object_has_own_property(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.dispatch_builtin_property_key(
            BuiltinPropertyKeyConsumer::HasOwnProperty,
            site,
            site.this_value,
            key,
            Value::from_immediate(Immediate::Undefined),
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Implements Object.prototype.propertyIsEnumerable for one ordinary own property.
    pub(crate) fn object_property_is_enumerable(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.dispatch_builtin_property_key(
            BuiltinPropertyKeyConsumer::PropertyIsEnumerable,
            site,
            site.this_value,
            key,
            Value::from_immediate(Immediate::Undefined),
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Starts the legacy accessor helper after validating its receiver and callable argument.
    pub(crate) fn object_define_legacy_accessor(
        &mut self,
        site: &CallSite,
        setter: bool,
    ) -> Result<(), ExecutionError> {
        let receiver = self.object_value_of(site.this_value)?;
        self.write(site.caller_base, site.destination, receiver)?;
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let callback = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.resolve_function_object(callback)?;
        self.dispatch_builtin_property_key(
            if setter {
                BuiltinPropertyKeyConsumer::DefineSetter
            } else {
                BuiltinPropertyKeyConsumer::DefineGetter
            },
            site,
            receiver,
            key,
            callback,
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Starts a legacy accessor lookup after ToObject and resumable key conversion.
    pub(crate) fn object_lookup_legacy_accessor(
        &mut self,
        site: &CallSite,
        setter: bool,
    ) -> Result<(), ExecutionError> {
        let receiver = self.object_value_of(site.this_value)?;
        self.write(site.caller_base, site.destination, receiver)?;
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.dispatch_builtin_property_key(
            if setter {
                BuiltinPropertyKeyConsumer::LookupSetter
            } else {
                BuiltinPropertyKeyConsumer::LookupGetter
            },
            site,
            receiver,
            key,
            Value::from_immediate(Immediate::Undefined),
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Implements the legacy __proto__ getter through the shared prototype internal method.
    pub(crate) fn object_proto_getter(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let receiver = self.object_value_of(site.this_value)?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.is_proxy_value(receiver) {
            self.dispatch_proxy_internal_method(
                native_site,
                receiver,
                ProxyInternalMethod::GetPrototypeOf,
            )?;
            return Ok(());
        }
        let prototype = self.object_snapshot(receiver)?.1.prototype;
        self.write(site.caller_base, site.destination, prototype)
    }

    /// Implements the legacy __proto__ setter with RequireObjectCoercible and false-throw rules.
    pub(crate) fn object_proto_setter(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        if matches!(
            site.this_value.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(site.this_value));
        }
        let prototype = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if prototype.as_immediate() != Some(Immediate::Null) && !self.is_object_value(prototype) {
            return self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::Undefined),
            );
        }
        if !self.is_object_value(site.this_value) {
            return self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::Undefined),
            );
        }
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.is_proxy_value(site.this_value) {
            self.dispatch_legacy_proxy_set_prototype(native_site, site.this_value, prototype)?;
            return Ok(());
        }
        if !self.ordinary_set_prototype_of(site.this_value, prototype)? {
            return Err(ExecutionError::NonExtensibleObject(site.this_value));
        }
        self.write(
            site.caller_base,
            site.destination,
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Walks ordinary prototypes synchronously and publishes state only at a Proxy boundary.
    pub(crate) fn begin_object_lookup_accessor(
        &mut self,
        site: NativeContinuationSite,
        mut object: Value,
        key: Value,
        setter: bool,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let key_identity = self.property_key(key)?;
        loop {
            if self.is_proxy_value(object) {
                return self.dispatch_object_lookup_get_own(site, object, key, setter);
            }
            if let Some(descriptor) = self.complete_own_property_descriptor(object, key_identity)? {
                let result = match descriptor {
                    PropertyDescriptor::Accessor(descriptor) if setter => descriptor.setter,
                    PropertyDescriptor::Accessor(descriptor) => descriptor.getter,
                    PropertyDescriptor::Data(_) | PropertyDescriptor::Generic(_) => None,
                }
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
                self.write(site.caller_base, site.destination, result)?;
                return Ok(None);
            }
            let prototype = self.object_snapshot(object)?.1.prototype;
            if prototype.as_immediate() == Some(Immediate::Null) {
                self.write(
                    site.caller_base,
                    site.destination,
                    Value::from_immediate(Immediate::Undefined),
                )?;
                return Ok(None);
            }
            object = prototype;
        }
    }

    /// Wraps one Proxy [[GetOwnProperty]] call with the lookup consumer continuation.
    fn dispatch_object_lookup_get_own(
        &mut self,
        site: NativeContinuationSite,
        object: Value,
        key: Value,
        setter: bool,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        self.push_object_lookup_accessor(
            site,
            ObjectLookupAccessorStage::GetOwn,
            setter,
            key,
            object,
        )?;
        let completion_depth = self.fiber.completions.len() - 1;
        let frame_depth = self.fiber.frames.len();
        let mode = if setter {
            ProxyGetOwnMode::LookupSetter
        } else {
            ProxyGetOwnMode::LookupGetter
        };
        let outcome = match self.dispatch_proxy_get_own(site, object, key, mode) {
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
            return Ok(outcome);
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_object_lookup_accessor(continuation, value)
    }

    /// Resumes either descriptor lookup or the following Proxy prototype lookup.
    pub(crate) fn resume_object_lookup_accessor(
        &mut self,
        continuation: NativeContinuation,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let NativeContinuationKind::ObjectLookupAccessor { stage, setter } = continuation.kind()
        else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        match stage {
            ObjectLookupAccessorStage::GetOwn if value.as_immediate() != Some(Immediate::Hole) => {
                self.write(
                    continuation.site().caller_base,
                    continuation.site().destination,
                    value,
                )?;
                Ok(None)
            }
            ObjectLookupAccessorStage::GetOwn => {
                self.dispatch_object_lookup_get_prototype(continuation, setter)
            }
            ObjectLookupAccessorStage::GetPrototype => {
                if value.as_immediate() == Some(Immediate::Null) {
                    self.write(
                        continuation.site().caller_base,
                        continuation.site().destination,
                        Value::from_immediate(Immediate::Undefined),
                    )?;
                    return Ok(None);
                }
                self.begin_object_lookup_accessor(
                    continuation.site(),
                    value,
                    continuation.first(),
                    setter,
                )
            }
        }
    }

    /// Obtains a Proxy prototype after its own descriptor was absent.
    fn dispatch_object_lookup_get_prototype(
        &mut self,
        continuation: NativeContinuation,
        setter: bool,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let site = continuation.site();
        let object = continuation.second();
        let key = continuation.first();
        self.push_object_lookup_accessor(
            site,
            ObjectLookupAccessorStage::GetPrototype,
            setter,
            key,
            object,
        )?;
        let completion_depth = self.fiber.completions.len() - 1;
        let frame_depth = self.fiber.frames.len();
        let outcome = match self.dispatch_proxy_internal_method(
            site,
            object,
            ProxyInternalMethod::GetPrototypeOf,
        ) {
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
            return Ok(outcome);
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_object_lookup_accessor(continuation, value)
    }

    #[inline]
    fn push_object_lookup_accessor(
        &mut self,
        site: NativeContinuationSite,
        stage: ObjectLookupAccessorStage,
        setter: bool,
        key: Value,
        object: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::object_lookup_accessor(
                site, stage, setter, key, object,
            ))
            .map_err(Self::completion_stack_error)
    }

    /// Implements the static Object.hasOwn nullish boundary and ordinary own-property query.
    pub(crate) fn object_has_own(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let object = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if matches!(
            object.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(object));
        }
        let object = self.object_value_of(object)?;
        let key = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.dispatch_builtin_property_key(
            BuiltinPropertyKeyConsumer::HasOwn,
            site,
            object,
            key,
            Value::from_immediate(Immediate::Undefined),
            Value::from_immediate(Immediate::Undefined),
        )
    }

    /// Implements Object.is with the VM's SameValue primitive.
    pub(crate) fn object_is(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let left = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let right = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.same_value(left, right)
    }

    /// Starts Object.prototype.toString, retaining its fallback across observable tag lookup.
    pub(crate) fn begin_object_to_string(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        if let Some(immediate) = site.this_value.as_immediate() {
            let direct = match immediate {
                Immediate::Undefined => Some(b"Undefined".as_slice()),
                Immediate::Null => Some(b"Null".as_slice()),
                _ => None,
            };
            if let Some(tag) = direct {
                let result = self.assemble_object_tag(None, tag)?;
                return self.write(site.caller_base, site.destination, result);
            }
        }
        let receiver = self.coerce_to_object(site.this_value)?;
        let builtin_tag = self.object_builtin_tag(receiver)?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        let continuation =
            NativeContinuation::object_to_string(native_site, receiver, builtin_tag as u8);
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        let symbol = self
            .realm
            .well_known_symbols
            .to_string_tag
            .expect("Symbol.toStringTag initializes before Object.prototype.toString");
        let key = self.property_key(symbol)?;
        if let Err(error) =
            self.dispatch_proxy_aware_property_read(native_site, receiver, receiver, key)
        {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            return Ok(());
        }
        if self.fiber.completions.len() <= completion_depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let tag = self.read(native_site.caller_base, native_site.destination)?;
        self.resume_object_to_string(continuation, tag)
    }

    /// Completes the tag lookup, accepting only primitive String overrides.
    pub(crate) fn resume_object_to_string(
        &mut self,
        continuation: NativeContinuation,
        tag: Value,
    ) -> Result<(), ExecutionError> {
        let raw = continuation
            .second()
            .as_i32()
            .and_then(|value| u8::try_from(value).ok())
            .and_then(ObjectBuiltinTag::from_raw)
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let result = if self.is_string_value(tag) {
            self.assemble_object_tag(Some(tag), &[])?
        } else {
            self.assemble_object_tag(None, raw.as_bytes())?
        };
        self.write(
            continuation.site().caller_base,
            continuation.site().destination,
            result,
        )
    }

    /// Computes the non-observable built-in fallback before reading `@@toStringTag`.
    fn object_builtin_tag(&mut self, value: Value) -> Result<ObjectBuiltinTag, ExecutionError> {
        if self.is_array_value(value)? {
            return Ok(ObjectBuiltinTag::Array);
        }
        if value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.arguments_object)
                .is_ok()
        }) {
            return Ok(ObjectBuiltinTag::Arguments);
        }
        if self.is_callable_value(value)? {
            return Ok(ObjectBuiltinTag::Function);
        }
        if self.native_error_kind(value)?.is_some() {
            return Ok(ObjectBuiltinTag::Error);
        }
        let Some(raw) = value.as_heap_ref() else {
            return Ok(ObjectBuiltinTag::Object);
        };
        if self
            .heap
            .checked_reference(raw, self.types.boolean_object)
            .is_ok()
        {
            return Ok(ObjectBuiltinTag::Boolean);
        }
        if self
            .heap
            .checked_reference(raw, self.types.number_object)
            .is_ok()
        {
            return Ok(ObjectBuiltinTag::Number);
        }
        if self
            .heap
            .checked_reference(raw, self.types.string_object)
            .is_ok()
        {
            return Ok(ObjectBuiltinTag::String);
        }
        if self
            .heap
            .checked_reference(raw, self.types.date_object)
            .is_ok()
        {
            return Ok(ObjectBuiltinTag::Date);
        }
        if self
            .heap
            .checked_reference(raw, self.types.regexp_object)
            .is_ok()
        {
            return Ok(ObjectBuiltinTag::RegExp);
        }
        Ok(ObjectBuiltinTag::Object)
    }

    /// Concatenates one UTF-16 tag without converting it through Rust UTF-8.
    fn assemble_object_tag(
        &mut self,
        string_tag: Option<Value>,
        fallback: &[u8],
    ) -> Result<Value, ExecutionError> {
        const PREFIX: &[u8] = b"[object ";
        let tag_length = string_tag
            .map(|value| self.string_value_length(value))
            .transpose()?
            .unwrap_or(fallback.len());
        let capacity = PREFIX
            .len()
            .checked_add(tag_length)
            .and_then(|length| length.checked_add(1))
            .ok_or(ExecutionError::StringBufferAllocationFailed)?;
        let mut units = Vec::new();
        units
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        units.extend(PREFIX.iter().map(|byte| u16::from(*byte)));
        if let Some(tag) = string_tag {
            self.append_primitive_string_units(tag, &mut units)?;
        } else {
            units.extend(fallback.iter().map(|byte| u16::from(*byte)));
        }
        units.push(u16::from(b']'));
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(units)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Materializes Object.keys/values/entries from ordinary enumerable data slots.
    pub(crate) fn object_enumeration(
        &mut self,
        site: &CallSite,
        native: NativeFunction,
    ) -> Result<Value, ExecutionError> {
        let source = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if matches!(
            source.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(source));
        }
        let result = self.create_array_from_site(&CallSite {
            argument_count: 0,
            ..*site
        })?;
        if let Some(raw) = source.as_heap_ref()
            && let Ok(string) = self.heap.checked_reference(raw, self.types.string)
        {
            return self.enumerate_string_primitive(result, string, native);
        }
        if !self.is_object_value(source) {
            return Ok(result);
        }
        let (_, snapshot) = self.object_snapshot(source)?;
        let mut keys = self.ordinary_own_property_keys(source, snapshot)?;
        let mut output_index = 0_i32;
        while let Some(entry) = keys.next_entry() {
            let key = entry.key;
            let Some(property) = entry.property else {
                continue;
            };
            if !property.attributes.enumerable()
                || !self.property_is_present_from_snapshot(snapshot, property)?
            {
                continue;
            }
            let Some(key_atom) = key.atom() else {
                continue;
            };
            if native == NativeFunction::ObjectKeys {
                let key_value = self.atom_string_value(key_atom)?;
                self.append_object_enumeration_item(result, output_index, key_value, native)?;
                output_index = output_index
                    .checked_add(1)
                    .ok_or(ExecutionError::RegisterWindowTooLarge(u32::MAX))?;
                continue;
            }
            let Some(value) = self.data_property_from_snapshot(snapshot, key)? else {
                continue;
            };
            match native {
                NativeFunction::ObjectEntries => {
                    self.append_object_entry(result, output_index, key_atom, value)?;
                }
                NativeFunction::ObjectValues => {
                    self.append_object_enumeration_item(result, output_index, value, native)?;
                }
                _ => return Err(ExecutionError::NonCallable(source)),
            }
            output_index = output_index
                .checked_add(1)
                .ok_or(ExecutionError::RegisterWindowTooLarge(u32::MAX))?;
        }
        let length = self.intern_intrinsic_name(b"length")?;
        self.set_own_data_property(result, length, Value::from_i32(output_index))?;
        Ok(result)
    }

    /// Materializes all present own string keys, including non-enumerable properties.
    pub(crate) fn object_get_own_property_names(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let source = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if matches!(
            source.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(source));
        }
        let result = self.create_array_from_site(&CallSite {
            argument_count: 0,
            ..*site
        })?;
        if !self.is_object_value(source) {
            return Ok(result);
        }
        let (_, snapshot) = self.object_snapshot(source)?;
        let keys = self.ordinary_own_property_keys(source, snapshot)?;
        let mut output_index = 0_u64;
        for key in keys {
            let Some(key) = key.atom() else {
                continue;
            };
            let name = self.atom_string_value(key)?;
            let output_key = self.safe_integer_property_atom(output_index)?;
            self.set_own_data_property(result, output_key, name)?;
            output_index = output_index
                .checked_add(1)
                .ok_or(ExecutionError::ArrayLengthOverflow)?;
        }
        let length = self.length_atom()?;
        self.set_own_data_property(result, length, safe_integer_value(output_index))?;
        Ok(result)
    }

    /// Materializes all present own Symbol keys in insertion order, excluding every string key.
    pub(crate) fn object_get_own_property_symbols(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let source = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if matches!(
            source.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(source));
        }
        let result = self.create_array_from_site(&CallSite {
            argument_count: 0,
            ..*site
        })?;
        if !self.is_object_value(source) {
            return Ok(result);
        }
        let (_, snapshot) = self.object_snapshot(source)?;
        let keys = self.ordinary_own_property_keys(source, snapshot)?;
        let mut output_index = 0_u64;
        for key in keys {
            let Some(symbol) = key.symbol() else {
                continue;
            };
            let output_key = self.safe_integer_property_atom(output_index)?;
            self.set_own_data_property(result, output_key, symbol.value())?;
            output_index = output_index
                .checked_add(1)
                .ok_or(ExecutionError::ArrayLengthOverflow)?;
        }
        let length = self.length_atom()?;
        self.set_own_data_property(result, length, safe_integer_value(output_index))?;
        Ok(result)
    }

    /// Returns Object.getPrototypeOf for the first call argument.
    pub(crate) fn object_get_prototype_of(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let object = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.object_prototype_of(object)
    }

    /// Returns the current ordinary prototype and applies the nullish TypeError boundary.
    pub(crate) fn object_prototype_of(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if matches!(
            value.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(value));
        }
        if self.is_object_value(value) {
            return self
                .object_snapshot(value)
                .map(|(_, object)| object.prototype);
        }
        Ok(self
            .realm
            .object_prototype
            .expect("Object prototype initializes before primitive boxing"))
    }

    /// Creates the object, then delegates its optional descriptor map to the shared state machine.
    pub(crate) fn begin_object_create(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let prototype = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if prototype.as_immediate() != Some(Immediate::Null) && !self.is_object_value(prototype) {
            return Err(ExecutionError::NotObject(prototype));
        }
        let object = self.create_ordinary_object_with_prototype(prototype)?;
        self.write(site.caller_base, site.destination, object)?;
        if let Some(descriptors) = self.call_argument(site, 1)?
            && descriptors.as_immediate() != Some(Immediate::Undefined)
        {
            return self.begin_define_properties_for_target(
                NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                object,
                descriptors,
            );
        }
        Ok(())
    }

    /// Begins Object.prototype.isPrototypeOf with its required argument/receiver ordering.
    pub(crate) fn begin_object_is_prototype_of(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let value = self.call_argument(site, 0)?;
        let Some(value) = value else {
            return self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::False),
            );
        };
        if !self.is_object_value(value) {
            return self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::False),
            );
        }
        let prototype = self.object_value_of(site.this_value)?;
        let native_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.continue_object_is_prototype_of(native_site, prototype, value)
            .map(|_| ())
    }

    /// Walks the prototype chain, routing Proxy targets through observable [[GetPrototypeOf]].
    fn continue_object_is_prototype_of(
        &mut self,
        site: NativeContinuationSite,
        prototype: Value,
        mut value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let next = if self.is_proxy_value(value) {
                return self.dispatch_object_is_prototype_of_proxy(site, prototype, value);
            } else {
                self.object_snapshot(value)?.1.prototype
            };
            if next == prototype {
                self.write(site.caller_base, site.destination, boolean_value(true))?;
                return Ok(None);
            }
            if next.as_immediate() == Some(Immediate::Null) {
                self.write(site.caller_base, site.destination, boolean_value(false))?;
                return Ok(None);
            }
            value = next;
        }
    }

    /// Publishes the receiver identity around one possibly suspended Proxy prototype lookup.
    fn dispatch_object_is_prototype_of_proxy(
        &mut self,
        site: NativeContinuationSite,
        prototype: Value,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(NativeContinuation::object_is_prototype_of(site, prototype))
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        let outcome = match self.dispatch_proxy_internal_method(
            site,
            value,
            ProxyInternalMethod::GetPrototypeOf,
        ) {
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
            return Ok(outcome);
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_object_is_prototype_of(continuation, value)
    }

    /// Resumes a Proxy [[GetPrototypeOf]] step and continues the chain without Rust recursion.
    pub(crate) fn resume_object_is_prototype_of(
        &mut self,
        continuation: NativeContinuation,
        value: Value,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        let prototype = continuation.first();
        if value.as_immediate() == Some(Immediate::Null) {
            self.write(
                continuation.site().caller_base,
                continuation.site().destination,
                Value::from_immediate(Immediate::False),
            )?;
            return Ok(None);
        }
        if value == prototype {
            self.write(
                continuation.site().caller_base,
                continuation.site().destination,
                Value::from_immediate(Immediate::True),
            )?;
            return Ok(None);
        }
        self.continue_object_is_prototype_of(continuation.site(), prototype, value)
    }

    /// Implements Object.isExtensible for ordinary object values.
    pub(crate) fn object_is_extensible(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.is_object_value(value) {
            Ok(self.object_snapshot(value)?.1.extensible)
        } else {
            Ok(false)
        }
    }

    /// Implements Object.preventExtensions and returns its original argument.
    pub(crate) fn object_prevent_extensions(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.is_object_value(value) {
            let (receiver, _) = self.object_snapshot(value)?;
            self.set_object_extensible(receiver, false)?;
        }
        Ok(value)
    }

    /// Enumerates the virtual indexed properties exposed by one primitive string.
    fn enumerate_string_primitive(
        &mut self,
        result: Value,
        string: GcRef<JsString>,
        native: NativeFunction,
    ) -> Result<Value, ExecutionError> {
        let units = self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let string = no_gc
                    .borrow(string, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok::<Vec<u16>, ExecutionError>(match string.as_view() {
                    JsStringView::Latin1(bytes) => {
                        bytes.iter().map(|&byte| u16::from(byte)).collect()
                    }
                    JsStringView::Utf16(units) => units.to_vec(),
                })
            })
        })?;
        let length = i32::try_from(units.len())
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(u32::MAX))?;
        for (index, unit) in units.into_iter().enumerate() {
            let index = i32::try_from(index)
                .map_err(|_| ExecutionError::RegisterWindowTooLarge(u32::MAX))?;
            let key_atom = self.property_key_atom(Value::from_i32(index))?;
            match native {
                NativeFunction::ObjectEntries => {
                    let pair = self.create_and_root_entry_pair(result, index)?;
                    let zero = self.property_key_atom(Value::from_i32(0))?;
                    let key = self.atom_string_value(key_atom)?;
                    self.set_own_data_property(pair, zero, key)?;
                    let one = self.property_key_atom(Value::from_i32(1))?;
                    let value = self.allocate_runtime_string(
                        JsString::try_from_utf16(&[unit])
                            .map_err(ExecutionError::PropertyKeyString)?,
                    )?;
                    self.set_own_data_property(pair, one, value)?;
                    let pair_length = self.intern_intrinsic_name(b"length")?;
                    self.set_own_data_property(pair, pair_length, Value::from_i32(2))?;
                }
                NativeFunction::ObjectKeys => {
                    let key = self.atom_string_value(key_atom)?;
                    self.append_object_enumeration_item(result, index, key, native)?;
                }
                NativeFunction::ObjectValues => {
                    let value = self.allocate_runtime_string(
                        JsString::try_from_utf16(&[unit])
                            .map_err(ExecutionError::PropertyKeyString)?,
                    )?;
                    self.append_object_enumeration_item(result, index, value, native)?;
                }
                _ => return Err(ExecutionError::NonCallable(result)),
            }
        }
        let length_atom = self.intern_intrinsic_name(b"length")?;
        self.set_own_data_property(result, length_atom, Value::from_i32(length))?;
        Ok(result)
    }

    /// Appends one materialized key/value item without duplicating result indexing.
    fn append_object_enumeration_item(
        &mut self,
        result: Value,
        output_index: i32,
        item: Value,
        native: NativeFunction,
    ) -> Result<(), ExecutionError> {
        debug_assert!(matches!(
            native,
            NativeFunction::ObjectKeys | NativeFunction::ObjectValues
        ));
        let result_key = self.property_key_atom(Value::from_i32(output_index))?;
        self.set_own_data_property(result, result_key, item)
    }

    /// Creates and roots an Object.entries pair before allocating its key string.
    fn create_and_root_entry_pair(
        &mut self,
        result: Value,
        output_index: i32,
    ) -> Result<Value, ExecutionError> {
        let pair = self.create_unrooted_array()?;
        let pair_key = self.property_key_atom(Value::from_i32(output_index))?;
        self.set_own_data_property(result, pair_key, pair)?;
        Ok(pair)
    }

    /// Appends one Object.entries pair whose source value remains rooted by the source object.
    fn append_object_entry(
        &mut self,
        result: Value,
        output_index: i32,
        key: AtomId,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pair = self.create_and_root_entry_pair(result, output_index)?;
        let zero = self.property_key_atom(Value::from_i32(0))?;
        let key = self.atom_string_value(key)?;
        self.set_own_data_property(pair, zero, key)?;
        let one = self.property_key_atom(Value::from_i32(1))?;
        self.set_own_data_property(pair, one, value)?;
        let length = self.intern_intrinsic_name(b"length")?;
        self.set_own_data_property(pair, length, Value::from_i32(2))
    }

    /// Allocates an empty Array value whose caller immediately publishes it into a rooted owner.
    fn create_unrooted_array(&mut self) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before Object.entries");
        self.create_array_object_with_prototype(prototype)
    }

    /// Copies an immortal atom spelling into one GC-managed ECMAScript string value.
    pub(crate) fn atom_string_value(&mut self, atom: AtomId) -> Result<Value, ExecutionError> {
        let string = self
            .atoms
            .get(atom)
            .expect("shape keys always reference live isolate atoms");
        let string = match string.as_view() {
            JsStringView::Latin1(bytes) => JsString::try_from_latin1(bytes),
            JsStringView::Utf16(units) => JsString::try_from_utf16(units),
        }
        .map_err(ExecutionError::PropertyKeyString)?;
        self.allocate_runtime_string(string)
    }
}
