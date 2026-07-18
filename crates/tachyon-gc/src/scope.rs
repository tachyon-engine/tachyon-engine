//! Generative allocation scopes and non-transferable rooted local handles.

use core::{
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
};
use std::rc::Rc;

use crate::{
    AllocationSpace, GcExternalMemory, GcRef, GcType, GcTypeId, Heap, HeapAllocationError,
    HeapReferenceError, MajorCollectionError, MajorCollectionStats, TemporaryRootError, Trace,
    persistent::{PersistentRootError, PersistentRootId},
};
use tachyon_value::Value;

/// A reference cannot become a local handle unless it is live and root capacity is reserved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootError {
    InvalidReference(HeapReferenceError),
    Capacity(TemporaryRootError),
}

/// A rooted logical reference that cannot escape its generative scope or cross a thread boundary.
///
/// ```compile_fail
/// use tachyon_gc::Local;
/// fn assert_send_sync<T: Send + Sync>() {}
/// assert_send_sync::<Local<'static, ()>>();
/// ```
///
/// A future retaining a local across an await is likewise not transferable to an executor worker:
///
/// ```compile_fail
/// use tachyon_gc::Local;
/// fn assert_send<T: Send>(_: T) {}
/// async fn retain(local: Local<'static, ()>) {
///     std::future::ready(()).await;
///     drop(local);
/// }
/// fn check(local: Local<'static, ()>) {
///     assert_send(retain(local));
/// }
/// ```
pub struct Local<'scope, T: ?Sized> {
    reference: GcRef<T>,
    scope: PhantomData<&'scope mut &'scope ()>,
    not_send_or_sync: PhantomData<Rc<()>>,
}

const _: [(); 4] = [(); core::mem::size_of::<Local<'static, ()>>()];
const _: [(); 4] = [(); core::mem::align_of::<Local<'static, ()>>()];

impl<T: ?Sized> Copy for Local<'_, T> {}

impl<T: ?Sized> Clone for Local<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> fmt::Debug for Local<'_, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Local")
            .field(&self.reference)
            .finish()
    }
}

impl<T: ?Sized> PartialEq for Local<'_, T> {
    fn eq(&self, other: &Self) -> bool {
        self.reference == other.reference
    }
}

impl<T: ?Sized> Eq for Local<'_, T> {}

impl<T: ?Sized> Hash for Local<'_, T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.reference.hash(state);
    }
}

impl<T: ?Sized> Local<'_, T> {
    fn new(reference: GcRef<T>) -> Self {
        Self {
            reference,
            scope: PhantomData,
            not_send_or_sync: PhantomData,
        }
    }

    /// Returns the typed logical address while the originating scope remains active.
    #[must_use]
    pub const fn as_gc_ref(self) -> GcRef<T> {
        self.reference
    }
}

/// A generative root checkpoint; dropping it rolls back every local created in this scope.
pub struct RunningScope<'heap, 'scope> {
    heap: &'heap mut Heap,
    checkpoint: usize,
    scope: PhantomData<&'scope mut &'scope ()>,
    not_send_or_sync: PhantomData<Rc<()>>,
}

impl<'heap, 'scope> RunningScope<'heap, 'scope> {
    pub(crate) fn new(heap: &'heap mut Heap, checkpoint: usize) -> Self {
        Self {
            heap,
            checkpoint,
            scope: PhantomData,
            not_send_or_sync: PhantomData,
        }
    }

