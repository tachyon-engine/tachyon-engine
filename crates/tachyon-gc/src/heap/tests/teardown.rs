use super::*;

#[test]
/// Drops both allocation layouts even while local handles still describe live objects.
fn heap_drop_destroys_live_small_and_large_payloads_exactly_once() {
    let small_drops = Arc::new(AtomicUsize::new(0));
    let large_drops = Arc::new(AtomicUsize::new(0));
    let mut types = TypeRegistry::new();
    let small_type = types.try_register::<DropNode>("DropNode").unwrap();
    let large_type = types.try_register::<DropLarge>("DropLarge").unwrap();
    let mut heap = Heap::new(HeapLimit::new(3 * SPAN_SIZE_BYTES), types);
    heap.try_allocate(
        small_type,
        0,
        0,
        DropNode {
            next: None,
            drops: Arc::clone(&small_drops),
        },
        AllocationSpace::Young,
    )
    .unwrap();
    heap.try_allocate(
        large_type,
        0,
        0,
        DropLarge {
            _bytes: [0; 70_000],
            drops: Arc::clone(&large_drops),
        },
        AllocationSpace::Old,
    )
    .unwrap();

    drop(heap);

    assert_eq!(small_drops.load(Ordering::Relaxed), 1);
    assert_eq!(large_drops.load(Ordering::Relaxed), 1);
}

#[test]
/// Leaves allocation bits cleared by ordinary sweep so teardown cannot invoke Drop twice.
fn heap_drop_does_not_redrop_payload_reclaimed_by_major_collection() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut types = TypeRegistry::new();
    let node_type = types.try_register::<DropNode>("DropNode").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
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
    heap.collect_major(&mut no_roots).unwrap();
    assert_eq!(drops.load(Ordering::Relaxed), 1);

    drop(heap);

    assert_eq!(drops.load(Ordering::Relaxed), 1);
}
