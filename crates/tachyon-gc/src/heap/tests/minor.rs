use super::*;

#[test]
/// Descriptor policy overrides a mistaken Young request for pinned/finalizer payloads.
fn old_only_type_policy_cannot_allocate_into_eden() {
    let mut types = TypeRegistry::new();
    let pinned_type = types
        .try_register_old_only::<PinnedPayload>("PinnedPayload")
        .unwrap();
    assert!(matches!(
        types.try_register::<PinnedPayload>("WrongPolicy"),
        Err(TypeRegistrationError::AllocationPolicyMismatch)
    ));
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    let pinned = heap
        .try_allocate(pinned_type, 0, 0, PinnedPayload, AllocationSpace::Young)
        .unwrap();
    assert_eq!(
        heap.span_table()
            .metadata(pinned.raw().span_id())
            .unwrap()
            .space(),
        SpanSpace::Old
    );
}

#[test]
/// Minor sweep reclaims white slots, ages survivors, promotes in place, and exposes old holes.
fn minor_collection_promotes_without_moving_and_reuses_dead_holes() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<DropNode>("DropNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    let mut root = heap
        .try_allocate(
            node_type,
            0,
            0,
            DropNode {
                next: None,
                drops: Arc::clone(&drops),
            },
            AllocationSpace::Young,
        )
        .unwrap();
    let dead = heap
        .try_allocate(
            node_type,
            0,
            0,
            DropNode {
                next: None,
                drops: Arc::clone(&drops),
            },
            AllocationSpace::Young,
        )
        .unwrap();
    let native_before = heap
        .span_table()
        .base_address(root.raw().span_id())
        .unwrap()
        .wrapping_add(usize::from(root.raw().span_offset().get()));

    let first = heap.collect_minor(&mut root).unwrap();
    assert_eq!(first.sweep.sweep.live_objects, 1);
    assert_eq!(first.sweep.sweep.reclaimed_objects, 1);
    assert_eq!(first.sweep.eden_to_survivor, 1);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(
        heap.span_table()
            .metadata(root.raw().span_id())
            .unwrap()
            .space(),
        SpanSpace::Survivor { age: 1 }
    );

    let second = heap.collect_minor(&mut root).unwrap();
    assert_eq!(second.mark.promotion_objects_scanned, 1);
    assert_eq!(second.sweep.whole_span_promotions, 1);
    assert_eq!(
        heap.span_table()
            .metadata(root.raw().span_id())
            .unwrap()
            .space(),
        SpanSpace::Old
    );
    let native_after = heap
        .span_table()
        .base_address(root.raw().span_id())
        .unwrap()
        .wrapping_add(usize::from(root.raw().span_offset().get()));
    assert_eq!(native_after, native_before);
    assert_eq!(root.raw().span_id(), dead.raw().span_id());

    let reused = heap
        .try_allocate(
            node_type,
            0,
            0,
            DropNode {
                next: None,
                drops: Arc::clone(&drops),
            },
            AllocationSpace::Old,
        )
        .unwrap();
    assert_eq!(reused.raw(), dead.raw());
    let mut no_roots = Vec::<Value>::new();
    heap.collect_major(&mut no_roots).unwrap();
    assert_eq!(drops.load(Ordering::Relaxed), 3);
}

#[test]
/// Pooling an empty Eden span repairs its active cache and reuses logical and native storage.
fn minor_collection_pools_empty_eden_and_repairs_active_cache() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    let dead = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    let base_address = heap
        .span_table()
        .base_address(dead.raw().span_id())
        .unwrap();
    let mut no_roots = Vec::<Value>::new();

    let stats = heap.collect_minor(&mut no_roots).unwrap();
    assert_eq!(stats.sweep.sweep.reclaimed_objects, 1);
    assert_eq!(stats.sweep.sweep.spans_released, 0);
    assert_eq!(stats.sweep.eden_spans_pooled, 1);
    assert_eq!(stats.sweep.eden_pool_retained_bytes, SPAN_SIZE_BYTES);
    assert_eq!(heap.committed_span_storage_bytes(), SPAN_SIZE_BYTES);
    let reused = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    assert_eq!(reused.raw(), dead.raw());
    assert_eq!(
        heap.span_table().base_address(reused.raw().span_id()),
        Some(base_address)
    );
    assert_eq!(heap.eden_pool_stats().spans_reused, 1);
}

