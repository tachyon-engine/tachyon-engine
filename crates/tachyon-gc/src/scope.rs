//! Generative allocation scopes and non-transferable rooted local handles.

use core::{
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
};
use std::rc::Rc;

use crate::{
    AllocationSpace, GcRef, GcType, Heap, HeapAllocationError, HeapReferenceError,
    MajorCollectionError, MajorCollectionStats, TemporaryRootError, Trace,
};

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

    /// Runs a full major with all current locals plus the caller's subsystem roots.
    pub fn collect_major(
        &mut self,
        additional_roots: &mut dyn Trace,
    ) -> Result<MajorCollectionStats, MajorCollectionError> {
        self.heap.collect_major(additional_roots)
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

    /// Returns root-stack capacity evidence without exposing mutable storage.
    #[must_use]
    pub fn temporary_root_stats(&self) -> crate::TemporaryRootStats {
        self.heap.temporary_root_stats()
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

#[cfg(test)]
mod tests {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
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

    impl Trace for DropProbe {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
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
    /// Rust unwinding drops the scope guard and cannot leave a stale temporary root behind.
    fn panic_unwind_restores_the_temporary_root_checkpoint() {
        let (mut heap, object_type, drops) = heap_and_type();
        let reference = heap
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

        let unwind = catch_unwind(AssertUnwindSafe(|| {
            heap.with_running_scope(|scope| {
                let _local = scope.root(reference).unwrap();
                assert_eq!(scope.temporary_root_stats().current_len, 1);
                panic!("intentional scope unwind");
            });
        }));
        assert!(unwind.is_err());
        assert_eq!(heap.temporary_root_stats().current_len, 0);

        let mut no_roots = Vec::<Value>::new();
        heap.collect_major(&mut no_roots).unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 1);
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
}
