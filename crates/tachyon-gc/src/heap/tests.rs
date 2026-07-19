use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::{AllocationSpace, GcExternalMemory, Heap, HeapAllocationError, HeapLimit};
use crate::{
    BarrierVerificationError, CardBitmap, Ephemeron, FinalizationRegistration,
    ForcedCollectionMode, GC_HEADER_EXTERNAL_BYTES_FLAG, GcRef, GcTriggerConfig,
    HeapReferenceError, ManagedAllocationError, MinorCollectionError, RawHeapRef, SPAN_SIZE_BYTES,
    SpanSpace, Trace, Tracer, TypeRegistrationError, TypeRegistry, WeakGcRef,
};
use tachyon_value::Value;

struct OtherPayload;

#[derive(Debug, Eq, PartialEq)]
struct LargePayload {
    _bytes: [u8; 70_000],
}

struct ChainNode {
    next: Option<GcRef<ChainNode>>,
}

struct LargeEdgeNode {
    _bytes: [u8; 70_000],
    next: Option<GcRef<ChainNode>>,
}

struct Leaf;

struct Fanout {
    edges: [Option<GcRef<Leaf>>; 300],
}

struct WeakHolder {
    target: WeakGcRef<ChainNode>,
}

struct EphemeronHolder {
    entry: Ephemeron<ChainNode>,
}

struct FinalizationHolder {
    registration: FinalizationRegistration<ChainNode>,
}

struct PinnedPayload;

struct ExternalPayload {
    backing: Box<[u8]>,
}

struct LargeExternalPayload {
    _inline: [u8; 70_000],
    backing: Box<[u8]>,
}

struct ReportedExternalBytes(usize);

struct DropNode {
    next: Option<GcRef<DropNode>>,
    drops: Arc<AtomicUsize>,
}

struct DropLarge {
    _bytes: [u8; 70_000],
    drops: Arc<AtomicUsize>,
}

struct StressRoots {
    stable: Vec<Value>,
    nodes: Vec<GcRef<ChainNode>>,
}

impl StressRoots {
    fn new() -> Self {
        Self {
            stable: Vec::with_capacity(8),
            nodes: Vec::with_capacity(16),
        }
    }
}

impl Trace for StressRoots {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.stable.trace(tracer);
        self.nodes.trace(tracer);
    }
}

struct DeterministicRng(u64);

impl DeterministicRng {
    #[inline]
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

impl Trace for OtherPayload {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl Trace for LargePayload {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl Trace for ChainNode {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.next.trace(tracer);
    }
}

impl Trace for LargeEdgeNode {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.next.trace(tracer);
    }
}

impl Trace for Leaf {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl Trace for Fanout {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.edges.trace(tracer);
    }
}

impl Trace for WeakHolder {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.target.trace(tracer);
    }
}

impl Trace for EphemeronHolder {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.entry.trace(tracer);
    }
}

impl Trace for FinalizationHolder {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.registration.trace(tracer);
    }
}

impl Trace for PinnedPayload {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl Trace for ExternalPayload {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl GcExternalMemory for ExternalPayload {
    fn external_memory_bytes(&self) -> usize {
        self.backing.len()
    }
}

impl Trace for LargeExternalPayload {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl GcExternalMemory for LargeExternalPayload {
    fn external_memory_bytes(&self) -> usize {
        self.backing.len()
    }
}

impl Trace for ReportedExternalBytes {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl GcExternalMemory for ReportedExternalBytes {
    fn external_memory_bytes(&self) -> usize {
        self.0
    }
}

impl Trace for DropNode {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.next.trace(tracer);
    }
}

impl Drop for DropNode {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

impl Trace for DropLarge {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl Drop for DropLarge {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn first_allocation_uses_slow_path_then_reuses_the_active_eden_span() {
    let mut types = TypeRegistry::new();
    let object_type = types.try_register::<Value>("Value").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    let first = heap
        .try_allocate(
            object_type,
            0,
            0,
            Value::from_i32(1),
            AllocationSpace::Young,
        )
        .unwrap();
    let second = heap
        .try_allocate(
            object_type,
            0,
            0,
            Value::from_i32(2),
            AllocationSpace::Young,
        )
        .unwrap();

