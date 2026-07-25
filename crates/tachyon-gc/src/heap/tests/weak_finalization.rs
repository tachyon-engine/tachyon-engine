use super::*;

#[test]
/// Full major clears a dead weak target before sweep invalidates its allocation.
fn full_major_clears_weak_edges_without_retaining_targets() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let weak_type = types.try_register::<WeakHolder>("WeakHolder").unwrap();
    let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
    let target = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
        )
        .unwrap();
    let mut holder = heap
        .try_allocate(
            weak_type,
            0,
            0,
            WeakHolder {
                target: WeakGcRef::new(target),
            },
            AllocationSpace::Old,
        )
        .unwrap();

    let stats = heap.collect_major(&mut holder).unwrap();
    assert_eq!(stats.mark.weak_owners, 1);
    assert_eq!(stats.mark.weak_slots_cleared, 1);
    assert_eq!(stats.sweep.reclaimed_objects, 1);
    let weak_capacity = heap.weak_owner_stats();
    assert_eq!(weak_capacity.current_len, 1);
    assert_eq!(weak_capacity.initial_capacity, 64);
    assert!(matches!(
        heap.verify_reference(target.raw(), None),
        Err(HeapReferenceError::UnallocatedSlot(_))
    ));
    let cleared = heap.with_running_scope(|scope| {
        let holder = scope.root(holder).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow(holder, weak_type)
                .unwrap()
                .target
                .get()
                .is_none()
        })
    });
    assert!(cleared);
}

#[test]
/// Reversed ephemeron owners require a second pass to propagate key liveness to the leaf.
fn full_major_reaches_ephemeron_fixed_point() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let ephemeron_type = types
        .try_register::<EphemeronHolder>("EphemeronHolder")
        .unwrap();
    let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
    let key = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
        )
        .unwrap();
    let second_key = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
        )
        .unwrap();
    let leaf = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
        )
        .unwrap();
    let first = heap
        .try_allocate(
            ephemeron_type,
            0,
            0,
            EphemeronHolder {
                entry: Ephemeron::new(key, Value::from_heap_ref(second_key.raw())),
            },
            AllocationSpace::Old,
        )
        .unwrap();
    let second = heap
        .try_allocate(
            ephemeron_type,
            0,
            0,
            EphemeronHolder {
                entry: Ephemeron::new(second_key, Value::from_heap_ref(leaf.raw())),
            },
            AllocationSpace::Old,
        )
        .unwrap();
    let mut roots = vec![
        Value::from_heap_ref(first.raw()),
        Value::from_heap_ref(second.raw()),
        Value::from_heap_ref(key.raw()),
    ];

    let stats = heap.collect_major(&mut roots).unwrap();
    assert!(stats.mark.ephemeron_passes >= 2);
    assert_eq!(stats.mark.ephemeron_values_marked, 2);
    assert_eq!(stats.mark.ephemerons_cleared, 0);
    assert_eq!(stats.sweep.live_objects, 5);
    assert!(heap.verify_reference(leaf.raw(), None).is_ok());
}

#[test]
/// A dead ephemeron key clears both entry fields and permits its value to be swept.
fn full_major_clears_dead_ephemerons() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let ephemeron_type = types
        .try_register::<EphemeronHolder>("EphemeronHolder")
        .unwrap();
    let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
    let key = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
        )
        .unwrap();
    let value = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
        )
        .unwrap();
    let mut holder = heap
        .try_allocate(
            ephemeron_type,
            0,
            0,
            EphemeronHolder {
                entry: Ephemeron::new(key, Value::from_heap_ref(value.raw())),
            },
            AllocationSpace::Old,
        )
        .unwrap();

    let stats = heap.collect_major(&mut holder).unwrap();
    assert_eq!(stats.mark.ephemerons_cleared, 1);
    assert_eq!(stats.sweep.reclaimed_objects, 2);
    let cleared = heap.with_running_scope(|scope| {
        let holder = scope.root(holder).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            let entry = &no_gc.borrow(holder, ephemeron_type).unwrap().entry;
            entry.key().is_none()
                && entry.value().as_immediate() == Some(tachyon_value::Immediate::Undefined)
        })
    });
    assert!(cleared);
}

#[test]
/// Minor clearing discovers a weak Old owner through its card and reclaims only the young target.
fn minor_clears_old_to_young_weak_edges() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let weak_type = types.try_register::<WeakHolder>("WeakHolder").unwrap();
    let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
    let target = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    let holder = heap
        .try_allocate(
            weak_type,
            0,
            0,
            WeakHolder {
                target: WeakGcRef::new(target),
            },
            AllocationSpace::Old,
        )
        .unwrap();
    let mut no_roots = Vec::<Value>::new();

    let stats = heap.collect_minor(&mut no_roots).unwrap();
    assert_eq!(stats.mark.mark.weak_slots_cleared, 1);
    assert_eq!(stats.sweep.sweep.reclaimed_objects, 1);
    assert!(heap.verify_reference(holder.raw(), None).is_ok());
    let cleared = heap.with_running_scope(|scope| {
        let holder = scope.root(holder).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow(holder, weak_type)
                .unwrap()
                .target
                .get()
                .is_none()
        })
    });
    assert!(cleared);
}