    /// Validates and roots an existing allocation before exposing a local handle.
    pub fn root<T: ?Sized>(&mut self, reference: GcRef<T>) -> Result<Local<'scope, T>, RootError> {
        self.heap.try_push_temporary_root(reference.raw())?;
        Ok(Local::new(reference))
    }

    /// Allocates and roots one object before returning control to code that may collect.
    pub fn try_allocate<T: Trace + 'static>(
        &mut self,
        object_type: GcType<T>,
        flags: u16,
        aux: u32,
        value: T,
        space: AllocationSpace,
    ) -> Result<Local<'scope, T>, ScopedAllocationError> {
        let reference = self
            .heap
            .try_allocate(object_type, flags, aux, value, space)
            .map_err(ScopedAllocationError::Allocation)?;
        self.heap
            .try_push_temporary_root(reference.raw())
            .map_err(ScopedAllocationError::Root)?;
        Ok(Local::new(reference))
    }

    /// Allocates, exactly charges, and roots one immutable external-backed payload.
    pub fn try_allocate_external<T: Trace + GcExternalMemory + 'static>(
        &mut self,
        object_type: GcType<T>,
        flags: u16,
        value: T,
        space: AllocationSpace,
    ) -> Result<Local<'scope, T>, ScopedAllocationError> {
        let reference = self
            .heap
            .try_allocate_external(object_type, flags, value, space)
            .map_err(ScopedAllocationError::Allocation)?;
        self.heap
            .try_push_temporary_root(reference.raw())
            .map_err(ScopedAllocationError::Root)?;
        Ok(Local::new(reference))
    }

    /// Runs a full major with all current locals plus the caller's subsystem roots.
    pub fn collect_major(
        &mut self,
        additional_roots: &mut dyn Trace,
    ) -> Result<MajorCollectionStats, MajorCollectionError> {
        self.heap.collect_major(additional_roots)
    }

    /// Runs young-only marking with all current locals plus caller-owned subsystem roots.
    pub fn mark_young(
        &mut self,
        additional_roots: &mut dyn Trace,
    ) -> Result<crate::YoungMarkStats, crate::MarkError> {
        self.heap.mark_young(additional_roots)
    }

    /// Runs a complete young mark/sweep while retaining all locals in this running scope.
    pub fn collect_minor(
        &mut self,
        additional_roots: &mut dyn Trace,
    ) -> Result<crate::MinorCollectionStats, crate::MinorCollectionError> {
        self.heap.collect_minor(additional_roots)
    }

    /// Publishes a completed local-to-local heap pointer store to the generational barrier.
    #[inline(always)]
    pub fn write_barrier<S: ?Sized, T: ?Sized>(
        &mut self,
        source: Local<'scope, S>,
        target: Local<'scope, T>,
    ) -> Result<bool, crate::HeapReferenceError> {
        self.heap
            .write_barrier(source.as_gc_ref().raw(), target.as_gc_ref().raw())
    }

    /// Publishes a potential heap edge stored through a NaN-boxed JavaScript value.
    #[inline(always)]
    pub fn write_value_barrier<S: ?Sized>(
        &mut self,
        source: Local<'scope, S>,
        target: Value,
    ) -> Result<bool, crate::HeapReferenceError> {
        let Some(target) = target.as_heap_ref() else {
            return Ok(false);
        };
        self.heap.write_barrier(source.as_gc_ref().raw(), target)
    }

    /// Implements AddToKeptObjects for a successfully dereferenced weak target.
    pub fn keep_alive<T: ?Sized>(
        &mut self,
        target: Local<'scope, T>,
    ) -> Result<bool, crate::KeptObjectError> {
        self.heap.keep_alive(target.as_gc_ref().raw())
    }

    /// Creates a nested checkpoint whose locals are removed before the outer scope resumes.
    pub fn with_nested_scope<R>(
        &mut self,
        callback: impl for<'nested> FnOnce(&mut RunningScope<'_, 'nested>) -> R,
    ) -> R {
        let checkpoint = self.heap.temporary_root_count();
        let mut nested = RunningScope::new(&mut *self.heap, checkpoint);
        callback(&mut nested)
    }

    /// Enters a phase where payloads may be borrowed because allocation and collection APIs vanish.
    pub fn with_no_gc_scope<R>(
        &mut self,
        callback: impl for<'no_gc> FnOnce(&mut NoGcScope<'_, 'scope, 'no_gc>) -> R,
    ) -> R {
        let mut no_gc = NoGcScope::new(&mut *self.heap);
        callback(&mut no_gc)
    }

    /// Returns root-stack capacity evidence without exposing mutable storage.
    #[must_use]
    pub fn temporary_root_stats(&self) -> crate::TemporaryRootStats {
        self.heap.temporary_root_stats()
    }

    /// Creates an isolate-owned long-lived root ID from a local after descriptor validation.
    pub fn persist<T: Trace + 'static>(
        &mut self,
        local: Local<'scope, T>,
        object_type: GcType<T>,
    ) -> Result<PersistentRootId<T>, PersistentRootError> {
        self.heap
            .create_persistent_root(local.as_gc_ref(), object_type)
    }

    /// Creates an independent root slot for a future actor-backed Persistent clone command.
    pub fn clone_persistent<T: Trace + 'static>(
        &mut self,
        id: PersistentRootId<T>,
        object_type: GcType<T>,
    ) -> Result<PersistentRootId<T>, PersistentRootError> {
        self.heap.clone_persistent_root(id, object_type)
    }

    /// Resolves a persistent ID and publishes a temporary root before returning a Local.
    pub fn local_from_persistent<T: Trace + 'static>(
        &mut self,
        id: PersistentRootId<T>,
        object_type: GcType<T>,
    ) -> Result<Local<'scope, T>, PersistentResolveError> {
        let reference = self
            .heap
            .resolve_persistent_root(id, object_type)
            .map_err(PersistentResolveError::Persistent)?;
        self.heap
            .try_push_temporary_root(reference.raw())
            .map_err(PersistentResolveError::TemporaryRoot)?;
        Ok(Local::new(reference))
    }

    /// Releases one exact root generation; stale copies cannot release a reused slot.
    pub fn release_persistent<T: Trace + 'static>(
        &mut self,
        id: PersistentRootId<T>,
        object_type: GcType<T>,
    ) -> Result<(), PersistentRootError> {
        self.heap.release_persistent_root(id, object_type)
    }
}

