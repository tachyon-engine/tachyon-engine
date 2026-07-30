use super::{fixtures::*, *};

#[test]
/// Calls the key-only builtin directly so accessor storage cannot hide behind facade errors.
fn object_keys_reads_accessor_metadata_without_loading_its_value() {
    let mut isolate = test_isolate();
    let object = isolate.create_ordinary_object().unwrap();
    let visible = isolate.intern_intrinsic_name(b"visible").unwrap();
    isolate
        .define_property(
            object,
            visible.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(isolate.realm.object_constructor.unwrap()),
                setter: None,
                enumerable: Some(true),
                configurable: Some(true),
            }),
        )
        .unwrap();
    isolate.fiber.registers = vec![object, Value::from_immediate(Immediate::Undefined)];
    let result = isolate
        .object_enumeration(
            &CallSite {
                caller_base: 0,
                destination: 1,
                callee: isolate.realm.object_keys.unwrap(),
                argument_base: 0,
                argument_source: None,
                argument_prefix: None,
                argument_prefix_offset: 0,
                argument_prefix_count: 0,
                argument_count: 1,
                this_value: Value::from_immediate(Immediate::Undefined),
                new_target: Value::from_immediate(Immediate::Undefined),
                construct_receiver: None,
                call_site: WordOffset::new(0),
            },
            NativeFunction::ObjectKeys,
        )
        .unwrap();
    let zero = isolate.property_key_atom(Value::from_i32(0)).unwrap();
    let key = isolate.get_data_property(result, zero).unwrap().unwrap();
    let expected = isolate.atom_string_value(visible).unwrap();
    assert!(isolate.strict_equal_values(key, expected).unwrap());
}

#[test]
fn ordinary_property_replacement_and_update_work_for_every_dispatch_batch() {
    assert_property_batch::<1>();
    assert_property_batch::<2>();
    assert_property_batch::<4>();
    assert_property_batch::<8>();
    assert_property_batch::<16>();
}

#[test]
fn computed_property_access_works_for_every_dispatch_batch() {
    assert_dynamic_property_batch::<1>();
    assert_dynamic_property_batch::<2>();
    assert_dynamic_property_batch::<4>();
    assert_dynamic_property_batch::<8>();
    assert_dynamic_property_batch::<16>();
}

#[test]
/// Forces collection during first numeric-key publication to exercise the shared rooting path.
fn computed_property_publication_roots_receiver_across_forced_major() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &dynamic_property_module(),
            ExecutionBudget {
                fuel: 8,
                quantum: 8,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

#[test]
fn integer_property_keys_preserve_ecmascript_decimal_spelling() {
    for (value, expected) in [
        (i32::MIN, "-2147483648"),
        (-1, "-1"),
        (0, "0"),
        (i32::MAX, "2147483647"),
    ] {
        let key = Int32PropertyKey::new(value);
        assert_eq!(key.as_bytes(), expected.as_bytes());
    }
}

#[test]
/// Plain closure creation stays allocation-light until prototype observation materializes it.
fn function_prototype_is_lazily_materialized_with_constructor_back_reference() {
    let mut isolate = test_isolate();
    let outcome = isolate
        .execute_with_batch::<8>(
            &call_module(),
            ExecutionBudget {
                fuel: 1,
                quantum: 1,
            },
        )
        .unwrap();
    assert_eq!(outcome, RunOutcome::BudgetExhausted);
    let function = isolate.fiber.registers[0];
    let reference = isolate
        .heap
        .checked_reference(function.as_heap_ref().unwrap(), isolate.types.function)
        .unwrap();
    let before = isolate.heap.with_running_scope(|scope| {
        let function = scope.root(reference).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow(function, isolate.types.function)
                .unwrap()
                .prototype_or_home_object
        })
    });
    assert!(before.is_none());

    let prototype_atom = isolate.prototype_atom().unwrap();
    let prototype = isolate
        .get_data_property(function, prototype_atom)
        .unwrap()
        .unwrap();
    let constructor_atom = isolate.constructor_atom().unwrap();
    assert_eq!(
        isolate
            .get_data_property(prototype, constructor_atom)
            .unwrap(),
        Some(function)
    );
}

