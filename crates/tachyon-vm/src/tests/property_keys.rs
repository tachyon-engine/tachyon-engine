use super::{fixtures::*, *};

#[test]
fn deleted_properties_reinsert_at_the_end_for_every_dispatch_batch() {
    assert_property_reinsert_batch::<1>();
    assert_property_reinsert_batch::<2>();
    assert_property_reinsert_batch::<4>();
    assert_property_reinsert_batch::<8>();
    assert_property_reinsert_batch::<16>();
}

#[test]
fn readded_symbol_moves_after_surviving_symbols() {
    let mut isolate = test_isolate();
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(object);
    let first = isolate.allocate_symbol(None).unwrap();
    isolate.fiber.registers.push(first);
    let second = isolate.allocate_symbol(None).unwrap();
    isolate.fiber.registers.push(second);
    let first_key = isolate.property_key(first).unwrap();
    let second_key = isolate.property_key(second).unwrap();
    isolate
        .set_own_data_property(object, first_key, Value::from_i32(1))
        .unwrap();
    isolate
        .set_own_data_property(object, second_key, Value::from_i32(2))
        .unwrap();
    assert!(isolate.delete_own_data_property(object, first_key).unwrap());
    isolate
        .set_own_data_property(object, first_key, Value::from_i32(3))
        .unwrap();

    let (_, snapshot) = isolate.object_snapshot(object).unwrap();
    assert_eq!(
        isolate
            .shapes
            .own_keys(snapshot.shape)
            .unwrap()
            .collect::<Vec<_>>(),
        [second_key, first_key]
    );
}

#[test]
fn ordinary_own_keys_partition_indices_strings_and_symbols() {
    let mut isolate = test_isolate();
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(object);
    let alpha = isolate.intern_intrinsic_name(b"alpha").unwrap();
    let nine = isolate.intern_intrinsic_name(b"9").unwrap();
    let two = isolate.intern_intrinsic_name(b"2").unwrap();
    let ten = isolate.intern_intrinsic_name(b"10").unwrap();
    let leading = isolate.intern_intrinsic_name(b"01").unwrap();
    let max_index = isolate.intern_intrinsic_name(b"4294967294").unwrap();
    let non_index = isolate.intern_intrinsic_name(b"4294967295").unwrap();
    let symbol = isolate.allocate_symbol(None).unwrap();
    isolate.fiber.registers.push(symbol);
    let symbol_key = isolate.property_key(symbol).unwrap();
    for key in [
        alpha.into(),
        nine.into(),
        symbol_key,
        two.into(),
        ten.into(),
        leading.into(),
        max_index.into(),
        non_index.into(),
    ] {
        isolate
            .set_own_data_property(object, key, Value::from_i32(1))
            .unwrap();
    }

    let (_, snapshot) = isolate.object_snapshot(object).unwrap();
    assert_eq!(
        isolate
            .ordinary_own_property_keys(object, snapshot)
            .unwrap()
            .collect::<Vec<_>>(),
        [
            two.into(),
            nine.into(),
            ten.into(),
            max_index.into(),
            alpha.into(),
            leading.into(),
            non_index.into(),
            symbol_key,
        ]
    );
}
