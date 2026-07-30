use super::{fixtures::*, *};
use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

#[test]
/// Forces collection at every child-intrinsic allocation and then executes each published graph.
fn child_realms_have_distinct_globals_and_remain_gc_roots() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let (first_id, first_global) = isolate.create_realm().unwrap();
    let (second_id, second_global) = isolate.create_realm().unwrap();
    let (third_id, third_global) = isolate.create_realm().unwrap();
    assert_ne!(first_id, second_id);
    assert_ne!(second_id, third_id);
    assert_ne!(first_global, second_global);
    assert_ne!(second_global, third_global);
    assert_eq!(isolate.inactive_realms.len(), 3);
    isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"realm-root").unwrap())
        .unwrap();
    assert!(isolate.native_error_kind(first_global).unwrap().is_none());
    assert!(isolate.native_error_kind(second_global).unwrap().is_none());
    assert!(isolate.native_error_kind(third_global).unwrap().is_none());
    let module = arithmetic_module();
    for realm in [first_id, second_id, third_id] {
        let outcome = isolate
            .execute_in_realm(
                realm,
                &module,
                ExecutionBudget {
                    fuel: 64,
                    quantum: 64,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    }
}

#[test]
fn compiled_code_can_be_loaded_once_per_realm() {
    let mut isolate = test_isolate();
    let (realm, _) = isolate.create_realm().unwrap();
    let module = arithmetic_module();
    let first = isolate
        .execute(
            &module,
            ExecutionBudget {
                fuel: 64,
                quantum: 64,
            },
        )
        .unwrap();
    let second = isolate
        .execute_in_realm(
            realm,
            &module,
            ExecutionBudget {
                fuel: 64,
                quantum: 64,
            },
        )
        .unwrap();
    assert!(matches!(first, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    assert!(matches!(second, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    assert_eq!(isolate.loaded_code.len(), 2);
}

#[test]
fn script_var_declaration_works_for_every_dispatch_batch() {
    assert_scope_batch::<1>();
    assert_scope_batch::<2>();
    assert_scope_batch::<4>();
    assert_scope_batch::<8>();
    assert_scope_batch::<16>();
}

#[test]
fn global_lexical_access_works_for_every_dispatch_batch() {
    assert_global_lexical_batch::<1>();
    assert_global_lexical_batch::<2>();
    assert_global_lexical_batch::<4>();
    assert_global_lexical_batch::<8>();
    assert_global_lexical_batch::<16>();
}

#[test]
fn global_intrinsic_overrides_work_for_every_dispatch_batch() {
    assert_global_intrinsic_override_batch::<1>();
    assert_global_intrinsic_override_batch::<2>();
    assert_global_intrinsic_override_batch::<4>();
    assert_global_intrinsic_override_batch::<8>();
    assert_global_intrinsic_override_batch::<16>();
}

/// Proves identifier reads observe the global object's current intrinsic-valued property.
fn assert_global_intrinsic_override_batch<const N: usize>() {
    let source = r#"
function fakeObject() {}
function secondObject() {}
var global = this;
var undefinedDesc = Object.getOwnPropertyDescriptor(global, "undefined");
var nanDesc = Object.getOwnPropertyDescriptor(global, "NaN");
var infinityDesc = Object.getOwnPropertyDescriptor(global, "Infinity");
var stringIndexDesc = Object.getOwnPropertyDescriptor("foo", "0");
var stringLengthDesc = Object.getOwnPropertyDescriptor("foo", "length");
var primitiveOwnQueries = Object.hasOwn("foo", "0") &&
    Object.prototype.hasOwnProperty.call("foo", "0") &&
    Object.prototype.propertyIsEnumerable.call("foo", "0");
var sealedCandidate = { value: 1 };
var frozenCandidate = { value: 1 };
Object.seal(sealedCandidate);
Object.freeze(frozenCandidate);
var integrityQueries = Object.isSealed(sealedCandidate) &&
    !Object.isFrozen(sealedCandidate) && Object.isSealed(frozenCandidate) &&
    Object.isFrozen(frozenCandidate);
var integrityTrace = "";
var proxyIntegrityTarget = { value: 1 };
Object.freeze(proxyIntegrityTarget);
var proxyIntegrity = new Proxy(proxyIntegrityTarget, {
    getOwnPropertyDescriptor: function(target, key) {
        integrityTrace = integrityTrace + key;
        return Reflect.getOwnPropertyDescriptor(target, key);
    }
});
var proxyIntegrityQueries = Object.isFrozen(proxyIntegrity) &&
    Object.isSealed(proxyIntegrity) && integrityTrace === "valuevalue";
var constantDescriptors = undefinedDesc.writable === false && undefinedDesc.enumerable === false &&
    undefinedDesc.configurable === false && nanDesc.writable === false &&
    nanDesc.enumerable === false && nanDesc.configurable === false &&
    infinityDesc.writable === false && infinityDesc.enumerable === false &&
    infinityDesc.configurable === false && stringIndexDesc.value === "f" &&
    stringIndexDesc.writable === false && stringIndexDesc.enumerable === true &&
    stringIndexDesc.configurable === false && stringLengthDesc.value === 3 &&
    stringLengthDesc.writable === false && stringLengthDesc.enumerable === false &&
    stringLengthDesc.configurable === false && primitiveOwnQueries && integrityQueries &&
    proxyIntegrityQueries;
global.Object = fakeObject;
var memberWrite = Object === fakeObject;
Object = secondObject;
constantDescriptors && memberWrite && Object === secondObject && global.Object === secondObject;
"#;
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(700 + N as u32),
                SourceName::new("global-intrinsic-override"),
                MediaType::JavaScript,
                Arc::from(source),
            ),
            CompileOptions::default(),
        )
        .expect("global intrinsic override fixture compiles");
    let outcome = test_isolate()
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 1_024,
                quantum: 1_024,
            },
        )
        .expect("global intrinsic override fixture executes");
    assert!(
        matches!(outcome, RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)),
        "dispatch batch {N} returned {outcome:?}"
    );
}

