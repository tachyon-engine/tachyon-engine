use super::*;

#[test]
/// Fixed-seed graph churn crosses every Phase 1B collection and lifetime boundary repeatedly.
fn randomized_forced_collection_stress_preserves_exact_graph_contracts() {
    const STRESS_STEPS: usize = 96;
    const STRESS_SEED: u64 = 0x6a09_e667_f3bc_c909;

    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let weak_type = types.try_register::<WeakHolder>("WeakHolder").unwrap();
    let ephemeron_type = types
        .try_register::<EphemeronHolder>("EphemeronHolder")
        .unwrap();
    let finalization_type = types
        .try_register::<FinalizationHolder>("FinalizationHolder")
        .unwrap();
    let fanout_type = types.try_register::<Fanout>("Fanout").unwrap();
    let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100).unwrap();
    let mut heap = Heap::with_trigger_config(HeapLimit::new(128 * SPAN_SIZE_BYTES), types, config);
    let mut roots = StressRoots::new();

    let key = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    let ephemeron_value = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    let weak_target = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    let finalization_target = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    let held = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    roots
        .nodes
        .extend([key, ephemeron_value, weak_target, finalization_target, held]);
    let weak_holder = heap
        .try_allocate(
            weak_type,
            0,
            0,
            WeakHolder {
                target: WeakGcRef::new(weak_target),
            },
            AllocationSpace::Old,
        )
        .unwrap();
    let ephemeron_holder = heap
        .try_allocate(
            ephemeron_type,
            0,
            0,
            EphemeronHolder {
                entry: Ephemeron::new(key, Value::from_heap_ref(ephemeron_value.raw())),
            },
            AllocationSpace::Old,
        )
        .unwrap();
    let finalization_holder = heap
        .try_allocate(
            finalization_type,
            0,
            0,
            FinalizationHolder {
                registration: FinalizationRegistration::new(
                    finalization_target,
                    Value::from_heap_ref(held.raw()),
                ),
            },
            AllocationSpace::Old,
        )
        .unwrap();
    roots.stable.extend([
        Value::from_heap_ref(key.raw()),
        Value::from_heap_ref(weak_holder.raw()),
        Value::from_heap_ref(ephemeron_holder.raw()),
        Value::from_heap_ref(finalization_holder.raw()),
    ]);
    roots.nodes.clear();

    heap.set_forced_collection_mode(ForcedCollectionMode::Major);
    let first_cycle_node = heap
        .try_allocate_with_gc(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
            &mut roots,
        )
        .unwrap();
    roots.nodes.push(first_cycle_node);
    assert_eq!(heap.finalization_queue_stats().pending, 1);

    let weak_was_cleared = heap.with_running_scope(|scope| {
        let holder = scope.root(weak_holder).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow(holder, weak_type)
                .unwrap()
                .target
                .get()
                .is_none()
        })
    });
    assert!(weak_was_cleared);
    assert!(heap.verify_reference(ephemeron_value.raw(), None).is_ok());
    assert!(heap.verify_reference(held.raw(), None).is_ok());

    let mut rng = DeterministicRng(STRESS_SEED);
    let attempts_before = heap.trigger_stats();
    for _ in 0..STRESS_STEPS {
        let mode = if rng.next().is_multiple_of(3) {
            ForcedCollectionMode::Major
        } else {
            ForcedCollectionMode::Minor
        };
        heap.set_forced_collection_mode(mode);
        let source = roots.nodes[(rng.next() as usize) % roots.nodes.len()];
        let allocated = heap
            .try_allocate_with_gc(
                node_type,
                0,
                0,
                ChainNode { next: Some(source) },
                AllocationSpace::Young,
                &mut roots,
            )
            .unwrap();
        heap.with_running_scope(|scope| {
            let source = scope.root(source).unwrap();
            let allocated_local = scope.root(allocated).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc.borrow_mut(source, node_type).unwrap().next = Some(allocated);
            });
            scope.write_barrier(source, allocated_local).unwrap();
        });
        if roots.nodes.len() == roots.nodes.capacity() {
            let remove = (rng.next() as usize) % roots.nodes.len();
            roots.nodes.swap_remove(remove);
        }
        roots.nodes.push(allocated);
        for root in &roots.nodes {
            assert!(heap.verify_reference(root.raw(), None).is_ok());
        }
        heap.verify_generational_barriers().unwrap();
    }
    let attempts_after = heap.trigger_stats();
    assert!(attempts_after.minor_attempts > attempts_before.minor_attempts);
    assert!(attempts_after.major_attempts > attempts_before.major_attempts);

    let anchor = roots.nodes[0];
    heap.set_forced_collection_mode(ForcedCollectionMode::Minor);
    for _ in 0..2 {
        heap.try_allocate_with_gc(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
            &mut roots,
        )
        .unwrap();
    }
    assert_eq!(
        heap.span_table().reference_space(anchor.raw()).unwrap(),
        crate::table::ReferenceSpace::OldSmall
    );

    heap.set_forced_collection_mode(ForcedCollectionMode::Major);
    let released_span = heap
        .try_allocate_with_gc(
            fanout_type,
            0,
            0,
            Fanout { edges: [None; 300] },
            AllocationSpace::Young,
            &mut roots,
        )
        .unwrap()
        .raw()
        .span_id();
    let reused_span = heap
        .try_allocate_with_gc(
            fanout_type,
            0,
            0,
            Fanout { edges: [None; 300] },
            AllocationSpace::Young,
            &mut roots,
        )
        .unwrap()
        .raw()
        .span_id();
    assert_eq!(reused_span, released_span);

    let mut low_types = TypeRegistry::new();
    let large_type = low_types
        .try_register::<LargePayload>("LargePayload")
        .unwrap();
    let mut low_heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), low_types);
    let mut no_roots = Vec::<Value>::new();
    assert!(matches!(
        low_heap.try_allocate_with_gc(
            large_type,
            0,
            0,
            LargePayload {
                _bytes: [0; 70_000]
            },
            AllocationSpace::Old,
            &mut no_roots,
        ),
        Err(ManagedAllocationError::Allocation(
            HeapAllocationError::HeapLimitExceeded { .. }
        ))
    ));
    assert_eq!(low_heap.trigger_stats().heap_limit_attempts, 1);
}

