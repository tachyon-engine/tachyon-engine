use core::cmp::Ordering;

use super::{fixtures::test_isolate, *};

struct TestCollatorBackend {
    backing: Box<[u8]>,
}

impl IntlCollatorBackend for TestCollatorBackend {
    fn compare_utf16(&self, left: &[u16], right: &[u16]) -> Result<Ordering, HostProviderError> {
        Ok(left.cmp(right))
    }

    fn external_memory_bytes(&self) -> usize {
        self.backing.len()
    }
}

#[test]
/// Exercises every ordinary-object mutation edge while all allocations force a major collection.
fn collator_slots_backend_and_properties_survive_forced_major_collections() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let locale = isolate
        .allocate_runtime_string(JsString::try_from_latin1(b"en-US").unwrap())
        .unwrap();
    let (collation, locale) = isolate
        .allocate_runtime_string_retaining(JsString::try_from_latin1(b"default").unwrap(), locale)
        .unwrap();
    let prototype = isolate.realm.object_prototype.unwrap();
    let collator = isolate
        .allocate_intl_collator_object(
            Box::new(TestCollatorBackend {
                backing: vec![0; 32].into_boxed_slice(),
            }),
            locale,
            collation,
            IntlCollatorResolvedOptions {
                usage: IntlCollatorUsage::Search,
                sensitivity: IntlCollatorSensitivity::Accent,
                case_first: IntlCollatorCaseFirst::Lower,
                ignore_punctuation: true,
                numeric: true,
            },
            prototype,
            AllocationSpace::Young,
        )
        .unwrap();
    isolate.fiber.registers.push(collator);

    let property = isolate.intern_intrinsic_name(b"property").unwrap();
    let collator = isolate.fiber.registers[0];
    let initial_shape = isolate.object_snapshot(collator).unwrap().1.shape;
    isolate
        .set_own_data_property(collator, property, Value::from_i32(17))
        .unwrap();
    let replacement_prototype = isolate.create_ordinary_object().unwrap();
    let collator = isolate.fiber.registers[0];
    assert!(
        isolate
            .ordinary_set_prototype_of(collator, replacement_prototype)
            .unwrap()
    );
    let (receiver, _) = isolate.object_snapshot(collator).unwrap();
    isolate.set_object_extensible(receiver, false).unwrap();

    let collator = isolate.fiber.registers[0];
    let raw = collator.as_heap_ref().unwrap();
    let collator_ref = isolate
        .heap
        .checked_reference(raw, isolate.types.intl_collator_object)
        .unwrap();
    let slots = isolate.heap.with_running_scope(|scope| {
        let collator_ref = scope.root(collator_ref).unwrap();
        let object = scope.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow(collator_ref, isolate.types.intl_collator_object)
                .copied()
                .unwrap()
        });
        let backend_ref = scope.root(object.backend).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            let backend = no_gc
                .borrow(backend_ref, isolate.types.intl_collator_backend)
                .unwrap();
            assert_eq!(
                backend
                    .backend
                    .compare_utf16(&[b'a' as u16], &[b'b' as u16]),
                Ok(Ordering::Less)
            );
            assert!(backend.external_memory_bytes() >= 32);
            (
                object.locale,
                object.collation,
                object.usage,
                object.sensitivity,
                object.case_first,
                object.ignore_punctuation,
                object.numeric,
                object.cached_bound_compare,
                object.ordinary,
            )
        })
    });
    assert_eq!(
        isolate.string_value_to_utf16(slots.0).unwrap(),
        "en-US".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(
        isolate.string_value_to_utf16(slots.1).unwrap(),
        "default".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(slots.2, IntlCollatorUsage::Search);
    assert_eq!(slots.3, IntlCollatorSensitivity::Accent);
    assert_eq!(slots.4, IntlCollatorCaseFirst::Lower);
    assert!(slots.5 && slots.6);
    assert_eq!(slots.7.as_immediate(), Some(Immediate::Undefined));
    assert!(!slots.8.extensible);
    assert_eq!(slots.8.prototype, replacement_prototype);
    assert_ne!(slots.8.shape, initial_shape);
    assert_eq!(
        isolate.get_data_property(collator, property).unwrap(),
        Some(Value::from_i32(17))
    );
}