#[test]
fn native_error_constructor_hierarchy_survives_forced_major() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &native_error_constructor_module(),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
/// Keeps the Error brand live through collection and rejects prototype-chain impersonation.
fn native_error_brand_survives_forced_major_and_cannot_be_forged() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let error = isolate
        .create_native_error(NativeErrorKind::Range, None)
        .unwrap();
    isolate.fiber.registers.push(error);
    isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"collect").unwrap())
        .unwrap();
    assert_eq!(
        isolate.native_error_kind(error).unwrap(),
        Some(NativeErrorKind::Range)
    );

    let prototype = isolate
        .realm
        .error_intrinsics
        .get(NativeErrorKind::Error)
        .prototype
        .unwrap();
    let fake = isolate
        .create_ordinary_object_with_prototype(prototype)
        .unwrap();
    assert_eq!(isolate.native_error_kind(fake).unwrap(), None);
}

#[test]
fn native_function_intrinsics_survive_forced_major_before_forwarding() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &function_prototype_call_module(),
            ExecutionBudget {
                fuel: 7,
                quantum: 7,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

#[test]
fn boxed_number_data_and_properties_survive_forced_major_allocations() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let prototype = isolate.realm.number_prototype.unwrap();
    let boxed = isolate
        .allocate_number_object(Value::from_i32(-7), prototype, AllocationSpace::Young)
        .unwrap();
    isolate.fiber.registers.push(boxed);
    let key = isolate.intern_intrinsic_name(b"field").unwrap();
    isolate
        .set_own_data_property(boxed, key, Value::from_i32(11))
        .unwrap();
    assert_eq!(isolate.this_number_value(boxed).unwrap().as_i32(), Some(-7));
    assert_eq!(
        isolate
            .get_data_property(boxed, key)
            .unwrap()
            .unwrap()
            .as_i32(),
        Some(11)
    );
    assert_eq!(
        isolate.object_snapshot(boxed).unwrap().1.prototype,
        prototype
    );
}

