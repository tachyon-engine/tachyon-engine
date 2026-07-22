//! Function virtual properties, lazy prototypes, and ordinary instanceof.

use super::super::*;

impl Isolate {
    /// Materializes an accessor function name after the computed key has become a PropertyKey.
    pub(crate) fn set_accessor_function_name(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        is_getter: bool,
    ) -> Result<(), ExecutionError> {
        self.set_computed_function_name(receiver, key, Some(is_getter))
    }

    /// Materializes an ordinary method name after its computed key becomes a PropertyKey.
    pub(crate) fn set_method_function_name(
        &mut self,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        self.set_computed_function_name(receiver, key, None)
    }

    /// Builds the exact string or `[description]` name with an optional accessor prefix.
    fn set_computed_function_name(
        &mut self,
        receiver: Value,
        key: PropertyKey,
        accessor: Option<bool>,
    ) -> Result<(), ExecutionError> {
        const GET_PREFIX: &[u8] = b"get ";
        const SET_PREFIX: &[u8] = b"set ";
        let prefix = match accessor {
            Some(true) => GET_PREFIX,
            Some(false) => SET_PREFIX,
            None => &[],
        };
        let function = self
            .resolve_function_object(receiver)
            .map_err(|_| ExecutionError::NonCallable(receiver))?;
        if !matches!(
            function.executable,
            FunctionExecutable::Bytecode { .. } | FunctionExecutable::ClassBytecode(_)
        ) {
            return Ok(());
        }

        let (key_length, symbol_description) = match key {
            PropertyKey::Atom(atom) => (
                self.atoms
                    .get(atom)
                    .ok_or(ExecutionError::InvalidAtom(atom))?
                    .len(),
                None,
            ),
            PropertyKey::Symbol(symbol) => {
                let description = self.symbol_description(symbol)?;
                let description_length = description
                    .map(|value| self.string_value_length(value))
                    .transpose()?
                    .unwrap_or(0);
                (
                    description_length
                        .checked_add(2)
                        .ok_or(ExecutionError::StringBufferAllocationFailed)?,
                    description,
                )
            }
            PropertyKey::Private(_) => {
                return Err(ExecutionError::PrivatePropertyKeyEscaped);
            }
        };
        let capacity = prefix
            .len()
            .checked_add(key_length)
            .ok_or(ExecutionError::StringBufferAllocationFailed)?;
        let mut units = Vec::new();
        units
            .try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        units.extend(prefix.iter().map(|&unit| u16::from(unit)));
        match key {
            PropertyKey::Atom(atom) => {
                let text = self
                    .atoms
                    .get(atom)
                    .ok_or(ExecutionError::InvalidAtom(atom))?;
                for index in 0..text.len() {
                    units.push(
                        text.as_view()
                            .code_unit_at(index)
                            .expect("index is bounded"),
                    );
                }
            }
            PropertyKey::Symbol(_) => {
                units.push(u16::from(b'['));
                if let Some(description) = symbol_description {
                    self.append_primitive_string_units(description, &mut units)?;
                }
                units.push(u16::from(b']'));
            }
            PropertyKey::Private(_) => {
                return Err(ExecutionError::PrivatePropertyKeyEscaped);
            }
        }
        debug_assert_eq!(units.len(), capacity);
        let name = JsString::try_from_owned_code_units(units)
            .map_err(ExecutionError::PropertyKeyString)?;
        let name = self.allocate_runtime_string(name)?;
        let name_key = self.name_atom()?;
        self.define_fresh_data_property(
            receiver,
            name_key,
            name,
            PropertyAttributes::data(false, false, true),
        )
    }