#[test]
/// A 10,000-object chain reaches its fixed point with a gray peak of one, proving iteration.
fn strong_marking_does_not_recurse_through_the_native_stack() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(1024 * 1024), types);
    let mut tail = None;
    for _ in 0..10_000 {
        tail = Some(
            heap.try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: tail },
                AllocationSpace::Old,
            )
            .unwrap(),
        );
    }
    let mut root = tail.expect("chain is non-empty");

    let stats = heap.mark_strong(&mut root).unwrap();

    assert_eq!(stats.marked_objects, 10_000);
    assert_eq!(stats.traced_objects, 10_000);
    assert_eq!(stats.traced_edges, 10_000);
    assert_eq!(heap.gray_queue_stats().peak_len, 1);
    assert_eq!(heap.gray_queue_stats().initial_capacity, 256);
    assert_eq!(heap.gray_queue_stats().growth_count, 0);
}

#[test]
/// A broad graph crosses the initial queue guess once and retains the measured high water.
fn strong_marking_records_bounded_gray_queue_growth() {
    let mut types = TypeRegistry::new();
    let leaf_type = types.try_register::<Leaf>("Leaf").unwrap();
    let fanout_type = types.try_register::<Fanout>("Fanout").unwrap();
    let mut heap = Heap::new(HeapLimit::new(1024 * 1024), types);
    let mut edges = [None; 300];
    for edge in &mut edges[..299] {
        *edge = Some(
            heap.try_allocate(leaf_type, 0, 0, Leaf, AllocationSpace::Old)
                .unwrap(),
        );
    }
    edges[299] = edges[0];
    let mut root = heap
        .try_allocate(fanout_type, 0, 0, Fanout { edges }, AllocationSpace::Old)
        .unwrap();

    let stats = heap.mark_strong(&mut root).unwrap();
    let queue = heap.gray_queue_stats();

    assert_eq!(stats.marked_objects, 300);
    assert_eq!(stats.traced_objects, 300);
    assert_eq!(stats.traced_edges, 301);
    assert_eq!(queue.initial_capacity, 256);
    assert_eq!(queue.growth_count, 1);
    assert_eq!(queue.peak_len, 299);
    assert!(queue.retained_capacity >= queue.peak_len);
}