#[test]
/// Keeps Symbol wrapper data and its ordinary property backing alive across a full collection.
fn boxed_symbol_data_and_properties_survive_forced_major_allocations() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let symbol = isolate.allocate_symbol(None).unwrap();
    let boxed = isolate.box_symbol(symbol).unwrap();
    isolate.fiber.registers.push(boxed);
    let key = isolate.intern_intrinsic_name(b"field").unwrap();
    isolate
        .set_own_data_property(boxed, key, Value::from_i32(11))
        .unwrap();
    assert_eq!(isolate.symbol_value_of(boxed).unwrap(), symbol);
    assert_eq!(
        isolate
            .get_data_property(boxed, key)
            .unwrap()
            .and_then(Value::as_i32),
        Some(11)
    );
    assert_eq!(
        isolate.object_snapshot(boxed).unwrap().1.prototype,
        isolate.realm.symbol_prototype.unwrap()
    );
}

#[test]
/// Keeps a pending Symbol description live before allocation and through a later full major.
fn symbol_description_survives_forced_major_allocations() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let description = isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"description").unwrap())
        .unwrap();
    let symbol = isolate.allocate_symbol(Some(description)).unwrap();
    isolate.fiber.registers.push(symbol);
    isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"collect").unwrap())
        .unwrap();
    let raw = symbol.as_heap_ref().unwrap();
    let symbol = isolate
        .heap
        .checked_reference(raw, isolate.types.symbol)
        .unwrap();
    let description = isolate.heap.with_running_scope(|scope| {
        let symbol = scope.root(symbol).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow(symbol, isolate.types.symbol)
                .map(|symbol| symbol.description.unwrap())
                .unwrap()
        })
    });
    let mut units = Vec::new();
    isolate
        .append_primitive_string_units(description, &mut units)
        .unwrap();
    assert_eq!(units, "description".encode_utf16().collect::<Vec<_>>());
}

#[test]
/// A live Symbol property edge preserves identity, while deletion releases that exact edge.
fn symbol_property_key_identity_tracks_property_liveness_across_forced_major() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let object = isolate.create_ordinary_object().unwrap();
    isolate.fiber.registers.push(object);
    let symbol = isolate.allocate_symbol(None).unwrap();
    let symbol_raw = symbol.as_heap_ref().unwrap();
    let key = isolate.property_key(symbol).unwrap();

    isolate
        .set_own_data_property(object, key, Value::from_i32(42))
        .unwrap();
    isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"collect").unwrap())
        .unwrap();
    assert_eq!(
        isolate.get_data_property(object, key).unwrap(),
        Some(Value::from_i32(42))
    );
    let snapshot = isolate.object_snapshot(object).unwrap().1;
    assert_eq!(
        isolate
            .symbol_property_key_value(snapshot, key.symbol().unwrap())
            .unwrap(),
        Some(symbol)
    );

    assert!(isolate.delete_own_data_property(object, key).unwrap());
    isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"reclaim").unwrap())
        .unwrap();
    assert!(
        isolate
            .heap
            .checked_reference(symbol_raw, isolate.types.symbol)
            .is_err()
    );
}

#[test]
fn bound_function_payload_and_name_survive_forced_major_allocations() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &bound_function_call_module(),
            ExecutionBudget {
                fuel: 16,
                quantum: 16,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

#[test]
fn array_identity_and_property_storage_survive_forced_major_allocations() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &array_push_module(),
            ExecutionBudget {
                fuel: 8,
                quantum: 8,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(2)));
}

#[test]
/// Forces collection between loaded literals so pending and published caches must both trace.
fn loaded_string_constants_survive_forced_major_during_module_load() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &string_constant_root_module(),
            ExecutionBudget {
                fuel: 4,
                quantum: 4,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::False)
    ));
}

