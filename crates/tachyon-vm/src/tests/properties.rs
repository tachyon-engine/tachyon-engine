use super::{fixtures::*, *};

#[test]
fn array_push_updates_existing_indexed_storage_and_length() {
    let mut isolate = test_isolate();
    let prototype = isolate.realm.array_prototype.unwrap();
    let array = isolate
        .create_array_object_with_prototype(prototype)
        .unwrap();
    let zero = isolate.property_key_atom(Value::from_i32(0)).unwrap();
    isolate
        .set_own_data_property(array, zero, Value::from_i32(1))
        .unwrap();
    let length = isolate.length_atom().unwrap();
    isolate
        .set_own_data_property(array, length, Value::from_i32(1))
        .unwrap();
    isolate.fiber.registers = vec![array, Value::from_i32(2), Value::from_i32(3)];
    let result = isolate
        .array_push(&CallSite {
            caller_base: 0,
            destination: 0,
            callee: isolate.realm.array_push.unwrap(),
            argument_base: 1,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 2,
            this_value: array,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: WordOffset::new(0),
        })
        .unwrap();
    assert_eq!(result.as_i32(), Some(3));
    assert_eq!(
        isolate
            .get_data_property(array, length)
            .unwrap()
            .unwrap()
            .as_i32(),
        Some(3)
    );
    let separator = isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"-").unwrap())
        .unwrap();
    isolate.fiber.registers.push(separator);
    let joined = isolate
        .array_join(&CallSite {
            caller_base: 0,
            destination: 0,
            callee: isolate.realm.array_join.unwrap(),
            argument_base: 3,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 1,
            this_value: array,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: WordOffset::new(0),
        })
        .unwrap();
    let expected = isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"1-2-3").unwrap())
        .unwrap();
    assert!(isolate.strict_equal_values(joined, expected).unwrap());
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
                .function_prototype
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

fn collect_major(isolate: &mut Isolate) {
    let mut roots = VmRoots {
        fiber: &mut isolate.fiber,
        finalization_jobs: &mut isolate.finalization_jobs,
        realm: &mut isolate.realm,
        loaded_code: &mut isolate.loaded_code,
    };
    isolate.heap.collect_major(&mut roots).unwrap();
}
