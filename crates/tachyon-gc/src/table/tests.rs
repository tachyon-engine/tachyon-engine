use super::{SpanTable, SpanTableError};
use crate::{
    CollectionEpoch, GcTypeId, HeapReferenceError, RawHeapRef, SizeClass, SlotIndex,
    SmallAllocationError, SpanId, SpanOffset, SpanReuseGeneration, SpanSpace,
};
use tachyon_value::{Immediate, Value};

fn size_class() -> SizeClass {
    SizeClass::new(16).expect("minimum size class")
}

#[test]
/// Forces metadata-vector growth and proves independently allocated span storage stays stable.
fn table_grows_on_demand_and_resolves_stable_independent_storage() {
    let mut table = SpanTable::new();
    assert_eq!(table.historical_span_count(), 0);
    assert_eq!(table.retained_entry_capacity(), 0);

    let first = table
        .try_allocate_small(size_class(), SpanSpace::Eden)
        .unwrap();
    let first_address = table.base_address(first).unwrap();
    let mut last = first;
    for _ in 1..32 {
        last = table
            .try_allocate_small(size_class(), SpanSpace::Old)
            .unwrap();
    }

    assert_eq!(first, SpanId::new(0));
    assert_eq!(last, SpanId::new(31));
    assert_eq!(table.live_spans(), 32);
    assert_eq!(table.historical_span_count(), 32);
    assert_eq!(table.base_address(first), Some(first_address));
    assert_ne!(table.base_address(last), Some(first_address));
}

#[test]
/// Releases coalesced ranges, reuses a stable ID, and exposes its incremented generation.
fn table_reuses_free_ranges_without_shrinking_historical_indices() {
    let mut table = SpanTable::new();
    let first = table
        .try_allocate_small(size_class(), SpanSpace::Eden)
        .unwrap();
    let second = table
        .try_allocate_small(size_class(), SpanSpace::Eden)
        .unwrap();
    let third = table
        .try_allocate_small(size_class(), SpanSpace::Eden)
        .unwrap();

    table.release(first).unwrap();
    table.release(second).unwrap();
    table.release(third).unwrap();
    assert_eq!(table.live_spans(), 0);
    assert_eq!(table.historical_span_count(), 3);
    assert_eq!(table.base_address(second), None);
    assert_eq!(
        table.release(second),
        Err(SpanTableError::VacantSpan(second))
    );

    let reused = table
        .try_allocate_small(size_class(), SpanSpace::Old)
        .unwrap();
    assert_eq!(reused, third);
    assert_eq!(
        table.metadata(reused).unwrap().reuse_generation(),
        SpanReuseGeneration::INITIAL.next()
    );
    assert_eq!(table.historical_span_count(), 3);
}

#[test]
fn unknown_span_ids_are_rejected_without_mutating_the_table() {
    let mut table = SpanTable::new();
    let unknown = SpanId::new(7);
    assert_eq!(
        table.release(unknown),
        Err(SpanTableError::UnknownSpan(unknown))
    );
    assert_eq!(table.live_spans(), 0);
}

#[test]
/// Forces epoch wrap and proves all live span bitmaps are reset before epoch one is reused.
fn epoch_overflow_resets_every_live_span_bitmap() {
    let mut table = SpanTable::new();
    let first = table
        .try_allocate_small(size_class(), SpanSpace::Eden)
        .unwrap();
    let second = table
        .try_allocate_small(size_class(), SpanSpace::Old)
        .unwrap();
    let maximum = CollectionEpoch::new(u32::MAX).unwrap();
    let slot = SlotIndex::new(0).unwrap();
    assert!(
        table
            .metadata_mut(first)
            .unwrap()
            .marks_mut()
            .mark(slot, maximum)
    );
    assert!(
        table
            .metadata_mut(second)
            .unwrap()
            .marks_mut()
            .mark(slot, maximum)
    );

    let next = table.advance_collection_epoch(maximum);

    assert_eq!(next, CollectionEpoch::INITIAL);
    assert!(
        !table
            .metadata(first)
            .unwrap()
            .marks()
            .is_marked(slot, maximum)
    );
    assert!(
        !table
            .metadata(second)
            .unwrap()
            .marks()
            .is_marked(slot, maximum)
    );
    assert!(
        table
            .metadata_mut(first)
            .unwrap()
            .marks_mut()
            .mark(slot, next)
    );
}

