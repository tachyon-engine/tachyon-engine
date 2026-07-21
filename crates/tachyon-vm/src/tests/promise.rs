use tachyon_gc::{ForcedCollectionMode, SPAN_SIZE_BYTES};

use super::super::*;

fn test_isolate() -> Isolate {
    Isolate::new(IsolateConfig::new(
        AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
        HeapLimit::new(16 * SPAN_SIZE_BYTES),
        StackLimits::new(64, 4_096),
        RealmLimits::new(64, 1_024).with_max_shapes(384),
    ))
    .unwrap()
}

#[test]
fn promise_jobs_move_through_the_traced_active_slot_in_fifo_order() {
    let mut queue = PromiseJobQueue::new();
    queue.push(PromiseJob::Reaction {
        handler: Value::from_i32(1),
        capability: Value::from_i32(2),
        argument: Value::from_i32(3),
        rejected: false,
    });
    queue.push(PromiseJob::Thenable {
        promise: Value::from_i32(4),
        thenable: Value::from_i32(5),
        then: Value::from_i32(6),
    });
    assert_eq!(queue.len(), 2);
    assert!(matches!(
        queue.begin_next(),
        Some(PromiseJob::Reaction { argument, .. }) if argument.as_i32() == Some(3)
    ));
    assert_eq!(queue.len(), 1);
    queue.finish_active();
    assert!(matches!(
        queue.begin_next(),
        Some(PromiseJob::Thenable { then, .. }) if then.as_i32() == Some(6)
    ));
}

#[test]
fn resolving_functions_share_the_first_call_guard_across_forced_major() {
    let mut isolate = test_isolate();
    let promise = isolate
        .create_promise(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
        )
        .unwrap();
    let arguments = isolate
        .create_promise_capability_arguments(promise)
        .unwrap();
    let arguments = isolate.native_call_state_snapshot(arguments).unwrap();
    let resolve = arguments.values[0];
    let reject = arguments.values[1];
    isolate.fiber.registers = vec![promise, resolve, reject];
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let FunctionExecutable::PromiseResolver {
        cell,
        reject: false,
    } = isolate.resolve_function_object(resolve).unwrap().executable
    else {
        panic!("resolve capability must use the shared cell")
    };
    let claimed = isolate.claim_promise_resolver(cell).unwrap().unwrap();
    isolate
        .settle_promise(claimed, PromiseState::Fulfilled, Value::from_i32(7))
        .unwrap();
    let FunctionExecutable::PromiseResolver { cell, reject: true } =
        isolate.resolve_function_object(reject).unwrap().executable
    else {
        panic!("reject capability must use the shared cell")
    };
    assert!(isolate.claim_promise_resolver(cell).unwrap().is_none());
    let snapshot = isolate.promise_snapshot(promise).unwrap();
    assert_eq!(snapshot.state, PromiseState::Fulfilled);
    assert_eq!(snapshot.result.as_i32(), Some(7));
}

#[test]
fn promise_species_accessor_descriptor_round_trips() {
    let mut isolate = test_isolate();
    let constructor = isolate.realm.promise_constructor.unwrap();
    let species = isolate.realm.well_known_symbols.species.unwrap();
    let key = isolate.property_key(species).unwrap();
    let descriptor = isolate
        .complete_own_property_descriptor(constructor, key)
        .unwrap()
        .unwrap();
    let PropertyDescriptor::Accessor(accessor) = descriptor else {
        panic!("Promise @@species must remain an accessor")
    };
    assert!(accessor.getter.is_some_and(|getter| {
        matches!(
            isolate.resolve_function_object(getter).unwrap().executable,
            FunctionExecutable::Native(NativeFunction::SpeciesGetter)
        )
    }));
    assert_eq!(
        accessor.setter.and_then(Value::as_immediate),
        Some(Immediate::Undefined)
    );
    assert_eq!(accessor.enumerable, Some(false));
    assert_eq!(accessor.configurable, Some(true));

    let result = isolate.create_ordinary_object().unwrap();
    isolate
        .materialize_property_descriptor(result, descriptor)
        .unwrap();
}
