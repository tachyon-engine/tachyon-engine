use super::*;

#[test]
/// Conservative Old initialization discovers young edges, then exact rebuilding clears them.
fn young_mark_rebuilds_small_remembered_cards() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
    let young = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    let old = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: Some(young) },
            AllocationSpace::Old,
        )
        .unwrap();
    let mut no_roots = Vec::<Value>::new();

    let retained = heap.mark_young(&mut no_roots).unwrap();
    assert_eq!(retained.mark.marked_objects, 1);
    assert_eq!(retained.dirty_cards_scanned, 1);
    assert_eq!(retained.old_objects_scanned, 1);
    assert_eq!(retained.card_false_positive_cards, 0);

    heap.with_running_scope(|scope| {
        let old = scope.root(old).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc.borrow_mut(old, node_type).unwrap().next = None;
        });
    });
    let cleared = heap.mark_young(&mut no_roots).unwrap();
    assert_eq!(cleared.dirty_cards_scanned, 1);
    assert_eq!(cleared.old_objects_scanned, 1);
    assert_eq!(cleared.mark.marked_objects, 0);
    assert_eq!(cleared.card_false_positive_cards, 1);
    let skipped = heap.mark_young(&mut no_roots).unwrap();
    assert_eq!(skipped.dirty_cards_scanned, 0);
    assert_eq!(skipped.old_objects_scanned, 0);
}

#[test]
/// A clean Old object enters the remembered set only after its explicit post-write barrier.
fn old_to_young_write_barrier_dirties_a_clean_source_card() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
    let old = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
        )
        .unwrap();
    let mut no_roots = Vec::<Value>::new();
    heap.mark_young(&mut no_roots).unwrap();
    assert_eq!(
        heap.mark_young(&mut no_roots).unwrap().dirty_cards_scanned,
        0
    );
    let young = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();

    heap.with_running_scope(|scope| {
        let old_local = scope.root(old).unwrap();
        let young_local = scope.root(young).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc.borrow_mut(old_local, node_type).unwrap().next = Some(young);
        });
        assert!(scope.write_barrier(old_local, young_local).unwrap());
    });

    let stats = heap.mark_young(&mut no_roots).unwrap();
    assert_eq!(stats.dirty_cards_scanned, 1);
    assert_eq!(stats.old_objects_scanned, 1);
    assert_eq!(stats.mark.marked_objects, 1);
}

#[test]
/// Fault injection distinguishes a missing card from a dirty owner absent from the chain.
fn barrier_verifier_rejects_small_card_and_intrusive_chain_omissions() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(3 * SPAN_SIZE_BYTES), types);
    let old = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Old,
        )
        .unwrap();
    let mut no_roots = Vec::<Value>::new();
    heap.mark_young(&mut no_roots).unwrap();
    let young = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    heap.with_running_scope(|scope| {
        let old = scope.root(old).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc.borrow_mut(old, node_type).unwrap().next = Some(young);
        });
    });

    let missing_card = BarrierVerificationError::MissingSmallCard {
        source: old.raw(),
        target: young.raw(),
    };
    assert_eq!(heap.verify_generational_barriers(), Err(missing_card));
    assert_eq!(
        heap.collect_minor(&mut no_roots),
        Err(MinorCollectionError::Barrier(missing_card))
    );
    assert!(heap.write_barrier(old.raw(), young.raw()).unwrap());
    assert_eq!(
        heap.verify_generational_barriers()
            .unwrap()
            .small_card_edges,
        1
    );

    heap.with_running_scope(|scope| {
        let old = scope.root(old).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc.borrow_mut(old, node_type).unwrap().next = None;
        });
    });
    heap.mark_young(&mut no_roots).unwrap();
    heap.with_running_scope(|scope| {
        let old = scope.root(old).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc.borrow_mut(old, node_type).unwrap().next = Some(young);
        });
    });
    let mut cards = CardBitmap::new();
    cards.mark(old.raw().span_offset());
    heap.span_table_mut()
        .replace_old_cards(old.raw().span_id(), cards);
    assert_eq!(
        heap.verify_generational_barriers(),
        Err(BarrierVerificationError::MissingRememberedSource {
            source: old.raw(),
            target: young.raw(),
        })
    );
}

