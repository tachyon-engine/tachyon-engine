//! Realm-local String iterator intrinsic graph and descriptor publication.

use super::*;

impl Isolate {
    /// Builds `%StringIteratorPrototype%` and publishes String's well-known iterator method.
    pub(super) fn initialize_string_iterator_intrinsics(
        &mut self,
        iterator_prototype: Value,
        function_prototype: Value,
        iterator_key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let prototype = self.allocate_intrinsic_ordinary_object(OrdinaryObject {
            shape: ShapeId::EMPTY,
            extensible: true,
            storage: None,
            prototype: iterator_prototype,
        })?;
        self.realm.string_iterator_prototype = Some(prototype);
        let next = self.allocate_native_function(
            NativeFunction::StringIteratorNext,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.string_iterator_next = Some(next);
        let next_atom = self.intern_intrinsic_name(b"next")?;
        self.set_intrinsic_data_property(prototype, next_atom, next, true)?;
        self.initialize_string_iterator_tag(prototype)?;
        let iterator = self.allocate_native_function(
            NativeFunction::StringIterator,
            OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype: function_prototype,
            },
        )?;
        self.realm.string_iterator = Some(iterator);
        let string_prototype = self
            .realm
            .string_prototype
            .expect("String prototype initializes before iterator intrinsics");
        self.define_data_property(
            string_prototype,
            iterator_key,
            DataPropertyDescriptor {
                value: Some(iterator),
                writable: Some(true),
                enumerable: Some(false),
                configurable: Some(true),
            },
        )
    }

    /// Publishes the standard non-writable, configurable `String Iterator` toStringTag.
    fn initialize_string_iterator_tag(&mut self, prototype: Value) -> Result<(), ExecutionError> {
        self.define_intrinsic_to_string_tag(prototype, b"String Iterator")
    }
}