#[test]
/// Publishes only initialized objects and covers verifier boundary/type/allocation failures.
fn typed_small_allocation_and_reference_verification_agree() {
    let mut table = SpanTable::new();
    let span = table
        .try_allocate_small(size_class(), SpanSpace::Eden)
        .unwrap();
    let type_id = GcTypeId::new(9).unwrap();
    let reference = table
        .try_allocate_in_span(
            span,
            type_id,
            0x55aa,
            17,
            Value::from_immediate(Immediate::Null),
        )
        .unwrap();

    let header = table
        .verify_reference(reference.raw(), Some(type_id))
        .unwrap();
    assert_eq!(header.type_id(), Some(type_id));
    assert_eq!(header.flags(), 0x55aa);
    assert_eq!(header.aux(), 17);
    assert_eq!(table.metadata(span).unwrap().allocated_slots(), 1);
    assert_eq!(table.release(span), Err(SpanTableError::LiveSpan(span)));

    let wrong_type = GcTypeId::new(10).unwrap();
    assert_eq!(
        table.verify_reference(reference.raw(), Some(wrong_type)),
        Err(HeapReferenceError::TypeMismatch {
            expected: wrong_type,
            actual: type_id,
        })
    );
    let unallocated = RawHeapRef::from_parts(span, SpanOffset::new(32).unwrap());
    assert_eq!(
        table.verify_reference(unallocated, None),
        Err(HeapReferenceError::UnallocatedSlot(unallocated))
    );
    let misaligned = RawHeapRef::from_parts(span, SpanOffset::new(17).unwrap());
    assert_eq!(
        table.verify_reference(misaligned, None),
        Err(HeapReferenceError::InvalidSlotBoundary(misaligned))
    );
}

#[test]
/// Proves Survivor rejects allocation and Old reuses reclaimed slots before bumping.
fn cohort_allocation_paths_enforce_survivor_and_old_free_list_rules() {
    let mut table = SpanTable::new();
    let type_id = GcTypeId::new(1).unwrap();
    let survivor = table
        .try_allocate_small(size_class(), SpanSpace::Survivor { age: 1 })
        .unwrap();
    assert_eq!(
        table.try_allocate_in_span(survivor, type_id, 0, 0, Value::from_i32(1)),
        Err(SmallAllocationError::SurvivorIsNotAllocatable(survivor))
    );

    let old = table
        .try_allocate_small(size_class(), SpanSpace::Old)
        .unwrap();
    let first = table
        .try_allocate_in_span(old, type_id, 0, 0, Value::from_i32(1))
        .unwrap();
    let second = table
        .try_allocate_in_span(old, type_id, 0, 0, Value::from_i32(2))
        .unwrap();
    assert!(table.reclaim_small_after_drop(first.raw()));
    let reused = table
        .try_allocate_in_span(old, type_id, 0, 0, Value::from_i32(3))
        .unwrap();
    assert_eq!(reused.raw(), first.raw());
    assert_ne!(reused.raw(), second.raw());
    assert_eq!(table.metadata(old).unwrap().allocated_slots(), 2);
}

#[test]
fn selected_span_rejects_payloads_larger_than_its_size_class() {
    #[derive(Debug, Eq, PartialEq)]
    struct Payload([u8; 16]);

    impl crate::Trace for Payload {
        fn trace(&mut self, _: &mut dyn crate::Tracer) {}
    }

    let mut table = SpanTable::new();
    let span = table
        .try_allocate_small(size_class(), SpanSpace::Eden)
        .unwrap();
    assert_eq!(
        table.try_allocate_in_span(span, GcTypeId::new(1).unwrap(), 0, 0, Payload([0; 16])),
        Err(SmallAllocationError::SizeClassTooSmall {
            required: 32,
            actual: 16,
        })
    );
    assert_eq!(core::mem::size_of::<Payload>(), 16);
    assert_eq!(table.metadata(span).unwrap().allocated_slots(), 0);
}