#[test]
/// Large owners require both their conservative owner bit and remembered-chain membership.
fn barrier_verifier_rejects_missing_large_owner_state() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let large_type = types
        .try_register::<LargeEdgeNode>("LargeEdgeNode")
        .unwrap();
    let mut heap = Heap::new(HeapLimit::new(5 * SPAN_SIZE_BYTES), types);
    let old = heap
        .try_allocate(
            large_type,
            0,
            0,
            LargeEdgeNode {
                _bytes: [0; 70_000],
                next: None,
            },
            AllocationSpace::Old,
        )
        .unwrap();
    let mut no_roots = Vec::<Value>::new();
    heap.mark_young(&mut no_roots).unwrap();
    let young = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    heap.with_running_scope(|scope| {
        let old = scope.root(old).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc.borrow_mut(old, large_type).unwrap().next = Some(young);
        });
    });

    assert_eq!(
        heap.verify_generational_barriers(),
        Err(BarrierVerificationError::MissingLargeRememberedBit {
            source: old.raw(),
            target: young.raw(),
        })
    );
    assert!(heap.write_barrier(old.raw(), young.raw()).unwrap());
    let stats = heap.verify_generational_barriers().unwrap();
    assert_eq!(stats.large_owner_edges, 1);
    assert_eq!(stats.old_to_young_edges, 1);
}

#[test]
/// Direct-old large objects use owner-level remembered state without allocating card arrays.
fn young_mark_scans_and_rebuilds_remembered_large_owners() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let large_type = types
        .try_register::<LargeEdgeNode>("LargeEdgeNode")
        .unwrap();
    let mut heap = Heap::new(HeapLimit::new(3 * SPAN_SIZE_BYTES), types);
    let young = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode { next: None },
            AllocationSpace::Young,
        )
        .unwrap();
    let large = heap
        .try_allocate(
            large_type,
            0,
            0,
            LargeEdgeNode {
                _bytes: [0; 70_000],
                next: Some(young),
            },
            AllocationSpace::Young,
        )
        .unwrap();
    let mut no_roots = Vec::<Value>::new();

    let stats = heap.mark_young(&mut no_roots).unwrap();
    assert_eq!(stats.remembered_large_owners_scanned, 1);
    assert_eq!(stats.mark.marked_objects, 1);

    heap.with_running_scope(|scope| {
        let large = scope.root(large).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc.borrow_mut(large, large_type).unwrap().next = None;
        });
    });
    let cleared = heap.mark_young(&mut no_roots).unwrap();
    assert_eq!(cleared.remembered_large_owners_scanned, 1);
    assert_eq!(cleared.mark.marked_objects, 0);
    let skipped = heap.mark_young(&mut no_roots).unwrap();
    assert_eq!(skipped.remembered_large_owners_scanned, 0);
}

#[test]
/// A failed young mark keeps the original dirty card so a repaired source is rescanned.
fn young_mark_error_preserves_remembered_state() {
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<ChainNode>("ChainNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    let invalid = RawHeapRef::new(u32::MAX).unwrap();
    // SAFETY: this deliberately forged typed edge is never dereferenced; the test proves exact
    // young marking rejects it and retains conservative remembered metadata on the error path.
    let invalid = unsafe { GcRef::<ChainNode>::from_raw_unchecked(invalid) };
    let old = heap
        .try_allocate(
            node_type,
            0,
            0,
            ChainNode {
                next: Some(invalid),
            },
            AllocationSpace::Old,
        )
        .unwrap();
    let mut no_roots = Vec::<Value>::new();
    assert!(matches!(
        heap.mark_young(&mut no_roots),
        Err(crate::MarkError::InvalidReference(_))
    ));

    heap.with_running_scope(|scope| {
        let old = scope.root(old).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            no_gc.borrow_mut(old, node_type).unwrap().next = None;
        });
    });
    let repaired = heap.mark_young(&mut no_roots).unwrap();
    assert_eq!(repaired.dirty_cards_scanned, 1);
    assert_eq!(repaired.old_objects_scanned, 1);
}