#[test]
/// Current-epoch live occupancy promotes a dense Eden span before its normal cohort age.
fn dense_live_eden_span_promotes_early_with_cards_prepared() {
    let mut types = TypeRegistry::new();
    let fanout_type = types.try_register::<Fanout>("Fanout").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    let first = heap
        .try_allocate(
            fanout_type,
            0,
            0,
            Fanout { edges: [None; 300] },
            AllocationSpace::Young,
        )
        .unwrap();
    let span_id = first.raw().span_id();
    let total_slots = heap
        .span_table()
        .metadata(span_id)
        .unwrap()
        .size_class()
        .slot_count() as usize;
    let required_live = total_slots
        .saturating_mul(usize::from(
            crate::tuning::EARLY_PROMOTION_OCCUPANCY_PERCENT,
        ))
        .div_ceil(100);
    let mut roots = Vec::with_capacity(required_live);
    roots.push(first);
    for _ in 1..required_live {
        roots.push(
            heap.try_allocate(
                fanout_type,
                0,
                0,
                Fanout { edges: [None; 300] },
                AllocationSpace::Young,
            )
            .unwrap(),
        );
    }

    let stats = heap.collect_minor(&mut roots).unwrap();

    assert_eq!(stats.sweep.whole_span_promotions, 1);
    assert_eq!(stats.sweep.early_whole_span_promotions, 1);
    assert_eq!(stats.mark.promotion_objects_scanned, required_live);
    assert_eq!(
        heap.span_table().metadata(span_id).unwrap().space(),
        SpanSpace::Old
    );
    assert_eq!(heap.span_table().young_storage_bytes(), 0);
    for root in roots {
        assert!(heap.verify_reference(root.raw(), None).is_ok());
    }
}

#[test]
/// Only one empty span per class is retained; overflow releases and major trim remain exact.
fn eden_pool_bounds_each_size_class_and_major_trims_retained_storage() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
    let mut first = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    heap.collect_minor(&mut first).unwrap();
    heap.try_allocate(
        node_type,
        0,
        0,
        ChainNode { next: None },
        AllocationSpace::Young,
    )
    .unwrap();
    let mut no_roots = Vec::<Value>::new();

    let minor = heap.collect_minor(&mut no_roots).unwrap();
    assert_eq!(minor.sweep.eden_spans_pooled, 1);
    assert_eq!(minor.sweep.eden_pool_overflow_spans_released, 1);
    assert_eq!(minor.sweep.sweep.spans_released, 1);
    assert_eq!(minor.sweep.eden_pool_retained_bytes, SPAN_SIZE_BYTES);
    assert_eq!(heap.eden_pool_stats().overflow_releases, 1);
    assert_eq!(heap.committed_span_storage_bytes(), SPAN_SIZE_BYTES);

    let major = heap.collect_major(&mut no_roots).unwrap();
    assert_eq!(major.sweep.spans_released, 1);
    assert_eq!(major.sweep.released_storage_bytes, SPAN_SIZE_BYTES);
    assert_eq!(major.sweep.eden_pool_retained_bytes, 0);
    assert_eq!(heap.eden_pool_stats().spans_trimmed, 1);
    assert_eq!(heap.committed_span_storage_bytes(), 0);
}

#[test]
/// A retained class cannot hide storage from a one-span hard limit needed by another class.
fn hard_limit_major_trims_pool_before_allocating_a_different_size_class() {
    let mut types = TypeRegistry::new();
    let value_type = types.try_register::<Value>("Value").unwrap();
    let fanout_type = types.try_register::<Fanout>("Fanout").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    heap.try_allocate(value_type, 0, 0, Value::from_i32(1), AllocationSpace::Young)
        .unwrap();
    let mut no_roots = Vec::<Value>::new();
    heap.collect_minor(&mut no_roots).unwrap();
    assert_eq!(heap.eden_pool_stats().retained_bytes, SPAN_SIZE_BYTES);

    let fanout = heap
        .try_allocate_with_gc(
            fanout_type,
            0,
            0,
            Fanout { edges: [None; 300] },
            AllocationSpace::Young,
            &mut no_roots,
        )
        .unwrap();

    assert!(heap.verify_reference(fanout.raw(), None).is_ok());
    assert_eq!(heap.trigger_stats().heap_limit_attempts, 1);
    assert_eq!(heap.eden_pool_stats().spans_trimmed, 1);
    assert_eq!(heap.eden_pool_stats().retained_spans, 0);
    assert_eq!(heap.committed_span_storage_bytes(), SPAN_SIZE_BYTES);
}

