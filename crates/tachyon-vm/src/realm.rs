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
        self.initialize_collection_intrinsics()?;
        self.initialize_math_intrinsics()?;
        self.initialize_json_intrinsics()?;
        self.initialize_reflect_intrinsics()?;
        self.initialize_proxy_intrinsics()?;
        self.initialize_promise_intrinsics()?;
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
        let get_own_property_symbols = self.allocate_native_function(
            NativeFunction::ObjectGetOwnPropertySymbols,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        let get_own_symbols_atom = self.intern_intrinsic_name(b"getOwnPropertySymbols")?;
        self.set_intrinsic_data_property(
            constructor,
            get_own_symbols_atom,
            get_own_property_symbols,
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
        let to_locale_string = self.allocate_native_function(
            NativeFunction::ObjectToLocaleString,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_to_locale_string = Some(to_locale_string);
        let to_locale_string_atom = self.intern_intrinsic_name(b"toLocaleString")?;
        self.set_intrinsic_data_property(
            object_prototype,
            to_locale_string_atom,
            to_locale_string,
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
        let value_of = self.allocate_native_function(
            NativeFunction::ObjectValueOf,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.object_value_of = Some(value_of);
        let value_of_atom = self.intern_intrinsic_name(b"valueOf")?;
        self.set_intrinsic_data_property(object_prototype, value_of_atom, value_of, true)?;
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
        let set_prototype_of = self.allocate_native_function(
            NativeFunction::ObjectSetPrototypeOf,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        let set_prototype_atom = self.intern_intrinsic_name(b"setPrototypeOf")?;
        self.set_intrinsic_data_property(constructor, set_prototype_atom, set_prototype_of, true)?;
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
        )?;
        for (name, native) in [
            (b"seal".as_slice(), NativeFunction::ObjectSeal),
            (b"freeze".as_slice(), NativeFunction::ObjectFreeze),
        ] {
            let function = self.allocate_native_function(
                native,
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: function_prototype,
                },
            )?;
            let atom = self.intern_intrinsic_name(name)?;
            self.set_intrinsic_data_property(constructor, atom, function, true)?;
        }
        Ok(())
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
        let string_constructor = allocate(self, NativeFunction::StringConstructor)?;
        self.realm.string_constructor = Some(string_constructor);
        let string_prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: self
                .realm
                .object_prototype
                .expect("Object prototype initializes before String prototype"),
        })?;
        self.realm.string_prototype = Some(string_prototype);
        self.set_function_prototype(string_constructor, string_prototype)?;
        let constructor_atom = self.constructor_atom()?;
        self.set_intrinsic_data_property(
            string_prototype,
            constructor_atom,
            string_constructor,
            true,
        )?;
        for (native, name) in [
            (NativeFunction::StringCharAt, b"charAt".as_slice()),
            (NativeFunction::StringCharCodeAt, b"charCodeAt".as_slice()),
            (NativeFunction::StringAt, b"at".as_slice()),
            (NativeFunction::StringCodePointAt, b"codePointAt".as_slice()),
            (NativeFunction::StringToString, b"toString".as_slice()),
            (NativeFunction::StringValueOf, b"valueOf".as_slice()),
            (
                NativeFunction::StringIsWellFormed,
                b"isWellFormed".as_slice(),
            ),
            (
                NativeFunction::StringToWellFormed,
                b"toWellFormed".as_slice(),
            ),
            (NativeFunction::StringSlice, b"slice".as_slice()),
            (NativeFunction::StringSubstring, b"substring".as_slice()),
            (NativeFunction::StringIndexOf, b"indexOf".as_slice()),
            (NativeFunction::StringIncludes, b"includes".as_slice()),
            (NativeFunction::StringLastIndexOf, b"lastIndexOf".as_slice()),
            (NativeFunction::StringStartsWith, b"startsWith".as_slice()),
            (NativeFunction::StringEndsWith, b"endsWith".as_slice()),
            (NativeFunction::StringConcat, b"concat".as_slice()),
            (NativeFunction::StringRepeat, b"repeat".as_slice()),
            (NativeFunction::StringPadStart, b"padStart".as_slice()),
            (NativeFunction::StringPadEnd, b"padEnd".as_slice()),
        ] {
            let method = allocate(self, native)?;
            let atom = self.intern_intrinsic_name(name)?;
            self.set_intrinsic_data_property(string_prototype, atom, method, true)?;
        }
        for (native, name) in [
            (
                NativeFunction::StringFromCharCode,
                b"fromCharCode".as_slice(),
            ),
            (
                NativeFunction::StringFromCodePoint,
                b"fromCodePoint".as_slice(),
            ),
        ] {
            let method = allocate(self, native)?;
            let atom = self.intern_intrinsic_name(name)?;
            self.set_intrinsic_data_property(string_constructor, atom, method, true)?;
        }
        let trim = allocate(self, NativeFunction::StringTrim)?;
        let trim_start = allocate(self, NativeFunction::StringTrimStart)?;
        let trim_end = allocate(self, NativeFunction::StringTrimEnd)?;
        for (name, method) in [
            (b"trim".as_slice(), trim),
            (b"trimStart".as_slice(), trim_start),
            (b"trimEnd".as_slice(), trim_end),
            (b"trimLeft".as_slice(), trim_start),
            (b"trimRight".as_slice(), trim_end),
        ] {
            let atom = self.intern_intrinsic_name(name)?;
            self.set_intrinsic_data_property(string_prototype, atom, method, true)?;
        }
        let regexp_constructor = allocate(self, NativeFunction::RegExpConstructor)?;
        self.realm.regexp_constructor = Some(regexp_constructor);
        let regexp_prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: self
                .realm
                .object_prototype
                .expect("Object prototype initializes before RegExp prototype"),
        })?;
        self.realm.regexp_prototype = Some(regexp_prototype);
        self.set_function_prototype(regexp_constructor, regexp_prototype)?;
        self.set_intrinsic_data_property(
            regexp_prototype,
            constructor_atom,
            regexp_constructor,
            true,
        )?;
        for (native, name) in [
            (NativeFunction::RegExpExec, b"exec".as_slice()),
            (NativeFunction::RegExpTest, b"test".as_slice()),
            (NativeFunction::RegExpToString, b"toString".as_slice()),
        ] {
            let method = allocate(self, native)?;
            let atom = self.intern_intrinsic_name(name)?;
            self.set_intrinsic_data_property(regexp_prototype, atom, method, true)?;
        }
        let symbol_constructor = allocate(self, NativeFunction::SymbolConstructor)?;
        self.realm.symbol_constructor = Some(symbol_constructor);
        self.initialize_symbol_prototype(
            symbol_constructor,
            self.realm
                .object_prototype
                .expect("Object initializes before Symbol"),
            function_prototype,
        )?;
        self.initialize_to_primitive_symbol(symbol_constructor)?;
        self.initialize_iterator_symbol(symbol_constructor)?;
        self.initialize_remaining_well_known_symbols(symbol_constructor)?;
        self.initialize_symbol_registry_functions(symbol_constructor, function_prototype)?;
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
        let boolean = allocate(self, NativeFunction::BooleanConstructor)?;
        self.realm.boolean_constructor = Some(boolean);
        let boolean_prototype = self.allocate_boolean_object(
            Value::from_immediate(Immediate::False),
            object_prototype,
            AllocationSpace::Old,
        )?;
        self.realm.boolean_prototype = Some(boolean_prototype);
        self.set_function_prototype(boolean, boolean_prototype)?;
        self.set_intrinsic_data_property(boolean_prototype, constructor_atom, boolean, true)?;
        let boolean_to_string = allocate(self, NativeFunction::BooleanToString)?;
        self.realm.boolean_to_string = Some(boolean_to_string);
        let boolean_to_string_atom = self.intern_intrinsic_name(b"toString")?;
        self.set_intrinsic_data_property(
            boolean_prototype,
            boolean_to_string_atom,
            boolean_to_string,
            true,
        )?;
        let boolean_value_of = allocate(self, NativeFunction::BooleanValueOf)?;
        self.realm.boolean_value_of = Some(boolean_value_of);
        let boolean_value_of_atom = self.intern_intrinsic_name(b"valueOf")?;
        self.set_intrinsic_data_property(
            boolean_prototype,
            boolean_value_of_atom,
            boolean_value_of,
            true,
        )?;
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
        self.set_intrinsic_constant_property(symbol_constructor, to_primitive, symbol)?;
        let function_prototype = self
            .realm
            .function_prototype
            .expect("Function initializes before Symbol methods");
        let key = self.property_key(symbol)?;
        let method = self.allocate_native_function(
            NativeFunction::SymbolToPrimitive,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.define_data_property(
            self.realm
                .symbol_prototype
                .expect("Symbol prototype initializes before methods"),
            key,
            DataPropertyDescriptor {
                value: Some(method),
                writable: Some(false),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )
    }

    /// Installs the ordinary Symbol prototype and the primitive-receiver methods it owns.
    fn initialize_symbol_prototype(
        &mut self,
        symbol_constructor: Value,
        object_prototype: Value,
        function_prototype: Value,
    ) -> Result<(), ExecutionError> {
        let prototype =
            self.allocate_symbol_object(None, object_prototype, AllocationSpace::Old)?;
        self.realm.symbol_prototype = Some(prototype);
        self.set_function_prototype(symbol_constructor, prototype)?;
        let constructor = self.constructor_atom()?;
        self.set_intrinsic_data_property(prototype, constructor, symbol_constructor, true)?;
        for (name, native) in [
            (b"toString".as_slice(), NativeFunction::SymbolToString),
            (b"valueOf".as_slice(), NativeFunction::SymbolValueOf),
        ] {
            self.install_collection_method(prototype, function_prototype, name, native)?;
        }
        self.install_collection_accessor(
            prototype,
            function_prototype,
            b"description",
            NativeFunction::SymbolDescription,
        )?;
        Ok(())
    }

    /// Allocates and publishes the realm-local well-known `Symbol.iterator` identity.
    fn initialize_iterator_symbol(
        &mut self,
        symbol_constructor: Value,
    ) -> Result<(), ExecutionError> {
        let description = self.allocate_runtime_string(
            JsString::try_from_latin1(b"Symbol.iterator")
                .map_err(ExecutionError::PropertyKeyString)?,
        )?;
        let symbol = self.allocate_symbol(Some(description))?;
        self.realm.well_known_symbols.iterator = Some(symbol);
        let iterator = self.intern_intrinsic_name(b"iterator")?;
        self.set_intrinsic_constant_property(symbol_constructor, iterator, symbol)
    }

    /// Publishes the remaining standard well-known Symbols as immutable constructor properties.
    fn initialize_remaining_well_known_symbols(
        &mut self,
        symbol_constructor: Value,
    ) -> Result<(), ExecutionError> {
        for (name, description) in [
            (
                b"asyncDispose".as_slice(),
                b"Symbol.asyncDispose".as_slice(),
            ),
            (
                b"asyncIterator".as_slice(),
                b"Symbol.asyncIterator".as_slice(),
            ),
            (b"dispose".as_slice(), b"Symbol.dispose".as_slice()),
            (b"hasInstance".as_slice(), b"Symbol.hasInstance".as_slice()),
            (
                b"isConcatSpreadable".as_slice(),
                b"Symbol.isConcatSpreadable".as_slice(),
            ),
            (b"match".as_slice(), b"Symbol.match".as_slice()),
            (b"matchAll".as_slice(), b"Symbol.matchAll".as_slice()),
            (b"replace".as_slice(), b"Symbol.replace".as_slice()),
            (b"search".as_slice(), b"Symbol.search".as_slice()),
            (b"species".as_slice(), b"Symbol.species".as_slice()),
            (b"split".as_slice(), b"Symbol.split".as_slice()),
            (b"toStringTag".as_slice(), b"Symbol.toStringTag".as_slice()),
            (b"unscopables".as_slice(), b"Symbol.unscopables".as_slice()),
        ] {
            let description = self.allocate_runtime_string(
                JsString::try_from_latin1(description)
                    .map_err(ExecutionError::PropertyKeyString)?,
            )?;
            let symbol = self.allocate_symbol(Some(description))?;
            let property = self.intern_intrinsic_name(name)?;
            self.set_intrinsic_constant_property(symbol_constructor, property, symbol)?;
            if name == b"toStringTag" {
                self.realm.well_known_symbols.to_string_tag = Some(symbol);
            } else if name == b"species" {
                self.realm.well_known_symbols.species = Some(symbol);
            }
        }
        Ok(())
    }

    /// Installs the non-constructor Symbol registry functions on the intrinsic Symbol function.
    fn initialize_symbol_registry_functions(
        &mut self,
        symbol_constructor: Value,
        function_prototype: Value,
    ) -> Result<(), ExecutionError> {
        for (name, native) in [
            (b"for".as_slice(), NativeFunction::SymbolFor),
            (b"keyFor".as_slice(), NativeFunction::SymbolKeyFor),
        ] {
            let function = self.allocate_native_function(
                native,
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: function_prototype,
                },
            )?;
            let name = self.intern_intrinsic_name(name)?;
            self.set_intrinsic_data_property(symbol_constructor, name, function, true)?;
        }
        Ok(())
    }

    /// Builds the callable prototype chain before constructors depend on `%Function.prototype%`.
    fn initialize_function_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let call_atom = self.intern_intrinsic_name(b"call")?;
        let apply_atom = self.intern_intrinsic_name(b"apply")?;
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
            promise_jobs: &mut self.promise_jobs,
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
        let apply = self.allocate_native_function(
            NativeFunction::FunctionPrototypeApply,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.set_intrinsic_data_property(function_prototype, apply_atom, apply, true)?;
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
            global_this: self.intern_intrinsic_name(b"globalThis")?,
            undefined: self.intern_intrinsic_name(b"undefined")?,
            nan: self.intern_intrinsic_name(b"NaN")?,
            infinity: self.intern_intrinsic_name(b"Infinity")?,
            errors: [
                self.intern_intrinsic_name(b"Error")?,
                self.intern_intrinsic_name(b"EvalError")?,
                self.intern_intrinsic_name(b"ReferenceError")?,
                self.intern_intrinsic_name(b"SyntaxError")?,
                self.intern_intrinsic_name(b"TypeError")?,
                self.intern_intrinsic_name(b"RangeError")?,
                self.intern_intrinsic_name(b"URIError")?,
            ],
            array: self.intern_intrinsic_name(b"Array")?,
            object: self.intern_intrinsic_name(b"Object")?,
            string: self.intern_intrinsic_name(b"String")?,
            regexp: self.intern_intrinsic_name(b"RegExp")?,
            map: self.intern_intrinsic_name(b"Map")?,
            set: self.intern_intrinsic_name(b"Set")?,
            weak_map: self.intern_intrinsic_name(b"WeakMap")?,
            weak_set: self.intern_intrinsic_name(b"WeakSet")?,
            symbol: self.intern_intrinsic_name(b"Symbol")?,
            number: self.intern_intrinsic_name(b"Number")?,
            boolean: self.intern_intrinsic_name(b"Boolean")?,
            function: self.intern_intrinsic_name(b"Function")?,
            math: self.intern_intrinsic_name(b"Math")?,
            json: self.intern_intrinsic_name(b"JSON")?,
            reflect: self.intern_intrinsic_name(b"Reflect")?,
            proxy: self.intern_intrinsic_name(b"Proxy")?,
            promise: self.intern_intrinsic_name(b"Promise")?,
            global_numbers: [
                self.intern_intrinsic_name(b"isFinite")?,
                self.intern_intrinsic_name(b"isNaN")?,
                self.intern_intrinsic_name(b"parseFloat")?,
                self.intern_intrinsic_name(b"parseInt")?,
            ],
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
            let (prototype_parent, constructor_parent) = if kind == NativeErrorKind::Error {
                self.realm
                    .object_prototype
                    .map(|prototype| (prototype, function_prototype))
                    .expect("Object prototype initializes before Error prototypes")
            } else {
                let error = self.realm.error_intrinsics.get(NativeErrorKind::Error);
                (
                    error
                        .prototype
                        .expect("Error.prototype initializes before subclasses"),
                    error
                        .constructor
                        .expect("Error constructor initializes before subclasses"),
                )
            };
            let prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: prototype_parent,
            })?;
            self.realm.error_intrinsics.get_mut(kind).prototype = Some(prototype);
            let constructor = self.allocate_native_function(
                NativeFunction::ErrorConstructor(kind),
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: constructor_parent,
                },
            )?;
            self.realm.error_intrinsics.get_mut(kind).constructor = Some(constructor);
            self.set_function_prototype(constructor, prototype)?;
            self.set_intrinsic_data_property(prototype, constructor_atom, constructor, true)?;
            let name = self.allocate_runtime_string(
                JsString::try_from_latin1(kind.as_str().as_bytes())
                    .map_err(ExecutionError::PropertyKeyString)?,
            )?;
            let name_atom = self.intern_intrinsic_name(b"name")?;
            self.set_intrinsic_data_property(prototype, name_atom, name, true)?;
            let message = self.allocate_runtime_string(
                JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?,
            )?;
            let message_atom = self.message_atom()?;
            self.set_intrinsic_data_property(prototype, message_atom, message, true)?;
            if kind == NativeErrorKind::Error {
                let is_error = self.allocate_native_function(
                    NativeFunction::ErrorIsError,
                    OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: function_prototype,
                    },
                )?;
                let is_error_atom = self.intern_intrinsic_name(b"isError")?;
                self.set_intrinsic_data_property(constructor, is_error_atom, is_error, true)?;
                let to_string = self.allocate_native_function(
                    NativeFunction::ErrorToString,
                    OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype: function_prototype,
                    },
                )?;
                let to_string_atom = self.intern_intrinsic_name(b"toString")?;
                self.set_intrinsic_data_property(prototype, to_string_atom, to_string, true)?;
            }
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
        self.install_species_accessor(constructor, function_prototype)?;
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
        self.initialize_array_iterator_intrinsics(prototype, function_prototype)?;
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
        let for_each = self.allocate_native_function(
            NativeFunction::ArrayForEach,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_for_each = Some(for_each);
        let for_each_atom = self.intern_intrinsic_name(b"forEach")?;
        self.set_intrinsic_data_property(prototype, for_each_atom, for_each, true)?;
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

    /// Builds `%IteratorPrototype%`, `%ArrayIteratorPrototype%`, and Array `values`/`@@iterator`.
    fn initialize_array_iterator_intrinsics(
        &mut self,
        array_prototype: Value,
        function_prototype: Value,
    ) -> Result<(), ExecutionError> {
        let object_prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before iterator prototypes");
        let iterator_prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: object_prototype,
        })?;
        let identity = self.allocate_native_function(
            NativeFunction::IteratorIdentity,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.iterator_identity = Some(identity);
        let iterator_symbol = self
            .realm
            .well_known_symbols
            .iterator
            .expect("Symbol.iterator initializes before Array");
        let iterator_key = self.property_key(iterator_symbol)?;
        self.define_data_property(
            iterator_prototype,
            iterator_key,
            DataPropertyDescriptor {
                value: Some(identity),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )?;
        let array_iterator_prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: iterator_prototype,
        })?;
        self.realm.array_iterator_prototype = Some(array_iterator_prototype);
        let next = self.allocate_native_function(
            NativeFunction::ArrayIteratorNext,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_iterator_next = Some(next);
        let next_atom = self.intern_intrinsic_name(b"next")?;
        self.set_intrinsic_data_property(array_iterator_prototype, next_atom, next, true)?;
        let values = self.allocate_native_function(
            NativeFunction::ArrayValues,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.array_values = Some(values);
        let values_atom = self.intern_intrinsic_name(b"values")?;
        self.set_intrinsic_data_property(array_prototype, values_atom, values, true)?;
        self.define_data_property(
            array_prototype,
            iterator_key,
            DataPropertyDescriptor {
                value: Some(values),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )
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
        let function_prototype = self
            .realm
            .function_prototype
            .expect("Function prototype initializes before Math methods");
        for function in MathFunction::ALL {
            let method = self.allocate_native_function(
                function.native(),
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: function_prototype,
                },
            )?;
            self.realm.math_functions[function.index()] = Some(method);
            if function == MathFunction::Pow {
                self.realm.math_pow = Some(method);
            }
            let atom = self.intern_intrinsic_name(function.name().as_bytes())?;
            self.set_intrinsic_data_property(object, atom, method, true)?;
        }
        for function in GlobalNumberFunction::ALL {
            let method = self.allocate_native_function(
                function.native(),
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: function_prototype,
                },
            )?;
            self.realm.global_number_functions[function.index()] = Some(method);
        }
        Ok(())
    }

    /// Builds Map and Set constructor/prototype pairs before either global becomes observable.
    fn initialize_collection_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let function_prototype = self
            .realm
            .function_prototype
            .expect("Function prototype initializes before collection intrinsics");
        let object_prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before collection intrinsics");
        let map_prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: object_prototype,
        })?;
        self.realm.map_prototype = Some(map_prototype);
        let map = self.allocate_native_function(
            NativeFunction::MapConstructor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.map_constructor = Some(map);
        self.set_function_prototype(map, map_prototype)?;
        let constructor_atom = self.constructor_atom()?;
        self.set_intrinsic_data_property(map_prototype, constructor_atom, map, true)?;
        for (name, native) in [
            (b"get".as_slice(), NativeFunction::MapGet),
            (b"set".as_slice(), NativeFunction::MapSet),
            (b"has".as_slice(), NativeFunction::MapHas),
            (b"delete".as_slice(), NativeFunction::MapDelete),
            (b"clear".as_slice(), NativeFunction::MapClear),
            (b"forEach".as_slice(), NativeFunction::MapForEach),
            (b"getOrInsert".as_slice(), NativeFunction::MapGetOrInsert),
            (
                b"getOrInsertComputed".as_slice(),
                NativeFunction::MapGetOrInsertComputed,
            ),
        ] {
            self.install_collection_method(map_prototype, function_prototype, name, native)?;
        }
        self.install_collection_accessor(
            map_prototype,
            function_prototype,
            b"size",
            NativeFunction::MapSize,
        )?;
        let map_keys = self.install_collection_method(
            map_prototype,
            function_prototype,
            b"keys",
            NativeFunction::MapKeys,
        )?;
        let map_values = self.install_collection_method(
            map_prototype,
            function_prototype,
            b"values",
            NativeFunction::MapValues,
        )?;
        let map_entries = self.install_collection_method(
            map_prototype,
            function_prototype,
            b"entries",
            NativeFunction::MapEntries,
        )?;

        let set_prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: object_prototype,
        })?;
        self.realm.set_prototype = Some(set_prototype);
        let set = self.allocate_native_function(
            NativeFunction::SetConstructor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.set_constructor = Some(set);
        self.set_function_prototype(set, set_prototype)?;
        self.set_intrinsic_data_property(set_prototype, constructor_atom, set, true)?;
        for (name, native) in [
            (b"add".as_slice(), NativeFunction::SetAdd),
            (b"has".as_slice(), NativeFunction::SetHas),
            (b"delete".as_slice(), NativeFunction::SetDelete),
            (b"clear".as_slice(), NativeFunction::SetClear),
            (b"forEach".as_slice(), NativeFunction::SetForEach),
        ] {
            self.install_collection_method(set_prototype, function_prototype, name, native)?;
        }
        self.install_collection_accessor(
            set_prototype,
            function_prototype,
            b"size",
            NativeFunction::SetSize,
        )?;
        let set_values = self.install_collection_method(
            set_prototype,
            function_prototype,
            b"values",
            NativeFunction::SetValues,
        )?;
        let set_entries = self.install_collection_method(
            set_prototype,
            function_prototype,
            b"entries",
            NativeFunction::SetEntries,
        )?;
        let keys_atom = self.intern_intrinsic_name(b"keys")?;
        self.set_intrinsic_data_property(set_prototype, keys_atom, set_values, true)?;

        self.initialize_weak_collection_intrinsics(
            object_prototype,
            function_prototype,
            constructor_atom,
        )?;
        self.install_collection_to_string_tags(map_prototype, set_prototype)?;
        let iterator_symbol = self
            .realm
            .well_known_symbols
            .iterator
            .expect("Symbol.iterator initializes before collections");
        let iterator_key = self.property_key(iterator_symbol)?;
        self.define_data_property(
            map_prototype,
            iterator_key,
            DataPropertyDescriptor {
                value: Some(map_entries),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )?;
        self.define_data_property(
            set_prototype,
            iterator_key,
            DataPropertyDescriptor {
                value: Some(set_values),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )?;
        let iterator_parent = self
            .object_snapshot(
                self.realm
                    .array_iterator_prototype
                    .expect("Array iterator initializes before collections"),
            )?
            .1
            .prototype;
        let map_iterator_prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: iterator_parent,
        })?;
        let set_iterator_prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: iterator_parent,
        })?;
        self.realm.map_iterator_prototype = Some(map_iterator_prototype);
        self.realm.set_iterator_prototype = Some(set_iterator_prototype);
        let next = self.allocate_native_function(
            NativeFunction::CollectionIteratorNext,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        let next_atom = self.intern_intrinsic_name(b"next")?;
        self.set_intrinsic_data_property(map_iterator_prototype, next_atom, next, true)?;
        self.set_intrinsic_data_property(set_iterator_prototype, next_atom, next, true)?;
        let identity = self
            .realm
            .iterator_identity
            .expect("Iterator identity initializes before collections");
        self.define_data_property(
            map_iterator_prototype,
            iterator_key,
            DataPropertyDescriptor {
                value: Some(identity),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )?;
        self.define_data_property(
            set_iterator_prototype,
            iterator_key,
            DataPropertyDescriptor {
                value: Some(identity),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )?;
        let _ = (map_keys, map_values, set_entries);
        Ok(())
    }

    /// Defines the standard configurable `Symbol.toStringTag` properties for collection prototypes.
    fn install_collection_to_string_tags(
        &mut self,
        map_prototype: Value,
        set_prototype: Value,
    ) -> Result<(), ExecutionError> {
        let symbol = self
            .realm
            .well_known_symbols
            .to_string_tag
            .expect("Symbol.toStringTag initializes before collections");
        let key = self.property_key(symbol)?;
        for (prototype, tag) in [
            (map_prototype, b"Map".as_slice()),
            (set_prototype, b"Set".as_slice()),
            (
                self.realm
                    .weak_map_prototype
                    .expect("WeakMap prototype initializes before tag publication"),
                b"WeakMap".as_slice(),
            ),
            (
                self.realm
                    .weak_set_prototype
                    .expect("WeakSet prototype initializes before tag publication"),
                b"WeakSet".as_slice(),
            ),
        ] {
            let value = self.allocate_runtime_string(
                JsString::try_from_latin1(tag).map_err(ExecutionError::PropertyKeyString)?,
            )?;
            self.define_data_property(
                prototype,
                key,
                DataPropertyDescriptor {
                    value: Some(value),
                    writable: Some(false),
                    enumerable: Some(false),
                    configurable: Some(true),
                },
            )?;
        }
        Ok(())
    }

    /// Installs weak collection constructors and prototype methods over ephemeron-backed objects.
    fn initialize_weak_collection_intrinsics(
        &mut self,
        object_prototype: Value,
        function_prototype: Value,
        constructor_atom: AtomId,
    ) -> Result<(), ExecutionError> {
        let weak_map_prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: object_prototype,
        })?;
        let weak_map = self.allocate_native_function(
            NativeFunction::WeakMapConstructor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.weak_map_prototype = Some(weak_map_prototype);
        self.realm.weak_map_constructor = Some(weak_map);
        self.set_function_prototype(weak_map, weak_map_prototype)?;
        self.set_intrinsic_data_property(weak_map_prototype, constructor_atom, weak_map, true)?;
        for (name, native) in [
            (b"delete".as_slice(), NativeFunction::WeakMapDelete),
            (b"get".as_slice(), NativeFunction::WeakMapGet),
            (b"has".as_slice(), NativeFunction::WeakMapHas),
            (b"set".as_slice(), NativeFunction::WeakMapSet),
            (
                b"getOrInsert".as_slice(),
                NativeFunction::WeakMapGetOrInsert,
            ),
            (
                b"getOrInsertComputed".as_slice(),
                NativeFunction::WeakMapGetOrInsertComputed,
            ),
        ] {
            self.install_collection_method(weak_map_prototype, function_prototype, name, native)?;
        }

        let weak_set_prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: object_prototype,
        })?;
        let weak_set = self.allocate_native_function(
            NativeFunction::WeakSetConstructor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.weak_set_prototype = Some(weak_set_prototype);
        self.realm.weak_set_constructor = Some(weak_set);
        self.set_function_prototype(weak_set, weak_set_prototype)?;
        self.set_intrinsic_data_property(weak_set_prototype, constructor_atom, weak_set, true)?;
        for (name, native) in [
            (b"add".as_slice(), NativeFunction::WeakSetAdd),
            (b"delete".as_slice(), NativeFunction::WeakSetDelete),
            (b"has".as_slice(), NativeFunction::WeakSetHas),
        ] {
            self.install_collection_method(weak_set_prototype, function_prototype, name, native)?;
        }
        Ok(())
    }

    /// Installs a standard writable, non-enumerable collection prototype method.
    fn install_collection_method(
        &mut self,
        prototype: Value,
        function_prototype: Value,
        name: &[u8],
        native: NativeFunction,
    ) -> Result<Value, ExecutionError> {
        let method = self.allocate_native_function(
            native,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        let name = self.intern_intrinsic_name(name)?;
        self.set_intrinsic_data_property(prototype, name, method, true)?;
        Ok(method)
    }

    /// Installs `size` as a getter, retaining ordinary accessor observability for later completion.
    fn install_collection_accessor(
        &mut self,
        prototype: Value,
        function_prototype: Value,
        name: &[u8],
        native: NativeFunction,
    ) -> Result<(), ExecutionError> {
        let getter = self.allocate_native_function(
            native,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        let name = self.intern_intrinsic_name(name)?;
        self.define_property(
            prototype,
            PropertyKey::Atom(name),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(getter),
                setter: Some(Value::from_immediate(Immediate::Undefined)),
                enumerable: Some(false),
                configurable: Some(true),
            }),
        )
    }

    /// Builds the non-constructor JSON namespace and its UTF-16 parser entry point.
    fn initialize_json_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let object = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: self
                .realm
                .object_prototype
                .expect("Object prototype initializes before JSON"),
        })?;
        self.realm.json_object = Some(object);
        let function_prototype = self
            .realm
            .function_prototype
            .expect("Function prototype initializes before JSON.parse");
        let parse = self.allocate_native_function(
            NativeFunction::JsonParse,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.json_parse = Some(parse);
        let parse_atom = self.intern_intrinsic_name(b"parse")?;
        self.set_intrinsic_data_property(object, parse_atom, parse, true)?;
        let stringify = self.allocate_native_function(
            NativeFunction::JsonStringify,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.json_stringify = Some(stringify);
        let stringify_atom = self.intern_intrinsic_name(b"stringify")?;
        self.set_intrinsic_data_property(object, stringify_atom, stringify, true)
    }

    /// Installs the ordinary-internal-method Reflect subset before Proxy dispatch is available.
    fn initialize_reflect_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let object = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: self
                .realm
                .object_prototype
                .expect("Object prototype initializes before Reflect"),
        })?;
        self.realm.reflect_object = Some(object);
        for (name, native) in [
            (b"apply".as_slice(), NativeFunction::ReflectApply),
            (b"construct".as_slice(), NativeFunction::ReflectConstruct),
            (b"ownKeys".as_slice(), NativeFunction::ReflectOwnKeys),
            (
                b"defineProperty".as_slice(),
                NativeFunction::ReflectDefineProperty,
            ),
            (
                b"deleteProperty".as_slice(),
                NativeFunction::ReflectDeleteProperty,
            ),
            (
                b"getOwnPropertyDescriptor".as_slice(),
                NativeFunction::ReflectGetOwnPropertyDescriptor,
            ),
            (b"get".as_slice(), NativeFunction::ReflectGet),
            (
                b"getPrototypeOf".as_slice(),
                NativeFunction::ReflectGetPrototypeOf,
            ),
            (b"has".as_slice(), NativeFunction::ReflectHas),
            (
                b"isExtensible".as_slice(),
                NativeFunction::ReflectIsExtensible,
            ),
            (
                b"preventExtensions".as_slice(),
                NativeFunction::ReflectPreventExtensions,
            ),
            (b"set".as_slice(), NativeFunction::ReflectSet),
            (
                b"setPrototypeOf".as_slice(),
                NativeFunction::ReflectSetPrototypeOf,
            ),
        ] {
            let method = self.allocate_native_function(
                native,
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: self
                        .realm
                        .function_prototype
                        .expect("Function prototype initializes before Reflect"),
                },
            )?;
            let key = self.intern_intrinsic_name(name)?;
            self.set_intrinsic_data_property(object, key, method, true)?;
        }
        let symbol = self
            .realm
            .well_known_symbols
            .to_string_tag
            .expect("Symbol.toStringTag initializes before Reflect");
        let tag = self.allocate_runtime_string(
            JsString::try_from_latin1(b"Reflect").map_err(ExecutionError::PropertyKeyString)?,
        )?;
        let key = self.property_key(symbol)?;
        self.define_data_property(
            object,
            key,
            DataPropertyDescriptor {
                value: Some(tag),
                writable: Some(false),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )?;
        Ok(())
    }

    /// Creates the construct-only Proxy intrinsic without an ordinary default prototype property.
    fn initialize_proxy_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let constructor = self.allocate_native_function(
            NativeFunction::ProxyConstructor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: self
                    .realm
                    .function_prototype
                    .expect("Function prototype initializes before Proxy"),
            },
        )?;
        self.realm.proxy_constructor = Some(constructor);
        let revocable = self.allocate_native_function(
            NativeFunction::ProxyRevocable,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: self
                    .realm
                    .function_prototype
                    .expect("Function prototype initializes before Proxy"),
            },
        )?;
        let revocable_atom = self.intern_intrinsic_name(b"revocable")?;
        self.set_intrinsic_data_property(constructor, revocable_atom, revocable, true)?;
        Ok(())
    }

    /// Installs the Promise identity and the first allocation-only static operations.
    fn initialize_promise_intrinsics(&mut self) -> Result<(), ExecutionError> {
        let function_prototype = self
            .realm
            .function_prototype
            .expect("Function prototype initializes before Promise");
        let object_prototype = self
            .realm
            .object_prototype
            .expect("Object prototype initializes before Promise");
        let constructor = self.allocate_native_function(
            NativeFunction::PromiseConstructor,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        let prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: object_prototype,
        })?;
        self.realm.promise_constructor = Some(constructor);
        self.realm.promise_prototype = Some(prototype);
        self.set_function_prototype(constructor, prototype)?;
        self.install_species_accessor(constructor, function_prototype)?;
        let constructor_atom = self.constructor_atom()?;
        self.set_intrinsic_data_property(prototype, constructor_atom, constructor, true)?;
        for (name, native) in [
            (b"resolve".as_slice(), NativeFunction::PromiseResolve),
            (b"reject".as_slice(), NativeFunction::PromiseReject),
        ] {
            let function = self.allocate_native_function(
                native,
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: function_prototype,
                },
            )?;
            let atom = self.intern_intrinsic_name(name)?;
            self.set_intrinsic_data_property(constructor, atom, function, true)?;
            if native == NativeFunction::PromiseResolve {
                self.realm.promise_resolve = Some(function);
            } else {
                self.realm.promise_reject = Some(function);
            }
        }
        for (name, native) in [
            (b"then".as_slice(), NativeFunction::PromiseThen),
            (b"catch".as_slice(), NativeFunction::PromiseCatch),
        ] {
            let function = self.allocate_native_function(
                native,
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    extensible: true,
                    storage: None,
                    prototype: function_prototype,
                },
            )?;
            let atom = self.intern_intrinsic_name(name)?;
            self.set_intrinsic_data_property(prototype, atom, function, true)?;
            if native == NativeFunction::PromiseThen {
                self.realm.promise_then = Some(function);
            } else {
                self.realm.promise_catch = Some(function);
            }
        }
        Ok(())
    }

    /// Installs the standard inherited-constructor species accessor on one intrinsic constructor.
    fn install_species_accessor(
        &mut self,
        constructor: Value,
        function_prototype: Value,
    ) -> Result<(), ExecutionError> {
        let getter = self.allocate_native_function(
            NativeFunction::SpeciesGetter,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        let species = self
            .realm
            .well_known_symbols
            .species
            .expect("Symbol.species initializes before intrinsic constructors");
        let key = self.property_key(species)?;
        self.define_property(
            constructor,
            key,
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(getter),
                setter: Some(Value::from_immediate(Immediate::Undefined)),
                enumerable: Some(false),
                configurable: Some(true),
            }),
        )
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
        let global_object = self
            .realm
            .global_object
            .expect("global object initializes before intrinsic publication");
        self.realm
            .publish_intrinsic(atoms.global_this, global_object, true)?;
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
            atoms.regexp,
            self.realm
                .regexp_constructor
                .expect("RegExp initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.map,
            self.realm
                .map_constructor
                .expect("Map initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.set,
            self.realm
                .set_constructor
                .expect("Set initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.weak_map,
            self.realm
                .weak_map_constructor
                .expect("WeakMap initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.weak_set,
            self.realm
                .weak_set_constructor
                .expect("WeakSet initializes before global publication"),
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
        self.realm.publish_intrinsic(
            atoms.json,
            self.realm
                .json_object
                .expect("JSON initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.reflect,
            self.realm
                .reflect_object
                .expect("Reflect initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.proxy,
            self.realm
                .proxy_constructor
                .expect("Proxy initializes before global publication"),
            true,
        )?;
        self.realm.publish_intrinsic(
            atoms.promise,
            self.realm
                .promise_constructor
                .expect("Promise initializes before global publication"),
            true,
        )?;
        for (atom, value) in atoms
            .global_numbers
            .into_iter()
            .zip(self.realm.global_number_functions)
        {
            self.realm.publish_intrinsic(
                atom,
                value.expect("numeric global initializes before publication"),
                true,
            )?;
        }
        let mut globals = Vec::new();
        globals
            .try_reserve_exact(self.realm.intrinsic_bindings.len())
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        for binding in &self.realm.intrinsic_bindings {
            globals.push((binding.name, binding.value, binding.writable));
        }
        for (name, value, writable) in globals {
            self.define_data_property(
                global_object,
                name,
                DataPropertyDescriptor {
                    value: Some(value),
                    writable: Some(writable),
                    enumerable: Some(false),
                    configurable: Some(true),
                },
            )?;
        }
        Ok(())
    }
}
