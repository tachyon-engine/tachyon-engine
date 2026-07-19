use super::*;

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