#[test]
fn isolate_owns_the_atom_table_created_from_mandatory_host_config() {
    let mut isolate = test_isolate();
    let mandatory_entries = isolate.atoms().stats().entries;
    let atom = isolate
        .atoms_mut()
        .try_intern(JsString::try_from_str("property").unwrap())
        .unwrap();

    assert_eq!(atom.index() as usize, mandatory_entries);
    assert_eq!(isolate.atoms().get(atom).unwrap().len(), 8);
    assert_eq!(isolate.atoms().stats().entries, mandatory_entries + 1);
}

#[test]
fn primitive_realm_intrinsics_use_stable_non_writable_slots() {
    let mut isolate = test_isolate();
    let infinity = isolate
        .atoms
        .try_intern(JsString::try_from_str("Infinity").unwrap())
        .unwrap();
    let slot = isolate.realm.resolve_intrinsic(infinity).unwrap();

    assert_eq!(
        isolate
            .scope_value(ScopeResolution {
                atom: infinity,
                lexical_slot: None,
                intrinsic_slot: Some(slot),
                global_slot: None,
            })
            .unwrap()
            .and_then(Value::as_f64),
        Some(f64::INFINITY)
    );
    assert_eq!(
        isolate.realm.set_intrinsic(slot, Value::from_i32(1)),
        Err(ExecutionError::ReadOnlyBinding(infinity))
    );
}

#[test]
fn var_declaration_does_not_publish_over_primitive_realm_intrinsics() {
    let mut isolate = test_isolate();
    let infinity = isolate
        .atoms
        .try_intern(JsString::try_from_str("Infinity").unwrap())
        .unwrap();
    isolate
        .declare_scope_resolution(ScopeResolution {
            atom: infinity,
            lexical_slot: None,
            intrinsic_slot: isolate.realm.resolve_intrinsic(infinity),
            global_slot: None,
        })
        .unwrap();
    assert!(isolate.realm.global_bindings.is_empty());
}

#[test]
/// Proves sparse atom publication resolves through stable slots without binding-order scans.
fn realm_global_slots_are_stable_and_atom_indexed() {
    let mut isolate = test_isolate();
    let unused = isolate
        .atoms
        .try_intern(JsString::try_from_str("unused-property-name").unwrap())
        .unwrap();
    let first = isolate
        .atoms
        .try_intern(JsString::try_from_str("first-global").unwrap())
        .unwrap();
    let second = isolate
        .atoms
        .try_intern(JsString::try_from_str("second-global").unwrap())
        .unwrap();

    isolate.realm.set(first, Value::from_i32(1)).unwrap();
    let first_slot = isolate.realm.resolve(first).unwrap();
    isolate.realm.set(second, Value::from_i32(2)).unwrap();
    isolate.realm.set(first, Value::from_i32(3)).unwrap();

    assert!(isolate.realm.resolve(unused).is_none());
    assert_eq!(isolate.realm.resolve(first), Some(first_slot));
    assert_eq!(
        isolate.realm.get_slot(first_slot).and_then(Value::as_i32),
        Some(3)
    );
    assert_eq!(
        isolate
            .realm
            .resolve(second)
            .and_then(|slot| isolate.realm.get_slot(slot))
            .and_then(Value::as_i32),
        Some(2)
    );
    assert_eq!(
        isolate.realm.global_bindings[first_slot.index()].name,
        first
    );
    assert_eq!(
        isolate.realm.global_slots_by_atom.len(),
        second.index() as usize + 1
    );
}

#[test]
fn publishing_lower_atom_global_preserves_higher_atom_mapping() {
    let mut isolate = test_isolate();
    let lower = isolate.intern_intrinsic_name(b"lower-slot").unwrap();
    let higher = isolate.intern_intrinsic_name(b"higher-slot").unwrap();

    isolate.realm.set(higher, Value::from_i32(2)).unwrap();
    isolate.realm.set(lower, Value::from_i32(1)).unwrap();

    let higher_slot = isolate.realm.resolve(higher).unwrap();
    assert_eq!(
        isolate.realm.get_slot(higher_slot),
        Some(Value::from_i32(2))
    );
}