#[test]
fn property_publication_roots_receiver_and_heap_value_across_forced_major() {
    for module in [
        heap_value_property_module(),
        function_heap_value_property_module(),
    ] {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &module,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();
        let RunOutcome::Completed(child) = outcome else {
            panic!("property fixture must complete");
        };
        assert!(isolate.object_snapshot(child).is_ok());
    }
}

#[test]
fn symbols_with_the_same_description_are_distinct_property_keys() {
    let mut isolate = test_isolate();
    let description = isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"same").unwrap())
        .unwrap();
    isolate.fiber.registers.push(description);
    let first = isolate.allocate_symbol(Some(description)).unwrap();
    isolate.fiber.registers.push(first);
    let second = isolate.allocate_symbol(Some(description)).unwrap();
    isolate.fiber.registers.push(second);
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(object);

    let first_key = isolate.property_key(first).unwrap();
    let second_key = isolate.property_key(second).unwrap();
    assert_ne!(first_key, second_key);
    isolate
        .set_own_data_property(object, first_key, Value::from_i32(11))
        .unwrap();
    isolate
        .set_own_data_property(object, second_key, Value::from_i32(22))
        .unwrap();

    assert_eq!(
        isolate.get_data_property(object, first_key).unwrap(),
        Some(Value::from_i32(11))
    );
    assert_eq!(
        isolate.get_data_property(object, second_key).unwrap(),
        Some(Value::from_i32(22))
    );
}

#[test]
fn first_symbol_property_publication_roots_receiver_key_and_value_across_forced_major() {
    let mut isolate = test_isolate();
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(object);
    let symbol = isolate.allocate_symbol(None).unwrap();
    isolate.fiber.registers.push(symbol);
    let value = isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"value").unwrap())
        .unwrap();
    isolate.fiber.registers.clear();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);

    let key = isolate.property_key(symbol).unwrap();
    isolate.set_own_data_property(object, key, value).unwrap();
    isolate.fiber.registers.push(object);
    isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"collect").unwrap())
        .unwrap();

    let (_, snapshot) = isolate.object_snapshot(object).unwrap();
    let stored_symbol = isolate
        .symbol_property_key_value(snapshot, key.symbol().unwrap())
        .unwrap();
    assert_eq!(stored_symbol, Some(symbol));
    let stored_value = isolate.get_data_property(object, key).unwrap().unwrap();
    let mut units = Vec::new();
    isolate
        .append_primitive_string_units(stored_value, &mut units)
        .unwrap();
    assert_eq!(units, "value".encode_utf16().collect::<Vec<_>>());
}

#[test]
fn deleting_the_last_symbol_property_releases_its_gc_edge() {
    let mut isolate = test_isolate();
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(object);
    let symbol = isolate.allocate_symbol(None).unwrap();
    let symbol_raw = symbol.as_heap_ref().unwrap();
    let key = isolate.property_key(symbol).unwrap();
    isolate
        .set_own_data_property(object, key, Value::from_i32(7))
        .unwrap();

    collect_major(&mut isolate);
    assert!(
        isolate
            .heap
            .checked_reference(symbol_raw, isolate.types.symbol)
            .is_ok()
    );
    assert!(isolate.delete_own_data_property(object, key).unwrap());
    collect_major(&mut isolate);

    assert!(
        isolate
            .heap
            .checked_reference(symbol_raw, isolate.types.symbol)
            .is_err()
    );
    let (_, snapshot) = isolate.object_snapshot(object).unwrap();
    assert_eq!(
        isolate
            .symbol_property_key_value(snapshot, key.symbol().unwrap())
            .unwrap(),
        None
    );
}

#[test]
fn readding_a_live_deleted_symbol_restores_its_gc_edge() {
    let mut isolate = test_isolate();
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(object);
    let symbol = isolate.allocate_symbol(None).unwrap();
    isolate.fiber.registers.push(symbol);
    let symbol_raw = symbol.as_heap_ref().unwrap();
    let key = isolate.property_key(symbol).unwrap();
    isolate
        .set_own_data_property(object, key, Value::from_i32(1))
        .unwrap();
    assert!(isolate.delete_own_data_property(object, key).unwrap());
    collect_major(&mut isolate);

    isolate
        .set_own_data_property(object, key, Value::from_i32(2))
        .unwrap();
    assert_eq!(isolate.fiber.registers.pop(), Some(symbol));
    collect_major(&mut isolate);

    assert!(
        isolate
            .heap
            .checked_reference(symbol_raw, isolate.types.symbol)
            .is_ok()
    );
    assert_eq!(
        isolate.get_data_property(object, key).unwrap(),
        Some(Value::from_i32(2))
    );
    let (_, snapshot) = isolate.object_snapshot(object).unwrap();
    assert_eq!(
        isolate
            .symbol_property_key_value(snapshot, key.symbol().unwrap())
            .unwrap(),
        Some(symbol)
    );
}

