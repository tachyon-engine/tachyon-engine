use super::{fixtures::test_isolate, *};

struct TestNumberFormatBackend {
    backing: Box<[u8]>,
}

impl IntlNumberFormatBackend for TestNumberFormatBackend {
    fn format(&self, value: &IntlMathematicalValue) -> Result<Box<[u16]>, HostProviderError> {
        let value = match value {
            IntlMathematicalValue::Finite(value) => value.as_ref(),
            IntlMathematicalValue::NegativeZero => "-0",
            IntlMathematicalValue::PositiveInfinity => "Infinity",
            IntlMathematicalValue::NegativeInfinity => "-Infinity",
            IntlMathematicalValue::NaN => "NaN",
        };
        Ok(value.encode_utf16().collect::<Vec<_>>().into_boxed_slice())
    }

    fn external_memory_bytes(&self) -> usize {
        self.backing.len()
    }
}

#[test]
/// Proves payload, cache, prototype, shape, and external accounting survive forced major GC.
fn number_format_payload_and_properties_survive_forced_major_collections() {
    let mut isolate = test_isolate();
    isolate
        .heap
        .set_forced_collection_mode(ForcedCollectionMode::Major);
    let prototype = isolate.realm.object_prototype.unwrap();
    let number_format = isolate
        .allocate_intl_number_format_object(
            IntlNumberFormatCreation {
                resolved: IntlNumberFormatResolved {
                    locale: "en-US".into(),
                    numbering_system: "latn".into(),
                    options: IntlNumberFormatOptions::default(),
                },
                backend: Box::new(TestNumberFormatBackend {
                    backing: vec![0; 48].into_boxed_slice(),
                }),
            },
            prototype,
            AllocationSpace::Young,
        )
        .unwrap();
    isolate.fiber.registers.push(number_format);

    let property = isolate.intern_intrinsic_name(b"property").unwrap();
    isolate
        .set_own_data_property(number_format, property, Value::from_i32(23))
        .unwrap();
    let replacement_prototype = isolate.create_ordinary_object().unwrap();
    let number_format = isolate.fiber.registers[0];
    assert!(
        isolate
            .ordinary_set_prototype_of(number_format, replacement_prototype)
            .unwrap()
    );
    let (receiver, _) = isolate.object_snapshot(number_format).unwrap();
    isolate.set_object_extensible(receiver, false).unwrap();

    let raw = number_format.as_heap_ref().unwrap();
    let object_ref = isolate
        .heap
        .checked_reference(raw, isolate.types.intl_number_format_object)
        .unwrap();
    isolate.heap.with_running_scope(|scope| {
        let object_ref = scope.root(object_ref).unwrap();
        let object = scope.with_no_gc_scope(|no_gc| {
            no_gc
                .borrow(object_ref, isolate.types.intl_number_format_object)
                .copied()
                .unwrap()
        });
        assert_eq!(object.ordinary.prototype, replacement_prototype);
        assert!(!object.ordinary.extensible);
        assert_eq!(
            object.cached_bound_format.as_immediate(),
            Some(Immediate::Undefined)
        );
        let payload = scope.root(object.payload).unwrap();
        scope.with_no_gc_scope(|no_gc| {
            let payload = no_gc
                .borrow(payload, isolate.types.intl_number_format_payload)
                .unwrap();
            assert_eq!(&*payload.resolved.locale, "en-US");
            assert_eq!(&*payload.resolved.numbering_system, "latn");
            assert!(payload.external_memory_bytes() >= 48);
            assert_eq!(
                payload
                    .backend
                    .format(&IntlMathematicalValue::Finite("123".into())),
                Ok("123".encode_utf16().collect::<Vec<_>>().into_boxed_slice())
            );
        });
    });
    assert_eq!(
        isolate.get_data_property(number_format, property).unwrap(),
        Some(Value::from_i32(23))
    );
}