#[test]
/// Confirms loaded scope operands self-resolve once and retain the stable realm slot.
fn loaded_scope_resolution_caches_a_published_global_slot() {
    let mut isolate = test_isolate();
    let module = scoped_var_module();
    let code = isolate.load_module(&module).unwrap();
    assert!(
        isolate.loaded_code[code.index()].scope_resolutions[0]
            .global_slot
            .is_none()
    );

    let outcome = isolate
        .execute_loaded(
            code,
            ExecutionBudget {
                fuel: 16,
                quantum: 16,
            },
        )
        .unwrap();

    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(7)));
    let resolution = isolate.loaded_code[code.index()].scope_resolutions[0];
    let slot = resolution.global_slot.expect("executed binding is cached");
    assert_eq!(isolate.realm.resolve(resolution.atom), Some(slot));
    assert_eq!(
        isolate.realm.get_slot(slot).and_then(Value::as_i32),
        Some(7)
    );
}

#[test]
fn for_in_iterator_and_returned_keys_survive_forced_major() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &for_in_module(),
            ExecutionBudget {
                fuel: 32,
                quantum: 32,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(2)));
}

#[test]
fn captured_environment_survives_forced_major_allocation() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute_with_batch::<8>(
            &captured_environment_module(),
            ExecutionBudget {
                fuel: 32,
                quantum: 32,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
}

#[test]
/// Forced major collections cover closure prototype creation and receiver chain publication.
fn instanceof_prototype_chain_survives_forced_major() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &instanceof_module(),
            ExecutionBudget {
                fuel: 6,
                quantum: 6,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Completed(value)
            if value.as_immediate() == Some(Immediate::True)
    ));
}

#[test]
fn construct_receiver_stays_rooted_across_forced_major_property_allocation() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let outcome = isolate
        .execute(
            &construct_module(),
            ExecutionBudget {
                fuel: 32,
                quantum: 32,
            },
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
}

#[test]
/// Confirms verifier-produced stack depths become allocation reservations before dispatch.
fn entry_reserves_verified_execution_windows() {
    let layout = FunctionLayout {
        register_count: 2,
        max_handler_depth: 3,
        max_completion_depth: 4,
        ..FunctionLayout::default()
    };
    let mut isolate = test_isolate();
    let module = state_module(FunctionKind::Module, layout);
    let code = isolate.load_module(&module).unwrap();
    isolate.enter(code, FunctionId::new(0)).unwrap();

    assert!(isolate.fiber.frames.capacity() >= 1);
    assert!(isolate.fiber.registers.capacity() >= 2);
    assert!(isolate.fiber.handlers.capacity() >= 3);
    assert!(isolate.fiber.completions.capacity() >= 4);
    assert_eq!(
        isolate.fiber.frames[0].strictness,
        FunctionStrictness::Strict
    );
}

#[test]
fn entry_completion_depth_respects_the_host_stack_limit() {
    let layout = FunctionLayout {
        register_count: 1,
        max_completion_depth: 2,
        ..FunctionLayout::default()
    };
    let mut isolate = test_isolate();
    isolate.stack_limits = StackLimits::new(64, 4_096).with_max_completions(1);
    let module = state_module(FunctionKind::Script, layout);
    let code = isolate.load_module(&module).unwrap();

    assert_eq!(
        isolate.enter(code, FunctionId::new(0)),
        Err(ExecutionError::CompletionStackLimit {
            limit: 1,
            requested: 2,
        })
    );
}