#[test]
/// The old backing remains the root of copied values until compact replacement publication.
fn structural_delete_preserves_retained_values_across_forced_major() {
    let mut isolate = test_isolate();
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(object);
    let first = isolate.intern_intrinsic_name(b"first").unwrap();
    let middle = isolate.intern_intrinsic_name(b"middle").unwrap();
    let last = isolate.intern_intrinsic_name(b"last").unwrap();
    let first_value = isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"one").unwrap())
        .unwrap();
    isolate
        .set_own_data_property(object, first, first_value)
        .unwrap();
    isolate
        .set_own_data_property(object, middle, Value::from_i32(2))
        .unwrap();
    let last_value = isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"three").unwrap())
        .unwrap();
    isolate
        .set_own_data_property(object, last, last_value)
        .unwrap();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);

    assert!(isolate.delete_own_data_property(object, middle).unwrap());
    assert_eq!(
        isolate.get_data_property(object, first).unwrap(),
        Some(first_value)
    );
    assert_eq!(
        isolate.get_data_property(object, last).unwrap(),
        Some(last_value)
    );
    let (_, snapshot) = isolate.object_snapshot(object).unwrap();
    assert_eq!(isolate.shapes.property_count(snapshot.shape), 2);
}

#[test]
/// Exercises every closed descriptor class and rejects structurally invalid descriptor objects.
fn property_descriptor_parser_closes_field_combinations() {
    let mut isolate = test_isolate();
    let generic = descriptor_object(&mut isolate, &[]);
    assert!(matches!(
        isolate.parse_property_descriptor(generic).unwrap(),
        PropertyDescriptor::Generic(GenericPropertyDescriptor {
            enumerable: None,
            configurable: None,
        })
    ));

    let data = descriptor_object(&mut isolate, &[(b"value", Value::from_i32(7))]);
    assert!(matches!(
        isolate.parse_property_descriptor(data).unwrap(),
        PropertyDescriptor::Data(DataPropertyDescriptor {
            value: Some(value),
            writable: None,
            enumerable: None,
            configurable: None,
        }) if value.as_i32() == Some(7)
    ));

    let undefined = Value::from_immediate(Immediate::Undefined);
    let accessor = descriptor_object(&mut isolate, &[(b"get", undefined)]);
    assert!(matches!(
        isolate.parse_property_descriptor(accessor).unwrap(),
        PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
            getter: Some(value),
            setter: None,
            enumerable: None,
            configurable: None,
        }) if value == undefined
    ));

    let mixed = descriptor_object(
        &mut isolate,
        &[(b"value", Value::from_i32(1)), (b"get", undefined)],
    );
    assert!(matches!(
        isolate.parse_property_descriptor(mixed),
        Err(ExecutionError::InvalidPropertyDescriptor(value)) if value == mixed
    ));

    for field in [b"get".as_slice(), b"set".as_slice()] {
        let invalid = descriptor_object(&mut isolate, &[(field, Value::from_i32(1))]);
        assert!(matches!(
            isolate.parse_property_descriptor(invalid),
            Err(ExecutionError::NonCallable(value)) if value.as_i32() == Some(1)
        ));
    }
}