impl Drop for RunningScope<'_, '_> {
    fn drop(&mut self) {
        self.heap.truncate_temporary_roots(self.checkpoint);
    }
}

/// Allocation succeeded but publishing its temporary root can still fail independently.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopedAllocationError {
    Allocation(HeapAllocationError),
    Root(RootError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistentResolveError {
    Persistent(PersistentRootError),
    TemporaryRoot(RootError),
}

/// A descriptor token or live logical reference failed before native payload dereference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoGcBorrowError {
    UnregisteredOrMismatchedType { type_id: GcTypeId },
    InvalidReference(HeapReferenceError),
}

/// An exclusive heap phase that can lend payload references but exposes no GC-capable operation.
///
/// Allocation is rejected at compile time because `NoGcScope` deliberately has no allocation or
/// collection method:
///
/// ```compile_fail
/// use tachyon_gc::{AllocationSpace, GcType, NoGcScope, Trace, Tracer};
/// struct Object;
/// impl Trace for Object { fn trace(&mut self, _: &mut dyn Tracer) {} }
/// fn cannot_allocate(scope: &mut NoGcScope<'_, '_, '_>, object_type: GcType<Object>) {
///     scope.try_allocate(object_type, 0, 0, Object, AllocationSpace::Old);
/// }
/// ```
///
/// Payload borrows cannot escape the generative no-GC callback lifetime:
///
/// ```compile_fail
/// use tachyon_gc::{GcType, Local, NoGcScope, Trace, Tracer};
/// struct Object;
/// impl Trace for Object { fn trace(&mut self, _: &mut dyn Tracer) {} }
/// fn escape<'scope>(
///     scope: &NoGcScope<'_, 'scope, '_>,
///     local: Local<'scope, Object>,
///     object_type: GcType<Object>,
/// ) -> &'static Object {
///     scope.borrow(local, object_type).unwrap()
/// }
/// ```
///
/// An exclusive scope borrow prevents overlapping mutable payload references:
///
/// ```compile_fail
/// use tachyon_gc::{GcType, Local, NoGcScope, Trace, Tracer};
/// struct Object(u32);
/// impl Trace for Object { fn trace(&mut self, _: &mut dyn Tracer) {} }
/// fn alias<'scope>(
///     scope: &mut NoGcScope<'_, 'scope, '_>,
///     local: Local<'scope, Object>,
///     object_type: GcType<Object>,
/// ) {
///     let first = scope.borrow_mut(local, object_type).unwrap();
///     let second = scope.borrow_mut(local, object_type).unwrap();
///     first.0 += second.0;
/// }
/// ```
pub struct NoGcScope<'heap, 'scope, 'no_gc> {
    heap: &'heap mut Heap,
    scope: PhantomData<&'scope mut &'scope ()>,
    no_gc: PhantomData<&'no_gc mut &'no_gc ()>,
    not_send_or_sync: PhantomData<Rc<()>>,
}