    assert_eq!(first.raw().span_id(), second.raw().span_id());
    assert_ne!(first.raw(), second.raw());
    assert_eq!(heap.committed_span_storage_bytes(), SPAN_SIZE_BYTES);
    assert_eq!(heap.span_table().live_spans(), 1);
    assert_eq!(
        heap.verify_reference(first.raw(), Some(object_type.type_id()))
            .unwrap()
            .type_id(),
        Some(object_type.type_id())
    );
}

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
/// Fills the 16-byte class exactly and proves the next slow path returns a typed limit error.
fn full_active_span_obeys_the_configured_storage_limit() {
    let mut types = TypeRegistry::new();
    let object_type = types.try_register::<Value>("Value").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    let first = heap
        .try_allocate(
            object_type,
            0,
            0,
            Value::from_i32(0),
            AllocationSpace::Young,
        )
        .unwrap();
    let span = first.raw().span_id();
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
            AllocationSpace::Young,
        )
        .unwrap();
    }

    assert_eq!(
        heap.try_allocate(
            object_type,
            0,
            0,
            Value::from_i32(-1),
            AllocationSpace::Young
        ),
        Err(HeapAllocationError::HeapLimitExceeded {
            limit: SPAN_SIZE_BYTES,
            committed: SPAN_SIZE_BYTES,
            requested: SPAN_SIZE_BYTES,
        })
    );
    assert_eq!(heap.span_table().live_spans(), 1);
}

#[test]
fn heap_rejects_a_typed_token_not_registered_at_its_header_id() {
    let mut first_registry = TypeRegistry::new();
    let object_type = first_registry.try_register::<Value>("Value").unwrap();
    let mut conflicting_registry = TypeRegistry::new();
    let conflicting_type = conflicting_registry
        .try_register::<OtherPayload>("OtherPayload")
        .unwrap();
    assert_eq!(object_type.type_id(), conflicting_type.type_id());
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), conflicting_registry);

    assert_eq!(
        heap.try_allocate(
            object_type,
            0,
            0,
            Value::from_i32(1),
            AllocationSpace::Young
        ),
        Err(HeapAllocationError::UnregisteredOrMismatchedType {
            type_id: object_type.type_id(),
        })
    );
    assert_eq!(heap.committed_span_storage_bytes(), 0);
}

#[test]
fn checked_reference_restores_only_the_registered_payload_type() {
    let mut types = TypeRegistry::new();
    let value_type = types.try_register::<Value>("Value").unwrap();
    let other_type = types.try_register::<OtherPayload>("OtherPayload").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    let reference = heap
        .try_allocate(value_type, 0, 0, Value::from_i32(7), AllocationSpace::Young)
        .unwrap();

    assert_eq!(
        heap.checked_reference(reference.raw(), value_type),
        Ok(reference)
    );
    assert!(matches!(
        heap.checked_reference(reference.raw(), other_type),
        Err(HeapReferenceError::TypeMismatch { .. })
    ));
}

#[test]
/// Spans a continuation ID, verifies the owner, and rejects an interior logical reference.
fn large_objects_allocate_directly_in_contiguous_old_ranges() {
    let mut types = TypeRegistry::new();
    let object_type = types.try_register::<LargePayload>("LargePayload").unwrap();
    let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);
    let reference = heap
        .try_allocate(
            object_type,
            7,
            11,
            LargePayload {
                _bytes: [0; 70_000],
            },
            AllocationSpace::Young,
        )
        .unwrap();

    assert_eq!(reference.raw().span_id().index(), 0);
    assert_eq!(reference.raw().span_offset().get(), 16);
    assert_eq!(heap.committed_span_storage_bytes(), 2 * SPAN_SIZE_BYTES);
    assert_eq!(heap.span_table().live_spans(), 2);
    assert_eq!(
        heap.span_table()
            .large_metadata(reference.raw().span_id())
            .unwrap()
            .span_count(),
        2
    );
    let header = heap
        .verify_reference(reference.raw(), Some(object_type.type_id()))
        .unwrap();
    assert_eq!(header.flags(), 7);
    assert_eq!(header.aux(), 11);

