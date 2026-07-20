use super::fixtures::*;
use super::*;

#[test]
fn for_in_iterator_loop_is_stable_for_every_dispatch_batch() {
    assert_for_in_batch::<1>();
    assert_for_in_batch::<2>();
    assert_for_in_batch::<4>();
    assert_for_in_batch::<8>();
    assert_for_in_batch::<16>();
}

#[test]
fn accessor_for_in_metadata_is_stable_for_every_dispatch_batch() {
    assert_accessor_for_in_batch::<1>();
    assert_accessor_for_in_batch::<2>();
    assert_accessor_for_in_batch::<4>();
    assert_accessor_for_in_batch::<8>();
    assert_accessor_for_in_batch::<16>();
}

/// Runs kind-neutral accessor enumeration before and after an own shadow tombstone.
fn assert_accessor_for_in_batch<const N: usize>() {
    for delete_shadow in [false, true] {
        let mut isolate = test_isolate();
        let getter = isolate.realm.object_constructor.unwrap();
        let shadowed = isolate.intern_intrinsic_name(b"shadowed").unwrap();
        let visible = isolate.intern_intrinsic_name(b"visible").unwrap();
        let prototype = isolate.create_ordinary_object().unwrap();
        define_enumeration_accessor(&mut isolate, prototype, shadowed, getter, true);
        let object = isolate
            .create_ordinary_object_with_prototype(prototype)
            .unwrap();
        define_enumeration_accessor(&mut isolate, object, shadowed, getter, false);
        define_enumeration_accessor(&mut isolate, object, visible, getter, true);
        if delete_shadow {
            assert!(isolate.delete_own_data_property(object, shadowed).unwrap());
        }
        let source = isolate.intern_intrinsic_name(b"source").unwrap();
        isolate.realm.set(source, object).unwrap();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute_with_batch::<N>(
                &accessor_for_in_module(),
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        let expected = if delete_shadow { 2 } else { 1 };
        assert!(
            matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(expected))
        );
    }
}

/// Defines one configurable accessor whose getter must never run during key enumeration.
fn define_enumeration_accessor(
    isolate: &mut Isolate,
    object: Value,
    key: AtomId,
    getter: Value,
    enumerable: bool,
) {
    isolate
        .define_property(
            object,
            key.into(),
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor {
                getter: Some(getter),
                setter: None,
                enumerable: Some(enumerable),
                configurable: Some(true),
            }),
        )
        .unwrap();
}

/// Builds a global-source for-in loop that returns the complete iterator key count.
fn accessor_for_in_module() -> CompiledModule {
    let span = SourceSpan { start: 0, end: 1 };
    let mut builder = BytecodeBuilder::with_capacity(16, 2);
    let condition = builder.new_label().unwrap();
    let end = builder.new_label().unwrap();
    builder.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
    builder
        .emit(Opcode::CreateForInIterator, &[1, 0], span)
        .unwrap();
    builder.emit(Opcode::LoadUndefined, &[2], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[3, 0], span).unwrap();
    builder.emit(Opcode::LoadImmediate, &[4, 1], span).unwrap();
    builder.bind_label(condition).unwrap();
    builder.emit(Opcode::ForInNext, &[5, 1], span).unwrap();
    builder.emit(Opcode::StrictEqual, &[6, 5, 2], span).unwrap();
    builder
        .emit_jump_if_true(RegisterId::new(6), end, span)
        .unwrap();
    builder.emit(Opcode::Add, &[3, 3, 4], span).unwrap();
    builder.emit_jump(condition, span).unwrap();
    builder.bind_label(end).unwrap();
    builder.emit(Opcode::Return, &[3], span).unwrap();
    let (bytecode, source_map, register_count) = builder.finish().unwrap();
    let metadata = FunctionMetadata {
        layout: FunctionLayout {
            register_count,
            ..FunctionLayout::default()
        },
        source_map,
        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
    };
    CompiledModule::new(
        Arc::from("accessor for-in"),
        Vec::new(),
        vec![Arc::from("source")],
        vec![CompiledFunctionTemplate::new(
            FunctionId::new(0),
            bytecode,
            metadata,
        )],
        FunctionId::new(0),
    )
    .unwrap()
}

#[test]
fn logical_short_circuit_preserves_operands_for_every_dispatch_batch() {
    assert_logical_batch::<1>();
    assert_logical_batch::<2>();
    assert_logical_batch::<4>();
    assert_logical_batch::<8>();
    assert_logical_batch::<16>();
}

#[test]
fn switch_dispatch_chain_is_stable_for_every_dispatch_batch() {
    assert_switch_batch::<1>();
    assert_switch_batch::<2>();
    assert_switch_batch::<4>();
    assert_switch_batch::<8>();
    assert_switch_batch::<16>();
}

#[test]
fn catch_dispatch_and_cross_frame_throw_work_for_every_dispatch_batch() {
    assert_catch_batch::<1>();
    assert_catch_batch::<2>();
    assert_catch_batch::<4>();
    assert_catch_batch::<8>();
    assert_catch_batch::<16>();
}
