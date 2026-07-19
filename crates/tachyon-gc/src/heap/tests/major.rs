use super::*;

#[test]
/// Keeps an exact graph, reclaims one peer, and proves the rebuilt Old free list reuses its slot.
fn full_major_preserves_roots_and_reuses_reclaimed_old_slots() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<DropNode>("DropNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    let child = heap
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
    let mut root = heap
        .try_allocate(
            node_type,
            0,
            0,
            DropNode {
                next: Some(child),
                drops: Arc::clone(&drops),
            },
            AllocationSpace::Old,
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
            AllocationSpace::Old,
        )
        .unwrap();

    let stats = heap.collect_major(&mut root).unwrap();
    assert_eq!(stats.sweep.scanned_objects, 3);
    assert_eq!(stats.sweep.live_objects, 2);
    assert_eq!(stats.sweep.reclaimed_objects, 1);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(heap.verify_reference(root.raw(), None).is_ok());
    assert!(heap.verify_reference(child.raw(), None).is_ok());
    assert_eq!(
        heap.verify_reference(dead.raw(), None),
        Err(HeapReferenceError::UnallocatedSlot(dead.raw()))
    );

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
    let final_stats = heap.collect_major(&mut no_roots).unwrap();
    assert_eq!(final_stats.sweep.reclaimed_objects, 3);
    assert_eq!(final_stats.sweep.spans_released, 1);
    assert_eq!(drops.load(Ordering::Relaxed), 4);
    assert_eq!(heap.committed_span_storage_bytes(), 0);
}

#[test]
/// Builds a two-node cycle through a validated payload boundary; reachability, not ref counts, wins.
fn full_major_handles_reachable_and_unreachable_cycles() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<DropNode>("DropNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    let first = heap
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
    let second = heap
        .try_allocate(
            node_type,
            0,
            0,
            DropNode {
                next: Some(first),
                drops: Arc::clone(&drops),
            },
            AllocationSpace::Old,
        )
        .unwrap();
    let descriptor = heap.types.descriptor(node_type.type_id()).unwrap();
    let first_payload = heap.table.payload_address(first.raw(), descriptor).unwrap();
    // SAFETY: table verification paired this payload with `DropNode`; collection and allocation
    // are paused while this exclusive test-only mutation installs the back edge.
    unsafe { first_payload.cast::<DropNode>().as_mut().next = Some(second) };
    let mut root = first;

    let live = heap.collect_major(&mut root).unwrap();
    assert_eq!(live.sweep.live_objects, 2);
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    let mut no_roots = Vec::<Value>::new();
    let dead = heap.collect_major(&mut no_roots).unwrap();
    assert_eq!(dead.sweep.reclaimed_objects, 2);
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}

#[test]
/// Reclaims one independently backed large range and invokes its descriptor drop exactly once.
fn full_major_reclaims_large_owner_and_continuations() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut types = TypeRegistry::new();
    let large_type = types.try_register::<DropLarge>("DropLarge").unwrap();
    let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
    let reference = heap
        .try_allocate(
            large_type,
            0,
            0,
            DropLarge {
                _bytes: [0; 70_000],
                drops: Arc::clone(&drops),
            },
            AllocationSpace::Young,
        )
        .unwrap();
    let mut no_roots = Vec::<Value>::new();

    let stats = heap.collect_major(&mut no_roots).unwrap();

    assert_eq!(stats.sweep.scanned_objects, 1);
    assert_eq!(stats.sweep.reclaimed_objects, 1);
    assert_eq!(stats.sweep.spans_processed, 1);
    assert_eq!(stats.sweep.spans_released, 2);
    assert_eq!(stats.sweep.released_storage_bytes, 2 * SPAN_SIZE_BYTES);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(heap.committed_span_storage_bytes(), 0);
    assert_eq!(
        heap.verify_reference(reference.raw(), None),
        Err(HeapReferenceError::VacantSpan(reference.raw().span_id()))
    );
}

#[test]
/// Repeated majors retain live objects, reclaim once, then make an empty collection a no-op.
fn repeated_full_major_collections_do_not_redrop_objects() {
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
            AllocationSpace::Old,
        )
        .unwrap();

    assert_eq!(heap.collect_major(&mut root).unwrap().sweep.live_objects, 1);
    assert_eq!(heap.collect_major(&mut root).unwrap().sweep.live_objects, 1);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    let mut no_roots = Vec::<Value>::new();
    assert_eq!(
        heap.collect_major(&mut no_roots)
            .unwrap()
            .sweep
            .reclaimed_objects,
        1
    );
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(
        heap.collect_major(&mut no_roots)
            .unwrap()
            .sweep
            .scanned_objects,
        0
    );
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}
