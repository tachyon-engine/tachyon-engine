//! Realm intrinsic construction and publication.

use super::*;

impl Isolate {
    /// Builds the object/function prototype graph and intrinsic constructors before publication.
    pub(super) fn initialize_realm_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let object_prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: Value::from_immediate(Immediate::Null),
        })?;
        self.realm.object_prototype = Some(object_prototype);
        let global_object = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: object_prototype,
        })?;
        self.realm.global_object = Some(global_object);
        self.initialize_function_intrinsics()?;
        self.initialize_object_intrinsics()?;
        self.initialize_primitive_constructors()?;
        let atoms = self.intern_realm_intrinsic_atoms()?;
        self.initialize_error_intrinsics()?;
        self.initialize_array_intrinsics()?;
        self.initialize_math_intrinsics()?;
        self.publish_realm_intrinsic_bindings(atoms)
    }

    /// Builds Object constructor, Object.prototype, and the basic own-property native methods.
    fn initialize_object_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let function_prototype = self
            .realm
            .function_prototype
            .expect("function intrinsics initialize before Object constructor");
        let object_prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before Object constructor");
        let constructor = self.allocate_native_function(
            NativeFunction::ObjectConstructor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_constructor = Some(constructor);
        self.set_function_prototype(constructor, object_prototype)?;
        let constructor_atom = self.constructor_atom()?;
        self.set_intrinsic_data_property(object_prototype, constructor_atom, constructor, true)?;
        let define_property = self.allocate_native_function(
            NativeFunction::ObjectDefineProperty,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_define_property = Some(define_property);
        let define_atom = self.intern_intrinsic_name(b"defineProperty")?;
        self.set_intrinsic_data_property(constructor, define_atom, define_property, true)?;
        let get_own_property_descriptor = self.allocate_native_function(
            NativeFunction::ObjectGetOwnPropertyDescriptor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_get_own_property_descriptor = Some(get_own_property_descriptor);
        let get_own_descriptor_atom = self.intern_intrinsic_name(b"getOwnPropertyDescriptor")?;
        self.set_intrinsic_data_property(
            constructor,
            get_own_descriptor_atom,
            get_own_property_descriptor,
            true,
        )?;
        let get_own_property_names = self.allocate_native_function(
            NativeFunction::ObjectGetOwnPropertyNames,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_get_own_property_names = Some(get_own_property_names);
        let get_own_names_atom = self.intern_intrinsic_name(b"getOwnPropertyNames")?;
        self.set_intrinsic_data_property(
            constructor,
            get_own_names_atom,
            get_own_property_names,
            true,
        )?;
        let has_own_property = self.allocate_native_function(
            NativeFunction::ObjectHasOwnProperty,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_has_own_property = Some(has_own_property);
        let has_own_atom = self.intern_intrinsic_name(b"hasOwnProperty")?;
        self.set_intrinsic_data_property(object_prototype, has_own_atom, has_own_property, true)?;
        let property_is_enumerable = self.allocate_native_function(
            NativeFunction::ObjectPropertyIsEnumerable,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_property_is_enumerable = Some(property_is_enumerable);
        let property_is_enumerable_atom = self.intern_intrinsic_name(b"propertyIsEnumerable")?;
        self.set_intrinsic_data_property(
            object_prototype,
            property_is_enumerable_atom,
            property_is_enumerable,
            true,
        )?;
        let to_string = self.allocate_native_function(
            NativeFunction::ObjectToString,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_to_string = Some(to_string);
        let to_string_atom = self.intern_intrinsic_name(b"toString")?;
        self.set_intrinsic_data_property(object_prototype, to_string_atom, to_string, true)?;
        let assign = self.allocate_native_function(
            NativeFunction::ObjectAssign,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_assign = Some(assign);
        let assign_atom = self.intern_intrinsic_name(b"assign")?;
        self.set_intrinsic_data_property(constructor, assign_atom, assign, true)?;
        let keys = self.allocate_native_function(
            NativeFunction::ObjectKeys,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_keys = Some(keys);
        let keys_atom = self.intern_intrinsic_name(b"keys")?;
        self.set_intrinsic_data_property(constructor, keys_atom, keys, true)?;
        let values = self.allocate_native_function(
            NativeFunction::ObjectValues,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_values = Some(values);
        let values_atom = self.intern_intrinsic_name(b"values")?;
        self.set_intrinsic_data_property(constructor, values_atom, values, true)?;
        let entries = self.allocate_native_function(
            NativeFunction::ObjectEntries,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_entries = Some(entries);
        let entries_atom = self.intern_intrinsic_name(b"entries")?;
        self.set_intrinsic_data_property(constructor, entries_atom, entries, true)?;
        let has_own = self.allocate_native_function(
            NativeFunction::ObjectHasOwn,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_has_own = Some(has_own);
        let has_own_atom = self.intern_intrinsic_name(b"hasOwn")?;
        self.set_intrinsic_data_property(constructor, has_own_atom, has_own, true)?;
        let object_is = self.allocate_native_function(
            NativeFunction::ObjectIs,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_is = Some(object_is);
        let is_atom = self.intern_intrinsic_name(b"is")?;
        self.set_intrinsic_data_property(constructor, is_atom, object_is, true)?;
        let get_prototype_of = self.allocate_native_function(
            NativeFunction::ObjectGetPrototypeOf,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_get_prototype_of = Some(get_prototype_of);
        let get_prototype_atom = self.intern_intrinsic_name(b"getPrototypeOf")?;
        self.set_intrinsic_data_property(constructor, get_prototype_atom, get_prototype_of, true)?;
        let create = self.allocate_native_function(
            NativeFunction::ObjectCreate,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_create = Some(create);
        let create_atom = self.intern_intrinsic_name(b"create")?;
        self.set_intrinsic_data_property(constructor, create_atom, create, true)?;
        let is_prototype_of = self.allocate_native_function(
            NativeFunction::ObjectIsPrototypeOf,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_is_prototype_of = Some(is_prototype_of);
        let is_prototype_atom = self.intern_intrinsic_name(b"isPrototypeOf")?;
        self.set_intrinsic_data_property(
            object_prototype,
            is_prototype_atom,
            is_prototype_of,
            true,
        )?;
        let is_extensible = self.allocate_native_function(
            NativeFunction::ObjectIsExtensible,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_is_extensible = Some(is_extensible);
        let is_extensible_atom = self.intern_intrinsic_name(b"isExtensible")?;
        self.set_intrinsic_data_property(constructor, is_extensible_atom, is_extensible, true)?;
        let prevent_extensions = self.allocate_native_function(
            NativeFunction::ObjectPreventExtensions,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_prevent_extensions = Some(prevent_extensions);
        let prevent_extensions_atom = self.intern_intrinsic_name(b"preventExtensions")?;
        self.set_intrinsic_data_property(
            constructor,
            prevent_extensions_atom,
            prevent_extensions,
            true,
        )
    }

    /// Builds primitive conversion constructors with the shared callable prototype.
    fn initialize_primitive_constructors(&mut self) -> Result<(), ExecutionError> {
        let function_prototype = self
            .realm
            .function_prototype
            .expect("function intrinsics initialize before primitive constructors");
        let allocate = |this: &mut Self, native: NativeFunction| -> Result<Value, ExecutionError> {
            this.allocate_native_function(
                native,
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: function_prototype,
                },
            )
        };
        self.realm.string_constructor = Some(allocate(self, NativeFunction::StringConstructor)?);
        let symbol_constructor = allocate(self, NativeFunction::SymbolConstructor)?;
        self.realm.symbol_constructor = Some(symbol_constructor);
        self.initialize_to_primitive_symbol(symbol_constructor)?;
        let number = allocate(self, NativeFunction::NumberConstructor)?;
        self.realm.number_constructor = Some(number);
        let object_prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before Number prototype");
        let number_prototype = self.allocate_number_object(
            Value::from_i32(0),
            object_prototype,
            AllocationSpace::Old,
        )?;
        self.realm.number_prototype = Some(number_prototype);
        self.set_function_prototype(number, number_prototype)?;
        let constructor_atom = self.constructor_atom()?;
        self.set_intrinsic_data_property(number_prototype, constructor_atom, number, true)?;
        let to_exponential = allocate(self, NativeFunction::NumberToExponential)?;
        self.realm.number_to_exponential = Some(to_exponential);
        let to_exponential_atom = self.intern_intrinsic_name(b"toExponential")?;
        self.set_intrinsic_data_property(
            number_prototype,
            to_exponential_atom,
            to_exponential,
            true,
        )?;
        let to_fixed = allocate(self, NativeFunction::NumberToFixed)?;
        self.realm.number_to_fixed = Some(to_fixed);
        let to_fixed_atom = self.intern_intrinsic_name(b"toFixed")?;
        self.set_intrinsic_data_property(number_prototype, to_fixed_atom, to_fixed, true)?;
        let to_precision = allocate(self, NativeFunction::NumberToPrecision)?;
        self.realm.number_to_precision = Some(to_precision);
        let to_precision_atom = self.intern_intrinsic_name(b"toPrecision")?;
        self.set_intrinsic_data_property(number_prototype, to_precision_atom, to_precision, true)?;
        let to_string = allocate(self, NativeFunction::NumberToString)?;
        self.realm.number_to_string = Some(to_string);
        let to_string_atom = self.intern_intrinsic_name(b"toString")?;
        self.set_intrinsic_data_property(number_prototype, to_string_atom, to_string, true)?;
        let value_of = allocate(self, NativeFunction::NumberValueOf)?;
        self.realm.number_value_of = Some(value_of);
        let value_of_atom = self.intern_intrinsic_name(b"valueOf")?;
        self.set_intrinsic_data_property(number_prototype, value_of_atom, value_of, true)?;
        let is_nan = allocate(self, NativeFunction::NumberIsNaN)?;
        self.realm.number_is_nan = Some(is_nan);
        let is_nan_atom = self.intern_intrinsic_name(b"isNaN")?;
        self.set_intrinsic_data_property(number, is_nan_atom, is_nan, true)?;
        let is_finite = allocate(self, NativeFunction::NumberIsFinite)?;
        self.realm.number_is_finite = Some(is_finite);
        let is_finite_atom = self.intern_intrinsic_name(b"isFinite")?;
        self.set_intrinsic_data_property(number, is_finite_atom, is_finite, true)?;
        let is_integer = allocate(self, NativeFunction::NumberIsInteger)?;
        self.realm.number_is_integer = Some(is_integer);
        let is_integer_atom = self.intern_intrinsic_name(b"isInteger")?;
        self.set_intrinsic_data_property(number, is_integer_atom, is_integer, true)?;
        let is_safe_integer = allocate(self, NativeFunction::NumberIsSafeInteger)?;
        self.realm.number_is_safe_integer = Some(is_safe_integer);
        let is_safe_integer_atom = self.intern_intrinsic_name(b"isSafeInteger")?;
        self.set_intrinsic_data_property(number, is_safe_integer_atom, is_safe_integer, true)?;
        for (name, value) in [
            (b"EPSILON".as_slice(), Value::from_f64(f64::EPSILON)),
            (b"MAX_VALUE".as_slice(), Value::from_f64(f64::MAX)),
            (b"MIN_VALUE".as_slice(), Value::from_f64(f64::from_bits(1))),
            (
                b"MAX_SAFE_INTEGER".as_slice(),
                Value::from_f64(MAX_SAFE_INTEGER as f64),
            ),
            (
                b"MIN_SAFE_INTEGER".as_slice(),
                Value::from_f64(-(MAX_SAFE_INTEGER as f64)),
            ),
            (b"NaN".as_slice(), Value::from_f64(f64::NAN)),
            (
                b"POSITIVE_INFINITY".as_slice(),
                Value::from_f64(f64::INFINITY),
            ),
            (
                b"NEGATIVE_INFINITY".as_slice(),
                Value::from_f64(f64::NEG_INFINITY),
            ),
        ] {
            let atom = self.intern_intrinsic_name(name)?;
            self.set_intrinsic_constant_property(number, atom, value)?;
        }
        self.realm.boolean_constructor = Some(allocate(self, NativeFunction::BooleanConstructor)?);
        Ok(())
    }

    /// Allocates and publishes the realm-local well-known `Symbol.toPrimitive` identity.
    fn initialize_to_primitive_symbol(
        &mut self,
        symbol_constructor: Value,
    ) -> Result<(), ExecutionError> {
        let description = self.allocate_runtime_string(
            JsString::try_from_latin1(b"Symbol.toPrimitive")
                .map_err(ExecutionError::PropertyKeyString)?,
        )?;
        let symbol = self.allocate_symbol(Some(description))?;
        self.realm.well_known_symbols.to_primitive = Some(symbol);
        let to_primitive = self.intern_intrinsic_name(b"toPrimitive")?;
        self.set_intrinsic_constant_property(symbol_constructor, to_primitive, symbol)
    }

    /// Builds the callable prototype chain before constructors depend on `%Function.prototype%`.
    fn initialize_function_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let call_atom = self.intern_intrinsic_name(b"call")?;
        let object_prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before Function prototype");
        let call = self.allocate_native_function(
            NativeFunction::FunctionPrototypeCall,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: object_prototype,
            },
        )?;
        self.realm.function_prototype_call = Some(call);
        let shape = self
            .shapes
            .transition_add(
                ShapeId::EMPTY,
                call_atom,
                PropertyAttributes::data(true, false, true),
            )
            .map_err(ExecutionError::Shape)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let storage = self
            .heap
            .try_allocate_external_with_gc(
                self.types.property_storage,
                0,
                PropertyStorage::new(Box::new([call])),
                AllocationSpace::Old,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let function_prototype = self.allocate_native_function(
            NativeFunction::FunctionPrototype,
            OrdinaryObject {
                shape,
                extensible: true,
                storage: Some(storage),
                prototype: object_prototype,
            },
        )?;
        self.realm.function_prototype = Some(function_prototype);
        self.set_function_internal_prototype(call, function_prototype)?;
        let bind = self.allocate_native_function(
            NativeFunction::FunctionPrototypeBind,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.function_prototype_bind = Some(bind);
        let bind_atom = self.intern_intrinsic_name(b"bind")?;
        self.set_intrinsic_data_property(function_prototype, bind_atom, bind, true)?;
        let constructor = self.allocate_native_function(
            NativeFunction::FunctionConstructor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.function_constructor = Some(constructor);
        self.set_function_prototype(constructor, function_prototype)?;
        let constructor_atom = self.constructor_atom()?;
        self.set_intrinsic_data_property(function_prototype, constructor_atom, constructor, true)
    }

    /// Interns every mandatory global name before reserving the dense atom-indexed binding table.
    fn intern_realm_intrinsic_atoms(&mut self) -> Result<RealmIntrinsicAtoms, ExecutionError> {
        Ok(RealmIntrinsicAtoms {
            undefined: self.intern_intrinsic_name(b"undefined")?,
            nan: self.intern_intrinsic_name(b"NaN")?,
            infinity: self.intern_intrinsic_name(b"Infinity")?,
            errors: [
                self.intern_intrinsic_name(b"Error")?,
                self.intern_intrinsic_name(b"ReferenceError")?,
                self.intern_intrinsic_name(b"SyntaxError")?,
                self.intern_intrinsic_name(b"TypeError")?,
                self.intern_intrinsic_name(b"RangeError")?,
            ],
            array: self.intern_intrinsic_name(b"Array")?,
            object: self.intern_intrinsic_name(b"Object")?,
            string: self.intern_intrinsic_name(b"String")?,
            symbol: self.intern_intrinsic_name(b"Symbol")?,
            number: self.intern_intrinsic_name(b"Number")?,
            boolean: self.intern_intrinsic_name(b"Boolean")?,
            function: self.intern_intrinsic_name(b"Function")?,
            math: self.intern_intrinsic_name(b"Math")?,
        })
    }

    /// Creates the base Error pair first, then roots each subclass pair before property allocation.
    fn initialize_error_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let function_prototype = self
            .realm
            .function_prototype
            .expect("function intrinsics initialize before Error intrinsics");
        let constructor_atom = self.constructor_atom()?;
        for kind in NativeErrorKind::ALL {
            let parent = if kind == NativeErrorKind::Error {
                self.realm
                    .object_prototype
                    .expect("Object prototype initializes before Error prototypes")
            } else {
                self.realm
                    .error_intrinsics
                    .get(NativeErrorKind::Error)
                    .prototype
                    .expect("Error.prototype initializes before subclasses")
            };
            let prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: parent,
            })?;
            self.realm.error_intrinsics.get_mut(kind).prototype = Some(prototype);
            let constructor = self.allocate_native_function(
                NativeFunction::ErrorConstructor(kind),
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: function_prototype,
                },
            )?;
            self.realm.error_intrinsics.get_mut(kind).constructor = Some(constructor);
            self.set_function_prototype(constructor, prototype)?;
            self.set_intrinsic_data_property(prototype, constructor_atom, constructor, true)?;
        }
        Ok(())
    }

    /// Builds the ordinary Array constructor/prototype pair and its first indexed method.
    fn initialize_array_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let function_prototype = self
            .realm
            .function_prototype
            .expect("function intrinsics initialize before Array intrinsics");
        let prototype = self.allocate_array_object(
            self.realm
                .object_prototype
                .expect("Object prototype initializes before Array prototype"),
            AllocationSpace::Old,
        )?;
        self.realm.array_prototype = Some(prototype);
        let constructor = self.allocate_native_function(
            NativeFunction::ArrayConstructor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_constructor = Some(constructor);
        self.set_function_prototype(constructor, prototype)?;
        let constructor_atom = self.constructor_atom()?;
        self.set_intrinsic_data_property(prototype, constructor_atom, constructor, true)?;
        let is_array = self.allocate_native_function(
            NativeFunction::ArrayIsArray,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_is_array = Some(is_array);
        let is_array_atom = self.intern_intrinsic_name(b"isArray")?;
        self.set_intrinsic_data_property(constructor, is_array_atom, is_array, true)?;
        let length_atom = self.intern_intrinsic_name(b"length")?;
        self.set_intrinsic_data_property(prototype, length_atom, Value::from_i32(0), false)?;
        let concat = self.allocate_native_function(
            NativeFunction::ArrayConcat,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_concat = Some(concat);
        let concat_atom = self.intern_intrinsic_name(b"concat")?;
        self.set_intrinsic_data_property(prototype, concat_atom, concat, true)?;
        let push = self.allocate_native_function(
            NativeFunction::ArrayPush,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_push = Some(push);
        let push_atom = self.intern_intrinsic_name(b"push")?;
        self.set_intrinsic_data_property(prototype, push_atom, push, true)?;
        let join = self.allocate_native_function(
            NativeFunction::ArrayJoin,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_join = Some(join);
        let join_atom = self.intern_intrinsic_name(b"join")?;
        self.set_intrinsic_data_property(prototype, join_atom, join, true)?;
        let at = self.allocate_native_function(
            NativeFunction::ArrayAt,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_at = Some(at);
        let at_atom = self.intern_intrinsic_name(b"at")?;
        self.set_intrinsic_data_property(prototype, at_atom, at, true)?;
        let index_of = self.allocate_native_function(
            NativeFunction::ArrayIndexOf,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_index_of = Some(index_of);
        let index_of_atom = self.intern_intrinsic_name(b"indexOf")?;
        self.set_intrinsic_data_property(prototype, index_of_atom, index_of, true)?;
        let includes = self.allocate_native_function(
            NativeFunction::ArrayIncludes,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_includes = Some(includes);
        let includes_atom = self.intern_intrinsic_name(b"includes")?;
        self.set_intrinsic_data_property(prototype, includes_atom, includes, true)?;
        let pop = self.allocate_native_function(
            NativeFunction::ArrayPop,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_pop = Some(pop);
        let pop_atom = self.intern_intrinsic_name(b"pop")?;
        self.set_intrinsic_data_property(prototype, pop_atom, pop, true)?;
        let slice = self.allocate_native_function(
            NativeFunction::ArraySlice,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_slice = Some(slice);
        let slice_atom = self.intern_intrinsic_name(b"slice")?;
        self.set_intrinsic_data_property(prototype, slice_atom, slice, true)?;
        let shift = self.allocate_native_function(
            NativeFunction::ArrayShift,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_shift = Some(shift);
        let shift_atom = self.intern_intrinsic_name(b"shift")?;
        self.set_intrinsic_data_property(prototype, shift_atom, shift, true)?;
        let unshift = self.allocate_native_function(
            NativeFunction::ArrayUnshift,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_unshift = Some(unshift);
        let unshift_atom = self.intern_intrinsic_name(b"unshift")?;
        self.set_intrinsic_data_property(prototype, unshift_atom, unshift, true)?;
        let reverse = self.allocate_native_function(
            NativeFunction::ArrayReverse,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_reverse = Some(reverse);
        let reverse_atom = self.intern_intrinsic_name(b"reverse")?;
        self.set_intrinsic_data_property(prototype, reverse_atom, reverse, true)?;
        let fill = self.allocate_native_function(
            NativeFunction::ArrayFill,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_fill = Some(fill);
        let fill_atom = self.intern_intrinsic_name(b"fill")?;
        self.set_intrinsic_data_property(prototype, fill_atom, fill, true)?;
        let last_index_of = self.allocate_native_function(
            NativeFunction::ArrayLastIndexOf,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_last_index_of = Some(last_index_of);
        let last_index_of_atom = self.intern_intrinsic_name(b"lastIndexOf")?;
        self.set_intrinsic_data_property(prototype, last_index_of_atom, last_index_of, true)?;
        let copy_within = self.allocate_native_function(
            NativeFunction::ArrayCopyWithin,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_copy_within = Some(copy_within);
        let copy_within_atom = self.intern_intrinsic_name(b"copyWithin")?;
        self.set_intrinsic_data_property(prototype, copy_within_atom, copy_within, true)?;
        let flat = self.allocate_native_function(
            NativeFunction::ArrayFlat,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_flat = Some(flat);
        let flat_atom = self.intern_intrinsic_name(b"flat")?;
        self.set_intrinsic_data_property(prototype, flat_atom, flat, true)?;
        let sort = self.allocate_native_function(
            NativeFunction::ArraySort,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_sort = Some(sort);
        let sort_atom = self.intern_intrinsic_name(b"sort")?;
        self.set_intrinsic_data_property(prototype, sort_atom, sort, true)?;
        let to_string = self.allocate_native_function(
            NativeFunction::ArrayToString,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_to_string = Some(to_string);
        let to_string_atom = self.intern_intrinsic_name(b"toString")?;
        self.set_intrinsic_data_property(prototype, to_string_atom, to_string, true)
    }

    /// Builds the non-constructor Math namespace and its first numeric intrinsic.
    fn initialize_math_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let object = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: self
                .realm
                .object_prototype
                .expect("Object prototype initializes before Math"),
        })?;
        self.realm.math_object = Some(object);
        let pow = self.allocate_native_function(
            NativeFunction::MathPow,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: self
                    .realm
                    .function_prototype
                    .expect("Function prototype initializes before Math methods"),
            },
        )?;
        self.realm.math_pow = Some(pow);
        let pow_atom = self.intern_intrinsic_name(b"pow")?;
        self.set_intrinsic_data_property(object, pow_atom, pow, true)
    }

    /// Publishes all mandatory names without charging the host quota for user-created globals.
    fn publish_realm_intrinsic_bindings(
        &mut self,
        atoms: RealmIntrinsicAtoms,
    ) -> Result<(), ExecutionError> {
        self.realm.reserve_intrinsics(
            RealmIntrinsicAtoms::BINDING_COUNT,
            self.atoms.stats().entries,
        )?;
        self.realm.publish_intrinsic(
            atoms.undefined,
            Value::from_immediate(Immediate::Undefined),
            false,
        )?;
        self.realm
            .publish_intrinsic(atoms.nan, Value::from_f64(f64::NAN), false)?;
        self.realm
            .publish_intrinsic(atoms.infinity, Value::from_f64(f64::INFINITY), false)?;
        for kind in NativeErrorKind::ALL {
            let constructor = self
                .realm
                .error_intrinsics
                .get(kind)
                .constructor
                .expect("Error constructor initializes before global publication");
            self.realm
                .publish_intrinsic(atoms.error(kind), constructor, true)?;
        }
        self.realm.publish_intrinsic(
            atoms.array,
            self.realm
                .array_constructor
                .expect("Array initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.object,
            self.realm
                .object_constructor
                .expect("Object initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.string,
            self.realm
                .string_constructor
                .expect("String initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.symbol,
            self.realm
                .symbol_constructor
                .expect("Symbol initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.number,
            self.realm
                .number_constructor
                .expect("Number initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.boolean,
            self.realm
                .boolean_constructor
                .expect("Boolean initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.function,
            self.realm
                .function_constructor
                .expect("Function initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.math,
            self.realm
                .math_object
                .expect("Math initializes before global publication"),
            true,
        )?;
        Ok(())
    }
}