#[test]
/// Promotion prepares remembered cards before the source becomes Old in the sweep phase.
fn promoted_span_remembers_edges_to_younger_spans() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
    let mut parent = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    heap.collect_minor(&mut parent).unwrap();
    let child = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    heap.with_running_scope(|scope| {
        let parent = scope.root(parent).unwrap();
        let child_local = scope.root(child).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc.borrow_mut(parent, node_type).unwrap().next = Some(child);
        });
        assert!(!scope.write_barrier(parent, child_local).unwrap());
    });

    let promotion = heap.collect_minor(&mut parent).unwrap();
    assert_eq!(promotion.sweep.whole_span_promotions, 1);
    assert_eq!(promotion.mark.promotion_objects_scanned, 1);
    assert_eq!(
        heap.span_table()
            .metadata(parent.raw().span_id())
            .unwrap()
            .space(),
        SpanSpace::Old
    );
    assert_eq!(
        heap.span_table()
            .metadata(child.raw().span_id())
            .unwrap()
            .space(),
        SpanSpace::Survivor { age: 1 }
    );

    let mut no_roots = Vec::<Value>::new();
    let remembered = heap.collect_minor(&mut no_roots).unwrap();
    assert_eq!(remembered.mark.dirty_cards_scanned, 1);
    assert_eq!(remembered.mark.old_objects_scanned, 1);
    assert_eq!(remembered.mark.mark.marked_objects, 1);
    assert!(heap.verify_reference(child.raw(), None).is_ok());
}

#[test]
/// Minor sweep never reclaims Old payloads even when no precise root reaches them.
fn minor_collection_sweeps_only_young_spans() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<DropNode>("DropNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
    let old = heap
        .try_allocate(
            node_type,
            0,
            0,
            DropNode {
                next: None,
                drops: Arc::clone(&drops),
            },
            AllocationSpace::Old,
        )
        .unwrap();
    heap.try_allocate(
        node_type,
        0,
        0,
        DropNode {
            next: None,
            drops: Arc::clone(&drops),
        },
        AllocationSpace::Young,
    )
    .unwrap();
    let mut no_roots = Vec::<Value>::new();

    let minor = heap.collect_minor(&mut no_roots).unwrap();
    assert_eq!(minor.sweep.sweep.scanned_objects, 1);
    assert_eq!(minor.sweep.sweep.reclaimed_objects, 1);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    let allocation = heap.trigger_stats();
    assert_eq!(
        minor.sweep.sweep.allocated_young_bytes_total,
        allocation.young_allocated_bytes
    );
    assert_eq!(
        minor.sweep.sweep.allocated_old_bytes_total,
        allocation.old_allocated_bytes
    );
    assert_eq!(minor.sweep.sweep.young_live_spans, 0);
    assert_eq!(minor.sweep.sweep.old_live_spans, 1);
    assert_eq!(minor.sweep.sweep.eden_pool_retained_bytes, SPAN_SIZE_BYTES);
    assert!(heap.verify_reference(old.raw(), None).is_ok());
    let major = heap.collect_major(&mut no_roots).unwrap();
    assert_eq!(major.sweep.young_live_spans, 0);
    assert_eq!(major.sweep.old_live_spans, 0);
    assert_eq!(major.sweep.eden_pool_retained_bytes, 0);
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}

#[test]
/// Repeated empty minors reuse one stable entry without duplicate intrusive-list membership.
fn repeated_empty_minor_collections_keep_young_chain_bounded() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    let mut first = None;
    let mut no_roots = Vec::<Value>::new();
    for _ in 0..256 {
        let reference = heap
            .try_allocate(
                node_type,
                0,
                0,
                ChainNode { next: None },
                AllocationSpace::Young,
            )
            .unwrap();
        let expected = *first.get_or_insert(reference.raw());
        assert_eq!(reference.raw(), expected);
        let stats = heap.collect_minor(&mut no_roots).unwrap();
        assert_eq!(stats.sweep.sweep.spans_processed, 1);
        assert_eq!(stats.sweep.sweep.spans_released, 0);
        assert_eq!(stats.sweep.eden_spans_pooled, 1);
        assert_eq!(stats.sweep.eden_pool_retained_bytes, SPAN_SIZE_BYTES);
    }
    assert_eq!(heap.span_table().historical_span_count(), 1);
    assert_eq!(heap.span_table().live_spans(), 1);
    assert_eq!(heap.eden_pool_stats().retained_spans, 1);
    assert_eq!(heap.trim_eden_pool_storage().unwrap(), SPAN_SIZE_BYTES);
    assert_eq!(heap.span_table().live_spans(), 0);
    assert_eq!(heap.committed_span_storage_bytes(), 0);
}
