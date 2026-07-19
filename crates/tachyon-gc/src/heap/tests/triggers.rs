use super::*;

#[test]
/// Pending object fields join complete roots before a forced pre-allocation minor collection.
fn forced_minor_traces_pending_value_and_reclaims_other_young_objects() {
    let mut types = TypeRegistry::new();
    let object_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100).unwrap();
    let mut heap = Heap::with_trigger_config(HeapLimit::new(2 * SPAN_SIZE_BYTES), types, config);
    let target = heap
        .try_allocate(
            object_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    let dead = heap
        .try_allocate(
            object_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    heap.set_forced_collection_mode(ForcedCollectionMode::Minor);
    let mut no_roots = Vec::<Value>::new();

    let parent = heap
        .try_allocate_with_gc(
            object_type,
            0,
            0,
            ChainNode { next: Some(target) },
            AllocationSpace::Young,
            &mut no_roots,
        )
        .unwrap();

    assert!(heap.verify_reference(target.raw(), None).is_ok());
    assert!(heap.verify_reference(parent.raw(), None).is_ok());
    assert_eq!(
        heap.verify_reference(dead.raw(), None),
        Err(HeapReferenceError::UnallocatedSlot(dead.raw()))
    );
    let stats = heap.trigger_stats();
    assert_eq!(stats.minor_attempts, 1);
    assert_eq!(stats.minor_successes, 1);
}

#[test]
/// Forced major runs at every managed allocation point and preserves explicit subsystem roots.
fn forced_major_runs_per_allocation_and_traces_explicit_roots() {
    let mut types = TypeRegistry::new();
    let object_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100).unwrap();
    let mut heap = Heap::with_trigger_config(HeapLimit::new(4 * SPAN_SIZE_BYTES), types, config);
    let mut root = heap
        .try_allocate(
            object_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
        )
        .unwrap();
    heap.set_forced_collection_mode(ForcedCollectionMode::Major);

    for _ in 0..2 {
        heap.try_allocate_with_gc(
            object_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
            &mut root,
        )
        .unwrap();
    }

    assert!(heap.verify_reference(root.raw(), None).is_ok());
    let stats = heap.trigger_stats();
    assert_eq!(stats.major_attempts, 2);
    assert_eq!(stats.major_successes, 2);
    assert_eq!(stats.forced_attempts, 2);
}

#[test]
/// Descriptor policy is resolved before forced-minor selection, not after publication.
fn forced_minor_observes_effective_old_only_allocation_policy() {
    let mut types = TypeRegistry::new();
    let object_type = types
        .try_register_old_only::<PinnedPayload>("PinnedPayload")
        .unwrap();
    let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100).unwrap();
    let mut heap = Heap::with_trigger_config(HeapLimit::new(2 * SPAN_SIZE_BYTES), types, config);
    heap.set_forced_collection_mode(ForcedCollectionMode::Minor);
    let mut no_roots = Vec::<Value>::new();

    let reference = heap
        .try_allocate_with_gc(
            object_type,
            0,
            0,
            PinnedPayload,
            AllocationSpace::Young,
            &mut no_roots,
        )
        .unwrap();

    assert_eq!(
        heap.span_table()
            .metadata(reference.raw().span_id())
            .unwrap()
            .space(),
        SpanSpace::Old
    );
    assert_eq!(heap.trigger_stats().minor_attempts, 0);
}

#[test]
/// Raw publication accrues byte debt while only the complete-root managed path repays it.
fn raw_allocation_debt_triggers_the_next_managed_young_allocation() {
    let mut types = TypeRegistry::new();
    let object_type = types.try_register::<Value>("Value").unwrap();
    let config = GcTriggerConfig::new(32, usize::MAX, 100).unwrap();
    let mut heap = Heap::with_trigger_config(HeapLimit::new(4 * SPAN_SIZE_BYTES), types, config);
    let mut root = heap
        .try_allocate(
            object_type,
            0,
            0,
            Value::from_i32(1),
            AllocationSpace::Young,
        )
        .unwrap();
    assert_eq!(heap.trigger_stats().young_debt_bytes, 16);

    heap.try_allocate_with_gc(
        object_type,
        0,
        0,
        Value::from_i32(2),
        AllocationSpace::Young,
        &mut root,
    )
    .unwrap();

    assert!(heap.verify_reference(root.raw(), None).is_ok());
    let stats = heap.trigger_stats();
    assert_eq!(stats.minor_attempts, 1);
    assert_eq!(stats.young_debt_attempts, 1);
    assert_eq!(stats.young_debt_bytes, 16);
    assert_eq!(stats.young_allocated_bytes, 32);
}

#[test]
/// Debt selects major first, then a distinct size class crosses the exact pressure boundary.
fn old_debt_and_storage_pressure_select_full_major_collection() {
    let mut types = TypeRegistry::new();
    let value_type = types.try_register::<Value>("Value").unwrap();
    let fanout_type = types.try_register::<Fanout>("Fanout").unwrap();
    let config = GcTriggerConfig::new(usize::MAX, 32, 50).unwrap();
    let mut heap = Heap::with_trigger_config(HeapLimit::new(4 * SPAN_SIZE_BYTES), types, config);
    let mut root = heap
        .try_allocate(value_type, 0, 0, Value::from_i32(1), AllocationSpace::Old)
        .unwrap();
    heap.try_allocate_with_gc(
        value_type,
        0,
        0,
        Value::from_i32(2),
        AllocationSpace::Old,
        &mut root,
    )
    .unwrap();
    assert_eq!(heap.trigger_stats().old_debt_attempts, 1);

    heap.try_allocate_with_gc(
        fanout_type,
        0,
        0,
        Fanout { edges: [None; 300] },
        AllocationSpace::Old,
        &mut root,
    )
    .unwrap();
    assert_eq!(heap.trigger_stats().heap_pressure_attempts, 1);
}