#[test]
/// Minor ephemeron closure treats every Old key as live and retains its young value.
fn minor_ephemeron_with_old_key_marks_young_value() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let ephemeron_type = types
        .try_register::<EphemeronHolder>("EphemeronHolder")
        .unwrap();
    let mut heap = Heap::new(HeapLimit::new(3 * SPAN_SIZE_BYTES), types);
    let key = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
        )
        .unwrap();
    let value = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    heap.try_allocate(
        ephemeron_type,
        0,
        0,
        EphemeronHolder {
            entry: Ephemeron::new(key, Value::from_heap_ref(value.raw())),
        },
        AllocationSpace::Old,
    )
    .unwrap();
    let mut no_roots = Vec::<Value>::new();

    let stats = heap.collect_minor(&mut no_roots).unwrap();
    assert_eq!(stats.mark.mark.ephemeron_values_marked, 1);
    assert_eq!(stats.mark.mark.ephemerons_cleared, 0);
    assert!(heap.verify_reference(value.raw(), None).is_ok());
    assert_eq!(
        heap.span_table()
            .metadata(value.raw().span_id())
            .unwrap()
            .space(),
        SpanSpace::Survivor { age: 1 }
    );
}

#[test]
/// A dead young key clears its Old ephemeron owner and permits both young objects to die.
fn minor_clears_ephemeron_with_dead_young_key() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let ephemeron_type = types
        .try_register::<EphemeronHolder>("EphemeronHolder")
        .unwrap();
    let mut heap = Heap::new(HeapLimit::new(3 * SPAN_SIZE_BYTES), types);
    let key = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    let value = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    let holder = heap
        .try_allocate(
            ephemeron_type,
            0,
            0,
            EphemeronHolder {
                entry: Ephemeron::new(key, Value::from_heap_ref(value.raw())),
            },
            AllocationSpace::Old,
        )
        .unwrap();
    let mut no_roots = Vec::<Value>::new();

    let stats = heap.collect_minor(&mut no_roots).unwrap();
    assert_eq!(stats.mark.mark.ephemerons_cleared, 1);
    assert_eq!(stats.sweep.sweep.reclaimed_objects, 2);
    let cleared = heap.with_running_scope(|scope| {
        let holder = scope.root(holder).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            let entry = &no_gc.borrow(holder, ephemeron_type).unwrap().entry;
            entry.key().is_none()
                && entry.value().as_immediate() == Some(tachyon_value::Immediate::Undefined)
        })
    });
    assert!(cleared);
}

#[test]
/// AddToKeptObjects survives collections until the host explicitly ends the current job.
fn kept_objects_are_job_scoped_precise_roots() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    let target = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
        )
        .unwrap();
    heap.with_running_scope(|scope| {
        let target = scope.root(target).unwrap();
        assert!(scope.keep_alive(target).unwrap());
        assert!(!scope.keep_alive(target).unwrap());
    });
    let mut no_roots = Vec::<Value>::new();

    let retained = heap.collect_major(&mut no_roots).unwrap();
    assert_eq!(retained.sweep.live_objects, 1);
    assert_eq!(heap.kept_object_stats().current_len, 1);
    assert_eq!(heap.kept_object_stats().initial_capacity, 64);
    heap.clear_kept_objects_at_job_boundary();
    let released = heap.collect_major(&mut no_roots).unwrap();
    assert_eq!(released.sweep.reclaimed_objects, 1);
    assert_eq!(heap.kept_object_stats().current_len, 0);
}

#[test]
/// Dead finalization targets enqueue cleanup before sweep; queued registry/value stay rooted.
fn finalization_queue_roots_cleanup_until_safepoint_consumption() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let registry_type = types
        .try_register::<FinalizationHolder>("FinalizationHolder")
        .unwrap();
    let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
    let target = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
        )
        .unwrap();
    let held = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
        )
        .unwrap();
    let mut registry = heap
        .try_allocate(
            registry_type,
            0,
            0,
            FinalizationHolder {
                registration: FinalizationRegistration::new(
                    target,
                    Value::from_heap_ref(held.raw()),
                ),
            },
            AllocationSpace::Old,
        )
        .unwrap();

    let first = heap.collect_major(&mut registry).unwrap();
    assert_eq!(first.mark.finalizations_enqueued, 1);
    assert_eq!(first.sweep.reclaimed_objects, 1);
    assert_eq!(heap.finalization_queue_stats().pending, 1);
    assert_eq!(heap.finalization_queue_stats().initial_capacity, 64);
    let mut no_roots = Vec::<Value>::new();
    let queued = heap.collect_major(&mut no_roots).unwrap();
    assert_eq!(queued.sweep.live_objects, 2);
    let record = heap.pop_pending_finalization().unwrap();
    assert_eq!(record.owner(), registry.raw());
    assert_eq!(record.held_value().as_heap_ref(), Some(held.raw()));
    let drained = heap.collect_major(&mut no_roots).unwrap();
    assert_eq!(drained.sweep.reclaimed_objects, 2);
}

#[test]
/// Minor finalization enqueues an Old registry's dead young target before young sweep.
fn minor_enqueues_dead_young_finalization_targets() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let registry_type = types
        .try_register::<FinalizationHolder>("FinalizationHolder")
        .unwrap();
    let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
    let target = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    let registry = heap
        .try_allocate(
            registry_type,
            0,
            0,
            FinalizationHolder {
                registration: FinalizationRegistration::new(target, Value::from_i32(7)),
            },
            AllocationSpace::Old,
        )
        .unwrap();
    let mut no_roots = Vec::<Value>::new();

    let stats = heap.collect_minor(&mut no_roots).unwrap();
    assert_eq!(stats.mark.mark.finalizations_enqueued, 1);
    assert_eq!(stats.sweep.sweep.reclaimed_objects, 1);
    let record = heap.pop_pending_finalization().unwrap();
    assert_eq!(record.owner(), registry.raw());
    assert_eq!(record.held_value().as_i32(), Some(7));
}