    /// Reads a symbol's optional string description while the symbol remains rooted by its caller.
    fn symbol_description(&mut self, symbol: SymbolId) -> Result<Option<Value>, ExecutionError> {
        let symbol = self
            .heap
            .checked_reference(symbol.reference(), self.types.symbol)
            .map_err(ExecutionError::HeapReference)?;
        self.heap.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow_reference(symbol, self.types.symbol)
                .map(|symbol| symbol.description)
                .map_err(ExecutionError::NoGcBorrow)
        })
    }

    /// Exposes callable metadata as non-enumerable own virtual data properties.
    pub(super) fn function_metadata_property(
        &mut self,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<Value>, ExecutionError> {
        let Some(key) = key.atom() else {
            return Ok(None);
        };
        let Ok(function) = self.resolve_function_object(receiver) else {
            return Ok(None);
        };
        match function.executable {
            FunctionExecutable::Bound(data) => {
                let metadata = self.bound_function_snapshot(data)?;
                if key == self.length_atom()? {
                    return Ok(Some(metadata.length));
                }
                if key == self.name_atom()? {
                    return Ok(Some(metadata.name));
                }
                Ok(None)
            }
            FunctionExecutable::Native(native) => {
                if key == self.length_atom()? {
                    return Ok(Some(Value::from_i32(native.length())));
                }
                if key != self.name_atom()? {
                    return Ok(None);
                }
                let name = JsString::try_from_latin1(native.name().as_bytes())
                    .map_err(ExecutionError::PropertyKeyString)?;
                self.allocate_runtime_string(name).map(Some)
            }
            FunctionExecutable::ProxyRevoker(_) => {
                if key == self.length_atom()? {
                    return Ok(Some(Value::from_i32(0)));
                }
                if key != self.name_atom()? {
                    return Ok(None);
                }
                let name =
                    JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?;
                self.allocate_runtime_string(name).map(Some)
            }
            FunctionExecutable::PromiseResolver { .. } => {
                if key == self.length_atom()? {
                    return Ok(Some(Value::from_i32(1)));
                }
                if key != self.name_atom()? {
                    return Ok(None);
                }
                let name =
                    JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?;
                self.allocate_runtime_string(name).map(Some)
            }
            FunctionExecutable::PromiseCapabilityExecutor(_) => {
                if key == self.length_atom()? {
                    return Ok(Some(Value::from_i32(2)));
                }
                if key != self.name_atom()? {
                    return Ok(None);
                }
                let name =
                    JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?;
                self.allocate_runtime_string(name).map(Some)
            }
            FunctionExecutable::Bytecode { code, function, .. } => {
                self.bytecode_metadata_property(code, function, key)
            }
            FunctionExecutable::ClassBytecode(data) => {
                let data = self.class_constructor_snapshot(data)?;
                self.bytecode_metadata_property(data.code, data.function, key)
            }
        }
    }

    /// Reads immutable length/name metadata shared by ordinary and rare class bytecode payloads.
    fn bytecode_metadata_property(
        &mut self,
        code: CodeId,
        function: FunctionId,
        key: AtomId,
    ) -> Result<Option<Value>, ExecutionError> {
        let is_length = key == self.length_atom()?;
        let is_name = !is_length && key == self.name_atom()?;
        if !is_length && !is_name {
            return Ok(None);
        }
        let template = self
            .loaded_code(code)?
            .module
            .function(function)
            .ok_or(ExecutionError::MissingEntryFunction(function))?;
        if is_length {
            return Ok(Some(Value::from_i32(
                i32::try_from(template.layout().function_length).unwrap_or(i32::MAX),
            )));
        }
        let name = template
            .layout()
            .name_scope
            .and_then(|scope| {
                self.loaded_code(code)
                    .ok()?
                    .module
                    .scope_names()
                    .get(scope as usize)
            })
            .map_or("", AsRef::as_ref);
        let name = JsString::try_from_str(name).map_err(ExecutionError::PropertyKeyString)?;
        self.allocate_runtime_string(name).map(Some)
    }

    /// Publishes an inferred name only for an anonymous bytecode function created by the compiler.
    pub(crate) fn set_inferred_function_name(
        &mut self,
        receiver: Value,
        name: AtomId,
    ) -> Result<(), ExecutionError> {
        let function = self
            .resolve_function_object(receiver)
            .map_err(|_| ExecutionError::NonCallable(receiver))?;
        if !matches!(
            function.executable,
            FunctionExecutable::Bytecode { .. } | FunctionExecutable::ClassBytecode(_)
        ) {
            return Ok(());
        }
        let text = self
            .atoms
            .get(name)
            .ok_or(ExecutionError::InvalidAtom(name))?;
        let text = match text.as_view() {
            JsStringView::Latin1(bytes) => JsString::try_from_latin1(bytes),
            JsStringView::Utf16(units) => JsString::try_from_utf16(units),
        }
        .map_err(ExecutionError::PropertyKeyString)?;
        let value = self.allocate_runtime_string(text)?;
        let name_key = self.name_atom()?;
        self.define_fresh_data_property(
            receiver,
            name_key,
            value,
            PropertyAttributes::data(false, false, true),
        )
    }

    /// Tests the virtual metadata key without materializing a runtime name string.
    pub(super) fn is_function_metadata_property(
        &mut self,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<bool, ExecutionError> {
        let Some(key) = key.atom() else {
            return Ok(false);
        };
        if self.resolve_function_object(receiver).is_err() {
            return Ok(false);
        }
        Ok(key == self.length_atom()? || key == self.name_atom()?)
    }

    #[inline(always)]
    pub(crate) fn is_function_prototype_property(
        &mut self,
        receiver: Value,
        key: impl Into<PropertyKey>,
    ) -> bool {
        let Some(key) = key.into().atom() else {
            return false;
        };
        let is_prototype_name = self.intrinsic_property_atoms.prototype == Some(key)
            || self
                .atoms
                .get(key)
                .is_some_and(|name| name.equals_latin1(b"prototype"));
        if !is_prototype_name {
            return false;
        }
        self.resolve_function_object(receiver)
            .is_ok_and(|function| match function.executable {
                FunctionExecutable::Bytecode { code, function, .. } => self
                    .loaded_code(code)
                    .ok()
                    .and_then(|code| code.module.function(function))
                    .is_some_and(|metadata| {
                        !matches!(
                            metadata.kind(),
                            FunctionKind::ClassMethod | FunctionKind::ClassFieldInitializer
                        )
                    }),
                FunctionExecutable::Native(native) => native.has_default_prototype(),
                FunctionExecutable::ClassBytecode(_) => true,
                FunctionExecutable::Bound(_)
                | FunctionExecutable::ProxyRevoker(_)
                | FunctionExecutable::PromiseResolver { .. }
                | FunctionExecutable::PromiseCapabilityExecutor(_) => false,
            })
    }

    /// Classifies the immutable public prototype slot without adding flags to every function.
    pub(crate) fn has_read_only_prototype(
        &mut self,
        receiver: Value,
    ) -> Result<bool, ExecutionError> {
        let function = self.resolve_function_object(receiver)?;
        let (code, function) = match function.executable {
            FunctionExecutable::Bytecode { code, function, .. } => (code, function),
            FunctionExecutable::ClassBytecode(data) => {
                let data = self.class_constructor_snapshot(data)?;
                (data.code, data.function)
            }
            FunctionExecutable::Native(NativeFunction::ErrorConstructor(_)) => return Ok(true),
            FunctionExecutable::Native(NativeFunction::BooleanConstructor) => return Ok(true),
            _ => return Ok(false),
        };
        let kind = self
            .loaded_code(code)?
            .module
            .function(function)
            .ok_or(ExecutionError::MissingEntryFunction(function))?
            .kind();
        Ok(matches!(
            kind,
            FunctionKind::DerivedClassConstructor | FunctionKind::BaseClassConstructor
        ))
    }

    /// Materializes the spec-visible function prototype only on first observation or construction.
    pub(super) fn ensure_function_prototype(
        &mut self,
        function: Value,
    ) -> Result<Value, ExecutionError> {
        let raw = function
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(function))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NonCallable(function))?;
        let existing = self.heap.with_running_scope(|scope| {
            let function = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(function, self.types.function)
                    .map(|function| function.prototype_or_home_object)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        if let Some(prototype) = existing {
            return Ok(prototype);
        }
        self.materialize_function_prototype(function)
    }

    /// Allocates a one-slot constructor object, then publishes the lazy function edge with a barrier.
    fn materialize_function_prototype(&mut self, function: Value) -> Result<Value, ExecutionError> {
        let constructor_atom = self.constructor_atom()?;
        let object_prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before function prototype");
        let shape = self
            .shapes
            .transition_add(
                ShapeId::EMPTY,
                constructor_atom,
                PropertyAttributes::DEFAULT_DATA,
            )
            .map_err(ExecutionError::Shape)?;
        let mut roots = PrototypeInitializationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            function,
            object_prototype,
        };
        let storage = self
            .heap
            .try_allocate_external_with_gc(
                self.types.property_storage,
                0,
                PropertyStorage::new(Box::new([roots.function])),
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let prototype = self
            .heap
            .try_allocate_with_gc(
                self.types.ordinary_object,
                0,
                0,
                OrdinaryObject {
                    shape,
                    extensible: true,
                    storage: Some(storage),
                    prototype: roots.object_prototype,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let function = roots.function;
        self.set_function_prototype(function, Value::from_heap_ref(prototype.raw()))?;
        Ok(Value::from_heap_ref(prototype.raw()))
    }

    /// Replaces the inline function prototype slot and records its possible young edge.
    pub(crate) fn set_function_prototype(
        &mut self,
        function: Value,
        prototype: Value,
    ) -> Result<(), ExecutionError> {
        let raw = function
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(function))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NonCallable(function))?;
        self.heap.with_running_scope(|scope| {
            let function = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let object = no_gc
                    .borrow_mut(function, self.types.function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                object.prototype_or_home_object = Some(prototype);
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(function, prototype)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    /// Publishes `[[HomeObject]]` into the class-method-only half of the auxiliary function slot.
    pub(crate) fn set_function_home_object(
        &mut self,
        function: Value,
        home_object: Value,
    ) -> Result<(), ExecutionError> {
        let object = self.resolve_function_object(function)?;
        let FunctionExecutable::Bytecode {
            code,
            function: function_id,
            ..
        } = object.executable
        else {
            return Err(ExecutionError::NonCallable(function));
        };
        let kind = self
            .loaded_code(code)?
            .module
            .function(function_id)
            .ok_or(ExecutionError::MissingEntryFunction(function_id))?
            .kind();
        if !matches!(
            kind,
            FunctionKind::ClassMethod | FunctionKind::ClassFieldInitializer
        ) {
            return Err(ExecutionError::NonCallable(function));
        }
        self.set_function_prototype(function, home_object)
    }

    /// Reads one class method's `[[HomeObject]]` without materializing a public prototype.
    pub(crate) fn function_home_object(
        &mut self,
        function: Value,
    ) -> Result<Value, ExecutionError> {
        let raw = function
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(function))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NonCallable(function))?;
        let value = self.heap.with_running_scope(|scope| {
            let function = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(function, self.types.function)
                    .map(|function| function.prototype_or_home_object)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        value.ok_or(ExecutionError::UninitializedThis)
    }

    /// Replaces one callable's ordinary `[[Prototype]]` edge and publishes the GC barrier.
    pub(crate) fn set_function_internal_prototype(
        &mut self,
        function: Value,
        prototype: Value,
    ) -> Result<(), ExecutionError> {
        let raw = function
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(function))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NonCallable(function))?;
        self.heap.with_running_scope(|scope| {
            let function = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let object = no_gc
                    .borrow_mut(function, self.types.function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                object.ordinary.prototype = prototype;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(function, prototype)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    /// Implements ordinary HasInstance over the current constructor prototype and object chain.
    pub(crate) fn ordinary_instance_of(
        &mut self,
        value: Value,
        mut constructor: Value,
    ) -> Result<bool, ExecutionError> {
        loop {
            let function = self.resolve_function_object(constructor)?;
            let FunctionExecutable::Bound(data) = function.executable else {
                break;
            };
            constructor = self.bound_function_snapshot(data)?.call_target;
        }
        let prototype_atom = self.prototype_atom()?;
        let prototype = self.get_data_property(constructor, prototype_atom)?.ok_or(
            ExecutionError::InvalidInstanceofPrototype(Value::from_immediate(Immediate::Undefined)),
        )?;
        if !self.is_object_value(prototype) {
            return Err(ExecutionError::InvalidInstanceofPrototype(prototype));
        }
        if !self.is_object_value(value) {
            return Ok(false);
        }
        let (_, mut snapshot) = self.object_snapshot(value)?;
        loop {
            let candidate = snapshot.prototype;
            if candidate.as_immediate() == Some(Immediate::Null) {
                return Ok(false);
            }
            if candidate == prototype {
                return Ok(true);
            }
            let (_, next) = self.object_snapshot(candidate)?;
            snapshot = next;
        }
    }
}