    let continuation =
        RawHeapRef::from_parts(crate::SpanId::new(1), crate::SpanOffset::new(16).unwrap());
    assert_eq!(
        heap.verify_reference(continuation, None),
        Err(HeapReferenceError::LargeContinuationReference {
            reference: continuation,
            owner: reference.raw().span_id(),
            ordinal: 1,
        })
    );
    assert_eq!(
        heap.span_table()
            .base_address(crate::SpanId::new(1))
            .unwrap() as usize
            - heap
                .span_table()
                .base_address(reference.raw().span_id())
                .unwrap() as usize,
        SPAN_SIZE_BYTES
    );

    let reclaimed = heap.reclaim_large_after_drop(reference.raw()).unwrap();
    assert_eq!(reclaimed.span_count(), 2);
    assert_eq!(reclaimed.storage_bytes(), 2 * SPAN_SIZE_BYTES);
    assert_eq!(heap.committed_span_storage_bytes(), 0);
    assert_eq!(heap.span_table().live_spans(), 0);
    let reused = heap
        .try_allocate(
            object_type,
            0,
            0,
            LargePayload {
                _bytes: [0; 70_000],
            },
            AllocationSpace::Old,
        )
        .unwrap();
    assert_eq!(reused.raw(), reference.raw());
    assert_eq!(heap.span_table().historical_span_count(), 2);
}

#[test]
fn large_object_limit_failure_does_not_publish_owner_or_continuations() {
    let mut types = TypeRegistry::new();
    let object_type = types.try_register::<LargePayload>("LargePayload").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    assert_eq!(
        heap.try_allocate(
            object_type,
            0,
            0,
            LargePayload {
                _bytes: [0; 70_000],
            },
            AllocationSpace::Old,
        ),
        Err(HeapAllocationError::HeapLimitExceeded {
            limit: SPAN_SIZE_BYTES,
            committed: 0,
            requested: 2 * SPAN_SIZE_BYTES,
        })
    );
    assert_eq!(heap.span_table().live_spans(), 0);
    assert_eq!(heap.span_table().historical_span_count(), 0);
}

#[test]
/// Proves host backing charges cannot bypass spans and invalid releases do not underflow.
fn external_backing_bytes_share_the_hard_limit_and_release_exactly() {
    let mut types = TypeRegistry::new();
    let object_type = types.try_register::<Value>("Value").unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES + 32), types);
    heap.try_charge_external(32).unwrap();
    heap.try_allocate(
        object_type,
        0,
        0,
        Value::from_i32(1),
        AllocationSpace::Young,
    )
    .unwrap();

    assert_eq!(heap.external_bytes(), 32);
    assert_eq!(heap.committed_heap_bytes(), SPAN_SIZE_BYTES + 32);
    assert_eq!(
        heap.try_charge_external(1),
        Err(HeapAllocationError::HeapLimitExceeded {
            limit: SPAN_SIZE_BYTES + 32,
            committed: SPAN_SIZE_BYTES + 32,
            requested: 1,
        })
    );
    assert!(!heap.release_external(33));
    assert!(heap.release_external(32));
    assert_eq!(heap.external_bytes(), 0);
}

#[test]
/// Header-owned charges survive while rooted and are removed before young payload drop.
fn gc_owned_external_backing_is_charged_and_released_by_minor_sweep() {
    let mut types = TypeRegistry::new();
    let object_type = types
        .try_register::<ExternalPayload>("ExternalPayload")
        .unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES + 48), types);
    let mut reference = heap
        .try_allocate_external(
            object_type,
            0,
            ExternalPayload {
                backing: vec![0; 32].into_boxed_slice(),
            },
            AllocationSpace::Young,
        )
        .unwrap();

    let header = heap.verify_reference(reference.raw(), None).unwrap();
    assert_eq!(header.external_bytes(), Some(32));
    assert_ne!(header.flags() & GC_HEADER_EXTERNAL_BYTES_FLAG, 0);
    assert_eq!(heap.external_bytes(), 32);
    heap.try_charge_external(16).unwrap();
    assert!(!heap.release_external(32));
    assert_eq!(heap.external_bytes(), 48);
    assert!(heap.release_external(16));
    assert_eq!(heap.external_bytes(), 32);
    heap.collect_minor(&mut reference).unwrap();
    assert_eq!(heap.external_bytes(), 32);

    let mut no_roots = Vec::<Value>::new();
    let stats = heap.collect_minor(&mut no_roots).unwrap();
    assert_eq!(stats.sweep.sweep.reclaimed_objects, 1);
    assert_eq!(stats.sweep.sweep.external_bytes, 0);
    assert_eq!(heap.external_bytes(), 0);
}