#[test]
/// Young backing growth beyond the typed cap triggers minor before publishing the next class.
fn young_storage_cap_triggers_minor_only_when_backing_would_grow() {
    let mut types = TypeRegistry::new();
    let value_type = types.try_register::<Value>("Value").unwrap();
    let fanout_type = types.try_register::<Fanout>("Fanout").unwrap();
    let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100)
        .unwrap()
        .with_young_storage_cap_bytes(SPAN_SIZE_BYTES)
        .unwrap();
    let mut heap = Heap::with_trigger_config(HeapLimit::new(4 * SPAN_SIZE_BYTES), types, config);
    let dead = heap
        .try_allocate(value_type, 0, 0, Value::from_i32(1), AllocationSpace::Young)
        .unwrap();
    let mut no_roots = Vec::<Value>::new();

    let allocated = heap
        .try_allocate_with_gc(
            fanout_type,
            0,
            0,
            Fanout { edges: [None; 300] },
            AllocationSpace::Young,
            &mut no_roots,
        )
        .unwrap();

    assert!(heap.verify_reference(allocated.raw(), None).is_ok());
    assert_eq!(
        heap.verify_reference(dead.raw(), None),
        Err(HeapReferenceError::UnallocatedSlot(dead.raw()))
    );
    let stats = heap.trigger_stats();
    assert_eq!(stats.minor_attempts, 1);
    assert_eq!(stats.young_storage_cap_attempts, 1);
    assert_eq!(heap.eden_pool_stats().retained_spans, 1);
    assert_eq!(heap.span_table().young_storage_bytes(), SPAN_SIZE_BYTES);
}

#[test]
/// A hard-limit major rebuilds holes in the full active Old span before the single retry.
fn managed_allocation_reuses_old_holes_after_hard_limit_collection() {
    let mut types = TypeRegistry::new();
    let object_type = types.try_register::<Value>("Value").unwrap();
    let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100).unwrap();
    let mut heap = Heap::with_trigger_config(HeapLimit::new(SPAN_SIZE_BYTES), types, config);
    let mut root = heap
        .try_allocate(object_type, 0, 0, Value::from_i32(0), AllocationSpace::Old)
        .unwrap();
    let span = root.raw().span_id();
    let slot_count = heap
        .span_table()
        .metadata(span)
        .unwrap()
        .size_class()
        .slot_count();
    for value in 1..slot_count {
        heap.try_allocate(
            object_type,
            0,
            0,
            Value::from_i32(i32::from(value)),
            AllocationSpace::Old,
        )
        .unwrap();
    }

    let allocated = heap
        .try_allocate_with_gc(
            object_type,
            0,
            0,
            Value::from_i32(7),
            AllocationSpace::Old,
            &mut root,
        )
        .unwrap();

    assert_eq!(allocated.raw().span_id(), span);
    assert!(heap.verify_reference(root.raw(), None).is_ok());
    assert_eq!(heap.committed_span_storage_bytes(), SPAN_SIZE_BYTES);
    assert_eq!(heap.trigger_stats().heap_limit_attempts, 1);
}

#[test]
/// Repeated host notifications coalesce without turning later allocations into polling points.
fn memory_pressure_commands_coalesce_and_are_consumed_by_one_managed_allocation() {
    let mut types = TypeRegistry::new();
    let object_type = types.try_register::<Value>("Value").unwrap();
    let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100).unwrap();
    let mut heap = Heap::with_trigger_config(HeapLimit::new(4 * SPAN_SIZE_BYTES), types, config);
    heap.request_memory_pressure_collection();
    heap.request_memory_pressure_collection();
    let mut no_roots = Vec::<Value>::new();

    for value in 0..2 {
        heap.try_allocate_with_gc(
            object_type,
            0,
            0,
            Value::from_i32(value),
            AllocationSpace::Young,
            &mut no_roots,
        )
        .unwrap();
    }

    let stats = heap.trigger_stats();
    assert_eq!(stats.memory_pressure_requests, 2);
    assert_eq!(stats.memory_pressure_commands_consumed, 1);
    assert_eq!(stats.major_attempts, 1);
}

#[test]
/// Public heap wiring preserves separately bucketed host durations without reading a clock.
fn heap_pause_api_keeps_host_measured_minor_and_major_samples_separate() {
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), TypeRegistry::new());
    heap.record_collection_pause(
        crate::CollectionKind::Minor,
        core::time::Duration::from_nanos(3),
    );
    heap.record_collection_pause(
        crate::CollectionKind::Major,
        core::time::Duration::from_nanos(17),
    );

    let stats = heap.pause_stats();
    assert_eq!(stats.minor.samples, 1);
    assert_eq!(stats.minor.p99_upper_nanos, Some(4));
    assert_eq!(stats.major.samples, 1);
    assert_eq!(stats.major.p99_upper_nanos, Some(32));
}