#[test]
/// Kind conversions retain one logical slot while replacing only its compact payload form.
fn data_accessor_data_conversion_preserves_slot_and_count() {
    let mut isolate = test_isolate();
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(object);
    let key = isolate.intern_intrinsic_name(b"answer").unwrap();
    isolate
        .define_property(
            object,
            key.into(),
            PropertyDescriptor::Data(DataPropertyDescriptor {
                value: Some(Value::from_i32(7)),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
            }),
        )
        .unwrap();
    let (_, data_snapshot) = isolate.object_snapshot(object).unwrap();
    let data_lookup = isolate.shapes.lookup(data_snapshot.shape, key).unwrap();

    let getter = isolate.realm.object_constructor.unwrap();
    let undefined = Value::from_immediate(Immediate::Undefined);
    isolate
        .define_property(
            object,
            key.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(getter),
                setter: None,
                enumerable: None,
                configurable: None,
            }),
        )
        .unwrap();
    let (_, accessor_snapshot) = isolate.object_snapshot(object).unwrap();
    let accessor_lookup = isolate.shapes.lookup(accessor_snapshot.shape, key).unwrap();
    assert_eq!(accessor_lookup.kind, PropertyKind::Accessor);
    assert_eq!(accessor_lookup.slot, data_lookup.slot);
    assert_eq!(
        isolate.shapes.property_count(accessor_snapshot.shape),
        isolate.shapes.property_count(data_snapshot.shape)
    );
    assert!(matches!(
        isolate.complete_own_property_descriptor(object, key).unwrap(),
        Some(PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
            getter: Some(stored_getter),
            setter: Some(stored_setter),
            enumerable: Some(true),
            configurable: Some(true),
        })) if stored_getter == getter && stored_setter == undefined
    ));

    isolate
        .define_property(
            object,
            key.into(),
            PropertyDescriptor::Data(DataPropertyDescriptor {
                value: Some(Value::from_i32(9)),
                writable: Some(false),
                enumerable: None,
                configurable: None,
            }),
        )
        .unwrap();
    let (_, final_snapshot) = isolate.object_snapshot(object).unwrap();
    let final_lookup = isolate.shapes.lookup(final_snapshot.shape, key).unwrap();
    assert_eq!(final_lookup.kind, PropertyKind::Data);
    assert_eq!(final_lookup.slot, data_lookup.slot);
    assert_eq!(
        isolate.shapes.property_count(final_snapshot.shape),
        isolate.shapes.property_count(data_snapshot.shape)
    );
    assert!(matches!(
        isolate.complete_own_property_descriptor(object, key).unwrap(),
        Some(PropertyDescriptor::Data(DataPropertyDescriptor {
            value: Some(value),
            writable: Some(false),
            enumerable: Some(true),
            configurable: Some(true),
        })) if value.as_i32() == Some(9)
    ));
}

#[test]
/// Non-configurable, non-writable data properties permit only SameValue payload repetition.
fn non_configurable_data_descriptor_rejects_forbidden_changes() {
    let mut isolate = test_isolate();
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(object);
    let key = isolate.intern_intrinsic_name(b"fixed").unwrap();
    isolate
        .define_property(
            object,
            key.into(),
            PropertyDescriptor::Data(DataPropertyDescriptor {
                value: Some(Value::from_i32(7)),
                writable: Some(false),
                enumerable: Some(false),
                configurable: Some(false),
            }),
        )
        .unwrap();

    isolate
        .define_property(
            object,
            key.into(),
            PropertyDescriptor::Data(DataPropertyDescriptor {
                value: Some(Value::from_i32(7)),
                ..DataPropertyDescriptor::default()
            }),
        )
        .unwrap();
    for descriptor in [
        PropertyDescriptor::Data(DataPropertyDescriptor {
            value: Some(Value::from_i32(8)),
            ..DataPropertyDescriptor::default()
        }),
        PropertyDescriptor::Data(DataPropertyDescriptor {
            writable: Some(true),
            ..DataPropertyDescriptor::default()
        }),
        PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
            getter: Some(Value::from_immediate(Immediate::Undefined)),
            ..AccessorPropertyDescriptor::default()
        }),
    ] {
        assert!(matches!(
            isolate.define_property(object, key.into(), descriptor),
            Err(ExecutionError::InvalidPropertyRedefinition(value)) if value == object
        ));
    }
}