#[test]
/// Exercises every fiber-owned GC edge with a tracer that simulates object relocation.
fn fiber_trace_roots_visits_execution_state_without_native_stack_scanning() {
    let layout = FunctionLayout {
        register_count: 1,
        max_handler_depth: 1,
        max_completion_depth: 2,
        ..FunctionLayout::default()
    };
    let mut isolate = test_isolate();
    let module = state_module(FunctionKind::Script, layout);
    let code = isolate.load_module(&module).unwrap();
    isolate.enter(code, FunctionId::new(0)).unwrap();
    let raw = RawHeapRef::new(16).expect("valid logical address");
    isolate.fiber.registers[0] = Value::from_heap_ref(raw);
    let global = isolate
        .atoms
        .try_intern(JsString::try_from_str("global").unwrap())
        .unwrap();
    isolate
        .realm
        .set(global, Value::from_heap_ref(raw))
        .unwrap();
    let frame = isolate.fiber.frames.last_mut().expect("entry frame exists");
    // SAFETY: this test never dereferences the synthetic environment reference; it only checks
    // that the exact tracing contract rewrites every encoded fiber edge.
    frame.environment = Some(unsafe { GcRef::from_raw_unchecked(raw) });
    frame.return_register = Some(RegisterId::new(0));
    frame.this_value = Value::from_heap_ref(raw);
    frame.new_target = Value::from_heap_ref(raw);
    isolate.fiber.handlers.push(ActiveHandler {
        handler_index: 0,
        frame_depth: 1,
        environment_depth: 1,
    });
    isolate
        .fiber
        .completions
        .push_record(CompletionRecord::return_value(Value::from_heap_ref(raw)))
        .unwrap();
    isolate
        .fiber
        .completions
        .push_record(CompletionRecord::throw(Value::from_heap_ref(raw)))
        .unwrap();
    isolate.fiber.pending_exception = Some(Value::from_heap_ref(raw));
    let mut tracer = RewritingTracer;

    isolate.trace_roots(&mut tracer);

    let rewritten = RawHeapRef::new(32).expect("valid logical address");
    assert_eq!(isolate.fiber.registers[0].as_heap_ref(), Some(rewritten));
    let frame = isolate.fiber.frames.last().expect("entry frame exists");
    assert_eq!(frame.environment.map(GcRef::raw), Some(rewritten));
    assert_eq!(frame.this_value.as_heap_ref(), Some(rewritten));
    assert_eq!(frame.new_target.as_heap_ref(), Some(rewritten));
    assert!(matches!(
        isolate.fiber.completions.record(0),
        Some(record)
            if record.kind() == CompletionKind::Return
                && record.value().and_then(Value::as_heap_ref) == Some(rewritten)
    ));
    assert_eq!(
        isolate.fiber.pending_exception.and_then(Value::as_heap_ref),
        Some(rewritten)
    );
    assert!(matches!(
        isolate.fiber.completions.record(1),
        Some(record)
            if record.kind() == CompletionKind::Throw
                && record.value().and_then(Value::as_heap_ref) == Some(rewritten)
    ));
    assert_eq!(
        isolate
            .realm
            .resolve(global)
            .and_then(|slot| isolate.realm.get_slot(slot))
            .and_then(Value::as_heap_ref),
        Some(rewritten)
    );
}

struct RewritingTracer;

impl Tracer for RewritingTracer {
    fn trace_value(&mut self, value: &mut Value) {
        if let Some(reference) = value.as_heap_ref() {
            *value = Value::from_heap_ref(rewrite(reference));
        }
    }

    fn trace_raw_heap_ref(&mut self, reference: &mut RawHeapRef) {
        *reference = rewrite(*reference);
    }

    fn trace_weak_raw_heap_ref(&mut self, reference: &mut Option<RawHeapRef>) {
        *reference = reference.map(rewrite);
    }

    fn trace_ephemeron(&mut self, key: &mut Option<RawHeapRef>, value: &mut Value) {
        *key = key.map(rewrite);
        self.trace_value(value);
    }

    fn trace_finalization(&mut self, target: &mut Option<RawHeapRef>, held_value: &mut Value) {
        *target = target.map(rewrite);
        self.trace_value(held_value);
    }
}

fn rewrite(reference: RawHeapRef) -> RawHeapRef {
    RawHeapRef::new(reference.offset() + 16).expect("test offset stays non-zero")
}
