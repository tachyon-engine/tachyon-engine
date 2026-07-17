//! Immutable descriptor registration completed before a heap begins allocation.

use core::any::TypeId;

use crate::{
    GcAllocationPolicy, GcType, GcTypeId, Trace, TypeDescriptor,
    tuning::{
        CAPACITY_GROWTH_DENOMINATOR, CAPACITY_GROWTH_NUMERATOR, INITIAL_TYPE_DESCRIPTOR_CAPACITY,
    },
};

/// A bounded descriptor registration failure returned before runtime allocation starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeRegistrationError {
    DescriptorTableExhausted,
    DescriptorTableAllocationFailed,
    AllocationPolicyMismatch,
}

struct TypeEntry {
    rust_type_id: TypeId,
    descriptor: TypeDescriptor,
}

/// A compact descriptor table whose IDs are immutable once it moves into a heap.
#[derive(Default)]
pub struct TypeRegistry {
    entries: Vec<TypeEntry>,
}

impl TypeRegistry {
    /// Creates an unallocated builder; the first registration uses the centralized capacity hint.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Registers one concrete traced payload, returning the existing token for duplicate Rust types.
    pub fn try_register<T: Trace + 'static>(
        &mut self,
        name: &'static str,
    ) -> Result<GcType<T>, TypeRegistrationError> {
        self.try_register_with_policy(name, GcAllocationPolicy::YoungEligible)
    }

    /// Registers pinned/finalizer payloads that must bypass Eden even when callers request Young.
    pub fn try_register_old_only<T: Trace + 'static>(
        &mut self,
        name: &'static str,
    ) -> Result<GcType<T>, TypeRegistrationError> {
        self.try_register_with_policy(name, GcAllocationPolicy::OldOnly)
    }

    /// Centralizes duplicate-policy validation and immutable descriptor publication.
    fn try_register_with_policy<T: Trace + 'static>(
        &mut self,
        name: &'static str,
        allocation_policy: GcAllocationPolicy,
    ) -> Result<GcType<T>, TypeRegistrationError> {
        if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.rust_type_id == TypeId::of::<T>())
        {
            if entry.descriptor.allocation_policy() != allocation_policy {
                return Err(TypeRegistrationError::AllocationPolicyMismatch);
            }
            return Ok(GcType::new_with_policy(
                entry.descriptor.type_id(),
                entry.descriptor.name(),
                allocation_policy,
            ));
        }
        if self.entries.len() == u16::MAX as usize {
            return Err(TypeRegistrationError::DescriptorTableExhausted);
        }
        self.reserve_for_registration()?;
        let type_id = GcTypeId::new((self.entries.len() + 1) as u16)
            .expect("descriptor IDs start at one and are bounded above");
        let object_type = GcType::new_with_policy(type_id, name, allocation_policy);
        self.entries.push(TypeEntry {
            rust_type_id: TypeId::of::<T>(),
            descriptor: object_type.descriptor(),
        });
        Ok(object_type)
    }

    /// Resolves immutable erased callbacks from a validated object header type ID.
    #[must_use]
    pub fn descriptor(&self, type_id: GcTypeId) -> Option<TypeDescriptor> {
        self.entries
            .get(type_id.index() as usize - 1)
            .map(|entry| entry.descriptor)
    }

    /// Checks a typed allocation token against this registry in O(1) without function-pointer casts.
    #[must_use]
    #[inline(always)]
    pub fn matches<T: Trace + 'static>(&self, object_type: GcType<T>) -> bool {
        self.entries
            .get(object_type.type_id().index() as usize - 1)
            .is_some_and(|entry| entry.rust_type_id == TypeId::of::<T>())
    }

    /// Returns the number of immutable descriptors for accounting and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether no payload type has been registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn reserve_for_registration(&mut self) -> Result<(), TypeRegistrationError> {
        if self.entries.len() < self.entries.capacity() {
            return Ok(());
        }
        let target = if self.entries.capacity() == 0 {
            INITIAL_TYPE_DESCRIPTOR_CAPACITY
        } else {
            self.entries
                .capacity()
                .saturating_mul(CAPACITY_GROWTH_NUMERATOR)
                .div_ceil(CAPACITY_GROWTH_DENOMINATOR)
        };
        let target = target.max(self.entries.len() + 1).min(u16::MAX as usize);
        self.entries
            .try_reserve_exact(target - self.entries.len())
            .map_err(|_| TypeRegistrationError::DescriptorTableAllocationFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::TypeRegistry;
    use crate::{Trace, Tracer};

    struct First;
    struct Second;

    impl Trace for First {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Trace for Second {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    #[test]
    fn registration_is_stable_for_duplicates_and_distinct_for_types() {
        let mut registry = TypeRegistry::new();
        let first = registry.try_register::<First>("First").unwrap();
        let duplicate = registry.try_register::<First>("IgnoredAlias").unwrap();
        let second = registry.try_register::<Second>("Second").unwrap();

        assert_eq!(first.type_id(), duplicate.type_id());
        assert_ne!(first.type_id(), second.type_id());
        assert!(registry.matches(first));
        assert!(registry.matches(second));
        assert_eq!(
            registry.descriptor(first.type_id()).unwrap().name(),
            "First"
        );
        assert_eq!(registry.len(), 2);
        assert!(!registry.is_empty());
    }
}