#[test]
/// Non-configurable accessors accept identical pairs and reject pair or descriptor-kind changes.
fn non_configurable_accessor_descriptor_rejects_forbidden_changes() {
    let mut isolate = test_isolate();
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(object);
    let key = isolate.intern_intrinsic_name(b"fixedAccessor").unwrap();
    let getter = isolate.realm.object_constructor.unwrap();
    let setter = isolate.realm.array_constructor.unwrap();
    isolate
        .define_property(
            object,
            key.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(getter),
                setter: Some(setter),
                enumerable: Some(false),
                configurable: Some(false),
            }),
        )
        .unwrap();
    isolate
        .define_property(
            object,
            key.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(getter),
                setter: Some(setter),
                ..AccessorPropertyDescriptor::default()
            }),
        )
        .unwrap();

    let different_getter = isolate.realm.string_constructor.unwrap();
    for descriptor in [
        PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
            getter: Some(different_getter),
            ..AccessorPropertyDescriptor::default()
        }),
        PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
            setter: Some(different_getter),
            ..AccessorPropertyDescriptor::default()
        }),
        PropertyDescriptor::Data(DataPropertyDescriptor {
            value: Some(Value::from_i32(1)),
            ..DataPropertyDescriptor::default()
        }),
    ] {
        assert!(matches!(
            isolate.define_property(object, key.into(), descriptor),
            Err(ExecutionError::InvalidPropertyRedefinition(value)) if value == object
        ));
    }
}

#[test]
/// First publication keeps every unpublished edge alive across both forced-major allocations.
fn accessor_publication_roots_receiver_pair_getter_and_setter() {
    let mut isolate = test_isolate();
    let key = isolate.intern_intrinsic_name(b"accessor").unwrap();
    let getter = allocate_young_test_function(&mut isolate);
    isolate.fiber.registers.push(getter);
    let setter = allocate_young_test_function(&mut isolate);
    isolate.fiber.registers.push(setter);
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.clear();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);

    isolate
        .define_property(
            object,
            key.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(getter),
                setter: Some(setter),
                enumerable: Some(true),
                configurable: Some(true),
            }),
        )
        .unwrap();
    isolate.fiber.registers.push(object);
    let pair_raw = accessor_pair_raw(&mut isolate, object, key);

    assert!(
        isolate
            .heap
            .checked_reference(getter.as_heap_ref().unwrap(), isolate.types.function)
            .is_ok()
    );
    assert!(
        isolate
            .heap
            .checked_reference(setter.as_heap_ref().unwrap(), isolate.types.function)
            .is_ok()
    );
    assert!(
        isolate
            .heap
            .checked_reference(pair_raw, isolate.types.accessor_pair)
            .is_ok()
    );
    assert!(matches!(
        isolate.complete_own_property_descriptor(object, key).unwrap(),
        Some(PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
            getter: Some(stored_getter),
            setter: Some(stored_setter),
            enumerable: Some(true),
            configurable: Some(true),
        })) if stored_getter == getter && stored_setter == setter
    ));
}

#[test]
/// Accessor-to-data conversion removes both the pair edge and its sole callable edge.
fn accessor_to_data_conversion_releases_pair_and_callable() {
    let mut isolate = test_isolate();
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(object);
    let key = isolate.intern_intrinsic_name(b"temporaryAccessor").unwrap();
    let getter = allocate_young_test_function(&mut isolate);
    let getter_raw = getter.as_heap_ref().unwrap();
    isolate
        .define_property(
            object,
            key.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(getter),
                setter: None,
                enumerable: Some(false),
                configurable: Some(true),
            }),
        )
        .unwrap();
    let pair_raw = accessor_pair_raw(&mut isolate, object, key);
    collect_major(&mut isolate);
    assert!(
        isolate
            .heap
            .checked_reference(getter_raw, isolate.types.function)
            .is_ok()
    );

    isolate
        .define_property(
            object,
            key.into(),
            PropertyDescriptor::Data(DataPropertyDescriptor {
                value: Some(Value::from_i32(42)),
                writable: Some(true),
                enumerable: None,
                configurable: None,
            }),
        )
        .unwrap();
    collect_major(&mut isolate);

    assert!(
        isolate
            .heap
            .checked_reference(pair_raw, isolate.types.accessor_pair)
            .is_err()
    );
    assert!(
        isolate
            .heap
            .checked_reference(getter_raw, isolate.types.function)
            .is_err()
    );
}

