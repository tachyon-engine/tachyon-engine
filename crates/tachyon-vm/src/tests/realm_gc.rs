use super::{fixtures::*, *};

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
fn array_flat_work_stack_survives_forced_major_allocations() {
    let mut isolate = test_isolate();
    let prototype = isolate.realm.array_prototype.unwrap();
    let inner = isolate
        .create_array_object_with_prototype(prototype)
        .unwrap();
    let outer = isolate
        .create_array_object_with_prototype(prototype)
        .unwrap();
    let zero = isolate.safe_integer_property_atom(0).unwrap();
    let length = isolate.length_atom().unwrap();
    isolate
        .set_own_data_property(inner, zero, Value::from_i32(42))
        .unwrap();
    isolate
        .set_own_data_property(inner, length, Value::from_i32(1))
        .unwrap();
    isolate.set_own_data_property(outer, zero, inner).unwrap();
    isolate
        .set_own_data_property(outer, length, Value::from_i32(1))
        .unwrap();
    isolate.fiber.registers = vec![outer, Value::from_i32(1)];
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let result = isolate
        .array_flat(&CallSite {
            caller_base: 0,
            destination: 0,
            callee: isolate.realm.array_flat.unwrap(),
            argument_base: 1,
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 1,
            this_value: outer,
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: WordOffset::new(0),
        })
        .unwrap();
    assert_eq!(
        isolate
            .get_data_property(result, zero)
            .unwrap()
            .and_then(Value::as_i32),
        Some(42)
    );
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
    isolate.fiber.completions.extend([
        Completion::Return(Value::from_heap_ref(raw)),
        Completion::Throw(Value::from_heap_ref(raw)),
    ]);
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
        isolate.fiber.completions[0],
        Completion::Return(value) if value.as_heap_ref() == Some(rewritten)
    ));
    assert_eq!(
        isolate.fiber.pending_exception.and_then(Value::as_heap_ref),
        Some(rewritten)
    );
    assert!(matches!(
        isolate.fiber.completions[1],
        Completion::Throw(value) if value.as_heap_ref() == Some(rewritten)
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