impl<'heap, 'scope, 'no_gc> NoGcScope<'heap, 'scope, 'no_gc> {
    fn new(heap: &'heap mut Heap) -> Self {
        Self {
            heap,
            scope: PhantomData,
            no_gc: PhantomData,
            not_send_or_sync: PhantomData,
        }
    }

    /// Borrows a validated payload for no longer than this no-GC scope borrow.
    pub fn borrow<T: Trace + 'static>(
        &self,
        local: Local<'scope, T>,
        object_type: GcType<T>,
    ) -> Result<&T, NoGcBorrowError> {
        self.borrow_reference(local.as_gc_ref(), object_type)
    }

    /// Borrows a typed live reference without redundantly publishing it as a temporary root.
    ///
    /// The caller must already retain the reference in a traced owner if it needs to survive a
    /// future collection. This borrow itself cannot overlap allocation or collection because the
    /// `NoGcScope` exclusively holds the heap, and descriptor/liveness checks still run here.
    pub fn borrow_reference<T: Trace + 'static>(
        &self,
        reference: GcRef<T>,
        object_type: GcType<T>,
    ) -> Result<&T, NoGcBorrowError> {
        let payload = self.heap.checked_payload_shared(reference, object_type)?;
        // SAFETY: checked resolution matched this heap's Rust TypeId, header descriptor, payload
        // layout, allocation bit, and owner address. `NoGcScope` exclusively borrows the heap and
        // exposes no allocation/collection API for the returned reference's borrow lifetime.
        Ok(unsafe { &*payload })
    }

    /// Exclusively borrows a validated payload while the mutable scope borrow prevents aliases.
    pub fn borrow_mut<T: Trace + 'static>(
        &mut self,
        local: Local<'scope, T>,
        object_type: GcType<T>,
    ) -> Result<&mut T, NoGcBorrowError> {
        self.borrow_reference_mut(local.as_gc_ref(), object_type)
    }

    /// Exclusively borrows a typed live reference without publishing a temporary root.
    pub fn borrow_reference_mut<T: Trace + 'static>(
        &mut self,
        reference: GcRef<T>,
        object_type: GcType<T>,
    ) -> Result<&mut T, NoGcBorrowError> {
        let mut payload = self.heap.checked_payload_mut(reference, object_type)?;
        // SAFETY: checked resolution proves the concrete `T`; the exclusive borrow of this
        // `NoGcScope` prevents any second shared/mutable payload borrow or heap operation until the
        // returned reference expires.
        Ok(unsafe { payload.as_mut() })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use tachyon_value::Value;

    use super::RootError;
    use crate::{
        AllocationSpace, GcRef, Heap, HeapLimit, HeapReferenceError, RawHeapRef, SPAN_SIZE_BYTES,
        Trace, Tracer, TypeRegistry,
    };

    struct DropProbe {
        drops: Arc<AtomicUsize>,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct Payload {
        value: u32,
    }

    struct OtherPayload;

    #[derive(Debug, Eq, PartialEq)]
    struct LargePayload {
        bytes: [u8; 70_000],
    }

    impl Trace for DropProbe {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Trace for Payload {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Trace for OtherPayload {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Trace for LargePayload {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    fn heap_and_type() -> (Heap, crate::GcType<DropProbe>, Arc<AtomicUsize>) {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut types = TypeRegistry::new();
        let object_type = types.try_register::<DropProbe>("DropProbe").unwrap();
        (
            Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types),
            object_type,
            drops,
        )
    }

    #[test]
    /// A scoped allocation remains live across collection, then dies after the checkpoint rolls back.
    fn running_scope_roots_allocations_until_callback_exit() {
        let (mut heap, object_type, drops) = heap_and_type();
        heap.with_running_scope(|scope| {
            let local = scope
                .try_allocate(
                    object_type,
                    0,
                    0,
                    DropProbe {
                        drops: Arc::clone(&drops),
                    },
                    AllocationSpace::Old,
                )
                .unwrap();
            let mut no_other_roots = Vec::<Value>::new();
            let stats = scope.collect_major(&mut no_other_roots).unwrap();
            assert_eq!(stats.sweep.live_objects, 1);
            assert_eq!(scope.temporary_root_stats().current_len, 1);
            assert_eq!(local.as_gc_ref().raw().span_id().index(), 0);
        });

        assert_eq!(heap.temporary_root_stats().current_len, 0);
        let mut no_roots = Vec::<Value>::new();
        assert_eq!(
            heap.collect_major(&mut no_roots)
                .unwrap()
                .sweep
                .reclaimed_objects,
            1
        );
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    /// A nested checkpoint releases only nested locals while preserving its outer root prefix.
    fn nested_scope_rolls_back_only_its_own_roots() {
        let (mut heap, object_type, drops) = heap_and_type();
        let first = heap
            .try_allocate(
                object_type,
                0,
                0,
                DropProbe {
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let second = heap
            .try_allocate(
                object_type,
                0,
                0,
                DropProbe {
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();

        heap.with_running_scope(|scope| {
            let outer = scope.root(first).unwrap();
            scope.with_nested_scope(|nested| {
                let nested_local = nested.root(second).unwrap();
                let mut no_other_roots = Vec::<Value>::new();
                let stats = nested.collect_major(&mut no_other_roots).unwrap();
                assert_eq!(stats.sweep.live_objects, 2);
                assert_eq!(nested.temporary_root_stats().current_len, 2);
                assert_eq!(nested_local.as_gc_ref(), second);
            });
            assert_eq!(scope.temporary_root_stats().current_len, 1);
            let mut no_other_roots = Vec::<Value>::new();
            let stats = scope.collect_major(&mut no_other_roots).unwrap();
            assert_eq!(stats.sweep.live_objects, 1);
            assert_eq!(stats.sweep.reclaimed_objects, 1);
            assert_eq!(outer.as_gc_ref(), first);
        });
        assert_eq!(drops.load(Ordering::Relaxed), 1);

        let mut no_roots = Vec::<Value>::new();
        heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn invalid_reference_is_rejected_before_root_stack_publication() {
        let (mut heap, _, _) = heap_and_type();
        let raw = RawHeapRef::new(16).unwrap();
        let forged = GcRef::<DropProbe>::from_raw(raw);

        heap.with_running_scope(|scope| {
            assert_eq!(
                scope.root(forged),
                Err(RootError::InvalidReference(
                    HeapReferenceError::UnknownSpan(raw.span_id())
                ))
            );
            assert_eq!(scope.temporary_root_stats().current_len, 0);
            assert_eq!(scope.temporary_root_stats().retained_capacity, 0);
        });
    }

    #[test]
    /// Major marking composes subsystem-owned roots with locals instead of choosing one source.
    fn collection_composes_temporary_and_additional_roots() {
        let (mut heap, object_type, drops) = heap_and_type();
        let temporary = heap
            .try_allocate(
                object_type,
                0,
                0,
                DropProbe {
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();
        let mut additional = heap
            .try_allocate(
                object_type,
                0,
                0,
                DropProbe {
                    drops: Arc::clone(&drops),
                },
                AllocationSpace::Old,
            )
            .unwrap();

        heap.with_running_scope(|scope| {
            let _local = scope.root(temporary).unwrap();
            let stats = scope.collect_major(&mut additional).unwrap();
            assert_eq!(stats.mark.marked_objects, 2);
            assert_eq!(stats.sweep.live_objects, 2);
        });

        let mut no_roots = Vec::<Value>::new();
        heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    /// Shared and exclusive borrows resolve the same small payload without exposing heap operations.
    fn no_gc_scope_borrows_and_mutates_validated_small_payloads() {
        let mut types = TypeRegistry::new();
        let payload_type = types.try_register::<Payload>("Payload").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);

        heap.with_running_scope(|running| {
            let local = running
                .try_allocate(
                    payload_type,
                    0,
                    0,
                    Payload { value: 7 },
                    AllocationSpace::Old,
                )
                .unwrap();
            running.with_no_gc_scope(|no_gc| {
                assert_eq!(no_gc.borrow(local, payload_type).unwrap().value, 7);
                no_gc.borrow_mut(local, payload_type).unwrap().value = 11;
                assert_eq!(no_gc.borrow(local, payload_type).unwrap().value, 11);
            });
            let mut no_other_roots = Vec::<Value>::new();
            assert_eq!(
                running
                    .collect_major(&mut no_other_roots)
                    .unwrap()
                    .sweep
                    .live_objects,
                1
            );
            running.with_no_gc_scope(|no_gc| {
                assert_eq!(
                    no_gc.borrow(local, payload_type).unwrap(),
                    &Payload { value: 11 }
                );
            });
        });
    }

    #[test]
    /// Direct typed-reference borrows validate payloads without growing the temporary-root stack.
    fn no_gc_scope_borrows_traced_references_without_root_publication() {
        let mut types = TypeRegistry::new();
        let payload_type = types.try_register::<Payload>("Payload").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let reference = heap
            .try_allocate(
                payload_type,
                0,
                0,
                Payload { value: 7 },
                AllocationSpace::Old,
            )
            .unwrap();

        heap.with_running_scope(|running| {
            let before = running.temporary_root_stats();
            running.with_no_gc_scope(|no_gc| {
                assert_eq!(
                    no_gc
                        .borrow_reference(reference, payload_type)
                        .unwrap()
                        .value,
                    7
                );
                no_gc
                    .borrow_reference_mut(reference, payload_type)
                    .unwrap()
                    .value = 11;
            });
            assert_eq!(running.temporary_root_stats(), before);
        });
    }

    #[test]
    /// Large owner/continuation storage uses the same descriptor-checked borrow boundary.
    fn no_gc_scope_borrows_large_payloads_at_the_owner_offset() {
        let mut types = TypeRegistry::new();
        let payload_type = types.try_register::<LargePayload>("LargePayload").unwrap();
        let mut heap = Heap::new(HeapLimit::new(2 * SPAN_SIZE_BYTES), types);

        heap.with_running_scope(|running| {
            let local = running
                .try_allocate(
                    payload_type,
                    0,
                    0,
                    LargePayload {
                        bytes: [0x5a; 70_000],
                    },
                    AllocationSpace::Old,
                )
                .unwrap();
            running.with_no_gc_scope(|no_gc| {
                let payload = no_gc.borrow(local, payload_type).unwrap();
                assert_eq!(payload.bytes[0], 0x5a);
                assert_eq!(payload.bytes[69_999], 0x5a);
            });
        });
    }

    #[test]
    /// Unsafe raw retyping still cannot cross the checked descriptor boundary into a Rust borrow.
    fn no_gc_scope_rejects_a_forged_payload_type_before_dereference() {
        let mut types = TypeRegistry::new();
        let payload_type = types.try_register::<Payload>("Payload").unwrap();
        let other_type = types.try_register::<OtherPayload>("OtherPayload").unwrap();
        let mut heap = Heap::new(HeapLimit::new(SPAN_SIZE_BYTES), types);
        let reference = heap
            .try_allocate(
                payload_type,
                0,
                0,
                Payload { value: 7 },
                AllocationSpace::Old,
            )
            .unwrap();
        // SAFETY: this deliberately violates the pointee type only to exercise the checked NoGc
        // boundary; the forged reference is never dereferenced without descriptor validation.
        let forged = unsafe { GcRef::<OtherPayload>::from_raw_unchecked(reference.raw()) };

        heap.with_running_scope(|running| {
            let local = running.root(forged).unwrap();
            running.with_no_gc_scope(|no_gc| {
                assert!(matches!(
                    no_gc.borrow(local, other_type),
                    Err(super::NoGcBorrowError::InvalidReference(
                        HeapReferenceError::TypeMismatch { expected, actual }
                    )) if expected == other_type.type_id() && actual == payload_type.type_id()
                ));
            });
        });
    }

    #[test]
    /// A persistent root outlives its creating Local and release cannot invalidate a resolved Local.
    fn persistent_root_survives_scope_and_release_respects_temporary_root() {
        let (mut heap, object_type, drops) = heap_and_type();
        let persistent = heap.with_running_scope(|running| {
            let local = running
                .try_allocate(
                    object_type,
                    0,
                    0,
                    DropProbe {
                        drops: Arc::clone(&drops),
                    },
                    AllocationSpace::Old,
                )
                .unwrap();
            running.persist(local, object_type).unwrap()
        });
        let mut no_roots = Vec::<Value>::new();
        let retained = heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(retained.sweep.live_objects, 1);
        assert_eq!(heap.persistent_root_stats().live_roots, 1);

        heap.with_running_scope(|running| {
            let _local = running
                .local_from_persistent(persistent, object_type)
                .unwrap();
            running.release_persistent(persistent, object_type).unwrap();
            let mut no_other_roots = Vec::<Value>::new();
            let retained_local = running.collect_major(&mut no_other_roots).unwrap();
            assert_eq!(retained_local.sweep.live_objects, 1);
            assert_eq!(running.temporary_root_stats().current_len, 1);
        });

        assert_eq!(heap.persistent_root_stats().live_roots, 0);
        heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    /// Clone commands allocate independent slots so releasing one handle preserves the other root.
    fn cloned_persistent_roots_release_independently() {
        let (mut heap, object_type, drops) = heap_and_type();
        let (first, second) = heap.with_running_scope(|running| {
            let local = running
                .try_allocate(
                    object_type,
                    0,
                    0,
                    DropProbe {
                        drops: Arc::clone(&drops),
                    },
                    AllocationSpace::Old,
                )
                .unwrap();
            let first = running.persist(local, object_type).unwrap();
            let second = running.clone_persistent(first, object_type).unwrap();
            (first, second)
        });
        assert_eq!(heap.persistent_root_stats().live_roots, 2);

        heap.with_running_scope(|running| {
            running.release_persistent(first, object_type).unwrap();
        });
        let mut no_roots = Vec::<Value>::new();
        assert_eq!(
            heap.collect_major(&mut no_roots)
                .unwrap()
                .sweep
                .live_objects,
            1
        );
        assert_eq!(drops.load(Ordering::Relaxed), 0);

        heap.with_running_scope(|running| {
            running.release_persistent(second, object_type).unwrap();
        });
        heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    /// Reused root slots advance generation, so a stale ID cannot resolve or release its successor.
    fn stale_persistent_id_cannot_access_a_reused_slot() {
        let (mut heap, object_type, drops) = heap_and_type();
        let stale = heap.with_running_scope(|running| {
            let local = running
                .try_allocate(
                    object_type,
                    0,
                    0,
                    DropProbe {
                        drops: Arc::clone(&drops),
                    },
                    AllocationSpace::Old,
                )
                .unwrap();
            let id = running.persist(local, object_type).unwrap();
            running.release_persistent(id, object_type).unwrap();
            id
        });
        let current = heap.with_running_scope(|running| {
            let local = running
                .try_allocate(
                    object_type,
                    0,
                    0,
                    DropProbe {
                        drops: Arc::clone(&drops),
                    },
                    AllocationSpace::Old,
                )
                .unwrap();
            running.persist(local, object_type).unwrap()
        });

        heap.with_running_scope(|running| {
            assert!(matches!(
                running.local_from_persistent(stale, object_type),
                Err(super::PersistentResolveError::Persistent(
                    crate::PersistentRootError::StaleGeneration { .. }
                ))
            ));
            assert!(matches!(
                running.release_persistent(stale, object_type),
                Err(crate::PersistentRootError::StaleGeneration { .. })
            ));
            assert!(running.local_from_persistent(current, object_type).is_ok());
            running.release_persistent(current, object_type).unwrap();
        });
        let mut no_roots = Vec::<Value>::new();
        heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }
}
