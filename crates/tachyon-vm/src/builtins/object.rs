//! Object constructor, static method, and prototype slow paths.

use super::super::*;

impl Isolate {
    /// Implements the ordinary Object constructor for object values and primitive fallback values.
    pub(crate) fn create_object_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        if let Some(value) = self.call_argument(site, 0)?
            && self.is_object_value(value)
        {
            return Ok(value);
        }
        let object = self.create_ordinary_object()?;
        self.write(site.caller_base, site.destination, object)?;
        Ok(object)
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
        let key = self.property_key(key)?;
        self.begin_property_descriptor(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            object,
            key,
            descriptor,
        )
    }

    /// Materializes one own data or accessor descriptor, or undefined when the key is absent.
    pub(crate) fn object_get_own_property_descriptor(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let object = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if matches!(
            object.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(object));
        }
        let key = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self.property_key(key)?;
        let property = if self.is_object_value(object) {
            self.complete_own_property_descriptor(object, key)?
        } else {
            None
        };
        let Some(descriptor) = property else {
            return Ok(Value::from_immediate(Immediate::Undefined));
        };

        let result = self.create_ordinary_object()?;
        self.write(site.caller_base, site.destination, result)?;
        self.materialize_property_descriptor(result, descriptor)?;
        Ok(result)
    }

    /// Implements Object.prototype.hasOwnProperty for the currently supported ordinary properties.
    pub(crate) fn object_has_own_property(
        &mut self,
        site: &CallSite,
    ) -> Result<bool, ExecutionError> {
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self.property_key(key)?;
        self.has_own_property(site.this_value, key)
    }

    /// Implements Object.prototype.propertyIsEnumerable for one ordinary own property.
    pub(crate) fn object_property_is_enumerable(
        &mut self,
        site: &CallSite,
    ) -> Result<bool, ExecutionError> {
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self.property_key(key)?;
        Ok(self
            .complete_own_property_descriptor(site.this_value, key)?
            .is_some_and(|descriptor| descriptor.enumerable().unwrap_or(false)))
    }

    /// Implements the static Object.hasOwn nullish boundary and ordinary own-property query.
    pub(crate) fn object_has_own(&mut self, site: &CallSite) -> Result<bool, ExecutionError> {
        let object = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if matches!(
            object.as_immediate(),
            Some(Immediate::Undefined | Immediate::Null)
        ) {
            return Err(ExecutionError::NotObject(object));
        }
        let key = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self.property_key(key)?;
        self.has_own_property(object, key)
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

    /// Implements the ordinary tag-producing subset of Object.prototype.toString.
    pub(crate) fn object_to_string(&mut self, value: Value) -> Result<Value, ExecutionError> {
        let tag = if let Some(immediate) = value.as_immediate() {
            match immediate {
                Immediate::Undefined => "[object Undefined]",
                Immediate::Null => "[object Null]",
                Immediate::True | Immediate::False => "[object Boolean]",
                Immediate::Hole | Immediate::Uninitialized => "[object Object]",
            }
        } else if value.as_i32().is_some()
            || value.as_f64().is_some()
            || value.as_heap_ref().is_some_and(|raw| {
                self.heap
                    .checked_reference(raw, self.types.number_object)
                    .is_ok()
            })
        {
            "[object Number]"
        } else if let Some(raw) = value.as_heap_ref()
            && self.heap.checked_reference(raw, self.types.string).is_ok()
        {
            "[object String]"
        } else if let Some(raw) = value.as_heap_ref()
            && self
                .heap
                .checked_reference(raw, self.types.function)
                .is_ok()
        {
            "[object Function]"
        } else if self.is_array_value(value)? {
            "[object Array]"
        } else {
            "[object Object]"
        };
        self.allocate_runtime_string(
            JsString::try_from_latin1(tag.as_bytes()).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Copies enumerable ordinary data slots in stable shape insertion order.
    fn copy_own_data_properties(
        &mut self,
        target: Value,
        source: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(source) {
            return Ok(());
        }
        let (_, snapshot) = self.object_snapshot(source)?;
        let keys = self
            .shapes
            .own_keys(snapshot.shape)
            .map_err(ExecutionError::Shape)?;
        for key in keys {
            if !self
                .shapes
                .lookup(snapshot.shape, key)
                .expect("own key resolves in its source shape")
                .attributes
                .enumerable()
            {
                continue;
            }
            if let Some(value) = self.data_property_from_snapshot(snapshot, key)? {
                self.set_own_data_property(target, key, value)?;
            }
        }
        Ok(())
    }

    /// Implements Object.assign for ordinary data-property sources and one target object.
    pub(crate) fn object_assign(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let target = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let target = if self.is_object_value(target) {
            target
        } else {
            self.create_ordinary_object()?
        };
        for index in 1..site.argument_count {
            let source = self
                .call_argument(site, index)?
                .unwrap_or(Value::from_immediate(Immediate::Undefined));
            self.copy_own_data_properties(target, source)?;
        }
        Ok(target)
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
        let keys = self
            .shapes
            .own_keys(snapshot.shape)
            .map_err(ExecutionError::Shape)?;
        let mut output_index = 0_i32;
        for key in keys {
            let property = self
                .shapes
                .lookup(snapshot.shape, key)
                .expect("own key resolves in its source shape");
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
        let keys = self
            .shapes
            .own_keys(snapshot.shape)
            .map_err(ExecutionError::Shape)?;
        let mut output_index = 0_u64;
        for key in keys {
            let property = self
                .shapes
                .lookup(snapshot.shape, key)
                .expect("own key resolves in its source shape");
            if !self.property_is_present_from_snapshot(snapshot, property)? {
                continue;
            }
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
        if self.resolve_function_object(source).is_ok() {
            for key in [self.length_atom()?, self.name_atom()?] {
                if self.shapes.lookup(snapshot.shape, key).is_some() {
                    continue;
                }
                let name = self.atom_string_value(key)?;
                let output_key = self.safe_integer_property_atom(output_index)?;
                self.set_own_data_property(result, output_key, name)?;
                output_index = output_index
                    .checked_add(1)
                    .ok_or(ExecutionError::ArrayLengthOverflow)?;
            }
            let prototype = self.prototype_atom()?;
            if self.is_function_prototype_property(source, prototype)
                && self.shapes.lookup(snapshot.shape, prototype).is_none()
            {
                let name = self.atom_string_value(prototype)?;
                let output_key = self.safe_integer_property_atom(output_index)?;
                self.set_own_data_property(result, output_key, name)?;
                output_index = output_index
                    .checked_add(1)
                    .ok_or(ExecutionError::ArrayLengthOverflow)?;
            }
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

    /// Implements Object.create and publishes the new object before processing descriptors.
    pub(crate) fn object_create(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
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
            self.define_ordinary_properties(object, descriptors)?;
        }
        Ok(object)
    }

    /// Applies ordinary data descriptors from an Object.create descriptor map.
    fn define_ordinary_properties(
        &mut self,
        target: Value,
        descriptors: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(descriptors) {
            return Err(ExecutionError::NotObject(descriptors));
        }
        let (_, snapshot) = self.object_snapshot(descriptors)?;
        let keys = self
            .shapes
            .own_keys(snapshot.shape)
            .map_err(ExecutionError::Shape)?;
        for key in keys {
            let Some(descriptor) = self.data_property_from_snapshot(snapshot, key)? else {
                continue;
            };
            if !self.is_object_value(descriptor) {
                return Err(ExecutionError::NotObject(descriptor));
            }
            let descriptor = self.parse_data_property_descriptor(descriptor)?;
            self.define_data_property(target, key, descriptor)?;
        }
        Ok(())
    }

    /// Implements Object.prototype.isPrototypeOf for its receiver and first argument.
    pub(crate) fn object_is_prototype_of(
        &mut self,
        site: &CallSite,
    ) -> Result<bool, ExecutionError> {
        let value = self.call_argument(site, 0)?;
        self.is_prototype_of(site.this_value, value)
    }

    /// Walks one ordinary prototype chain without invoking user code or allocating.
    pub(crate) fn is_prototype_of(
        &mut self,
        prototype: Value,
        value: Option<Value>,
    ) -> Result<bool, ExecutionError> {
        let Some(value) = value else {
            return Ok(false);
        };
        if !self.is_object_value(prototype) || !self.is_object_value(value) {
            return Ok(false);
        }
        let (_, mut snapshot) = self.object_snapshot(value)?;
        loop {
            if snapshot.prototype == prototype {
                return Ok(true);
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return Ok(false);
            }
            let (_, next) = self.object_snapshot(snapshot.prototype)?;
            snapshot = next;
        }
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