#[test]
/// An old accessor pair remembers a newly installed young callable through the pair owner card.
fn accessor_pair_update_records_old_to_young_callable_edge() {
    let mut isolate = test_isolate();
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(object);
    let key = isolate
        .intern_intrinsic_name(b"rememberedAccessor")
        .unwrap();
    isolate
        .define_property(
            object,
            key.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(isolate.realm.object_constructor.unwrap()),
                setter: None,
                enumerable: Some(false),
                configurable: Some(true),
            }),
        )
        .unwrap();
    collect_minor(&mut isolate);
    collect_minor(&mut isolate);

    let young_getter = allocate_young_test_function(&mut isolate);
    let young_getter_raw = young_getter.as_heap_ref().unwrap();
    isolate
        .define_property(
            object,
            key.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(young_getter),
                ..AccessorPropertyDescriptor::default()
            }),
        )
        .unwrap();
    isolate.heap.verify_generational_barriers().unwrap();
    collect_minor(&mut isolate);

    assert!(
        isolate
            .heap
            .checked_reference(young_getter_raw, isolate.types.function)
            .is_ok()
    );
}

/// Creates and roots one ordinary descriptor object with caller-selected own data fields.
fn descriptor_object(isolate: &mut Isolate, fields: &[(&[u8], Value)]) -> Value {
    let descriptor = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(descriptor);
    for (name, value) in fields {
        let atom = isolate.intern_intrinsic_name(name).unwrap();
        isolate
            .set_own_data_property(descriptor, atom, *value)
            .unwrap();
    }
    descriptor
}

/// Allocates an otherwise ordinary callable in Young for liveness and barrier tests.
fn allocate_young_test_function(isolate: &mut Isolate) -> Value {
    let function_type = isolate.types.function;
    let prototype = isolate.realm.function_prototype.unwrap();
    let roots = &mut VmRoots {
        fiber: &mut isolate.fiber,
        suspended_fibers: &mut isolate.suspended_fibers,
        finalization_jobs: &mut isolate.finalization_jobs,
        promise_jobs: &mut isolate.promise_jobs,
        realm: &mut isolate.realm,
        loaded_code: &mut isolate.loaded_code,
        module_graph: &mut isolate.module_graph,
    };
    isolate
        .heap
        .try_allocate_with_gc(
            function_type,
            0,
            0,
            FunctionObject {
                executable: FunctionExecutable::Native(NativeFunction::ObjectConstructor),
                prototype_or_home_object: None,
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
        .map(|function| Value::from_heap_ref(function.raw()))
        .unwrap()
}

/// Recovers the raw accessor-pair edge from the compact shared property backing.
fn accessor_pair_raw(isolate: &mut Isolate, object: Value, key: AtomId) -> RawHeapRef {
    let (_, snapshot) = isolate.object_snapshot(object).unwrap();
    let property = isolate.shapes.lookup(snapshot.shape, key).unwrap();
    assert_eq!(property.kind, PropertyKind::Accessor);
    let storage = snapshot.storage.unwrap();
    isolate.heap.with_running_scope(|scope| {
        let storage = scope.root(storage).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow(storage, isolate.types.property_storage)
                .unwrap()
                .slots[property.slot as usize]
                .as_heap_ref()
                .unwrap()
        })
    })
}

fn collect_major(isolate: &mut Isolate) {
    let mut roots = VmRoots {
        fiber: &mut isolate.fiber,
        suspended_fibers: &mut isolate.suspended_fibers,
        finalization_jobs: &mut isolate.finalization_jobs,
        promise_jobs: &mut isolate.promise_jobs,
        realm: &mut isolate.realm,
        loaded_code: &mut isolate.loaded_code,
        module_graph: &mut isolate.module_graph,
    };
    isolate.heap.collect_major(&mut roots).unwrap();
}

fn collect_minor(isolate: &mut Isolate) {
    let mut roots = VmRoots {
        fiber: &mut isolate.fiber,
        suspended_fibers: &mut isolate.suspended_fibers,
        finalization_jobs: &mut isolate.finalization_jobs,
        promise_jobs: &mut isolate.promise_jobs,
        realm: &mut isolate.realm,
        loaded_code: &mut isolate.loaded_code,
        module_graph: &mut isolate.module_graph,
    };
    isolate.heap.collect_minor(&mut roots).unwrap();
}