#[test]
/// Large owners use the same header charge and release it with their continuation range.
fn gc_owned_external_backing_is_released_by_large_major_sweep() {
    let mut types = TypeRegistry::new();
    let object_type = types
        .try_register::<LargeExternalPayload>("LargeExternalPayload")
        .unwrap();
    let mut heap = Heap::new(HeapLimit::new(3 * SPAN_SIZE_BYTES), types);
    heap.try_allocate_external(
        object_type,
        0,
        LargeExternalPayload {
            _inline: [0; 70_000],
            backing: vec![0; 48].into_boxed_slice(),
        },
        AllocationSpace::Old,
    )
    .unwrap();
    assert_eq!(heap.external_bytes(), 48);

    let mut no_roots = Vec::<Value>::new();
    let stats = heap.collect_major(&mut no_roots).unwrap();
    assert_eq!(stats.sweep.reclaimed_objects, 1);
    assert_eq!(stats.sweep.external_bytes, 0);
    assert_eq!(heap.external_bytes(), 0);
    assert_eq!(heap.committed_span_storage_bytes(), 0);
}

#[test]
/// Reserved header ownership and unrepresentable charges fail before any object publication.
fn external_allocation_rejects_forged_flags_and_unrepresentable_backing() {
    let mut types = TypeRegistry::new();
    let object_type = types
        .try_register::<ReportedExternalBytes>("ReportedExternalBytes")
        .unwrap();
    let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
    assert_eq!(
        heap.try_allocate(
            object_type,
            GC_HEADER_EXTERNAL_BYTES_FLAG,
            1,
            ReportedExternalBytes(0),
            AllocationSpace::Old,
        ),
        Err(HeapAllocationError::ReservedHeaderFlag {
            flags: GC_HEADER_EXTERNAL_BYTES_FLAG,
        })
    );
    let too_large = u32::MAX as usize + 1;
    assert_eq!(
        heap.try_allocate_external(
            object_type,
            0,
            ReportedExternalBytes(too_large),
            AllocationSpace::Old,
        ),
        Err(HeapAllocationError::ExternalBytesTooLarge {
            bytes: too_large,
            maximum: u32::MAX as usize,
        })
    );
    assert_eq!(heap.span_table().live_spans(), 0);
    assert_eq!(heap.external_bytes(), 0);
}

#[test]
/// Combined backing pressure performs one major and retries after reclaiming the old charge.
fn managed_external_allocation_reclaims_backing_before_single_retry() {
    let mut types = TypeRegistry::new();
    let object_type = types
        .try_register::<ExternalPayload>("ExternalPayload")
        .unwrap();
    let config = GcTriggerConfig::new(usize::MAX, usize::MAX, 100).unwrap();
    let mut heap = Heap::with_trigger_config(HeapLimit::new(SPAN_SIZE_BYTES + 64), types, config);
    let dead = heap
        .try_allocate_external(
            object_type,
            0,
            ExternalPayload {
                backing: vec![0; 64].into_boxed_slice(),
            },
            AllocationSpace::Old,
        )
        .unwrap();
    let mut no_roots = Vec::<Value>::new();

    let replacement = heap
        .try_allocate_external_with_gc(
            object_type,
            0,
            ExternalPayload {
                backing: vec![0; 64].into_boxed_slice(),
            },
            AllocationSpace::Old,
            &mut no_roots,
        )
        .unwrap();

    assert_eq!(heap.external_bytes(), 64);
    assert!(heap.verify_reference(replacement.raw(), None).is_ok());
    assert_eq!(replacement.raw(), dead.raw());
    assert_eq!(heap.trigger_stats().major_attempts, 1);
    assert_eq!(heap.trigger_stats().heap_limit_attempts, 1);
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
    assert_eq!(record.registry(), registry.raw());
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
    assert_eq!(record.registry(), registry.raw());
    assert_eq!(record.held_value().as_i32(), Some(7));
}

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
