use super::{fixtures::test_isolate, *};

fn proxy_call_site(isolate: &Isolate, argument_count: u32) -> CallSite {
    CallSite {
        caller_base: 0,
        destination: 0,
        callee: isolate.realm.proxy_constructor.unwrap(),
        argument_base: 0,
        argument_prefix: None,
        argument_prefix_offset: 0,
        argument_prefix_count: 0,
        argument_count,
        this_value: Value::from_immediate(Immediate::Undefined),
        new_target: isolate.realm.proxy_constructor.unwrap(),
        construct_receiver: None,
        call_site: WordOffset::new(0),
    }
}

#[test]
/// Roots both ProxyCreate inputs and preserves their exact identities through a later major GC.
fn proxy_create_payload_survives_forced_major() {
    let mut isolate = test_isolate();
    let target = isolate.create_ordinary_object().unwrap();
    let handler = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers = vec![target, handler];
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let proxy = isolate
        .create_proxy_from_site(&proxy_call_site(&isolate, 2))
        .unwrap();
    isolate.fiber.registers = vec![proxy];
    isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"collect").unwrap())
        .unwrap();
    let raw = proxy.as_heap_ref().unwrap();
    let proxy = isolate
        .heap
        .checked_reference(raw, isolate.types.proxy_object)
        .unwrap();
    let snapshot = isolate.heap.with_running_scope(|scope| {
        let proxy = scope.root(proxy).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow(proxy, isolate.types.proxy_object)
                .copied()
                .unwrap()
        })
    });
    assert_eq!((snapshot.target, snapshot.handler), (target, handler));
    assert!(isolate.is_object_value(Value::from_heap_ref(proxy.raw())));
}

#[test]
fn proxy_constructor_validates_arguments_and_has_no_default_prototype() {
    let mut isolate = test_isolate();
    let target = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers = vec![target, Value::from_immediate(Immediate::Null)];
    assert!(matches!(
        isolate.create_proxy_from_site(&proxy_call_site(&isolate, 2)),
        Err(ExecutionError::NotObject(value))
            if value.as_immediate() == Some(Immediate::Null)
    ));
    let prototype = isolate.prototype_atom().unwrap();
    assert!(
        !isolate
            .is_function_prototype_property(isolate.realm.proxy_constructor.unwrap(), prototype,)
    );
}
