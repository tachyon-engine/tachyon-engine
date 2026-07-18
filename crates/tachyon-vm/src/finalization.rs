//! Post-collection FinalizationRegistry cleanup jobs owned and rooted by the VM.

use std::collections::VecDeque;

use tachyon_gc::{Heap, PendingFinalization, RawHeapRef, Trace, Tracer};
use tachyon_value::Value;

use crate::Isolate;

/// One VM job whose registry and held value remain exact roots while its callback runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalizationCleanupJob(PendingFinalization);

impl FinalizationCleanupJob {
    #[must_use]
    pub const fn registry(self) -> RawHeapRef {
        self.0.registry()
    }

    #[must_use]
    pub const fn held_value(self) -> Value {
        self.0.held_value()
    }
}

/// Retained FIFO capacity and work state for diagnostics and later corpus tuning.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FinalizationJobQueueStats {
    pub queued: usize,
    pub initial_capacity: usize,
    pub growth_count: usize,
    pub peak_len: usize,
    pub retained_capacity: usize,
    pub slack_entries: usize,
}

/// Work completed by one cleanup safepoint before success or a callback throw.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FinalizationSafepointStats {
    pub scheduled_from_gc: usize,
    pub callbacks_completed: usize,
    pub queued_for_next_safepoint: usize,
    pub deferred_in_gc: usize,
}

/// A cleanup safepoint cannot run recursively or publish jobs without reserved storage.
#[derive(Debug, Eq, PartialEq)]
pub enum FinalizationSafepointError<E> {
    Reentrant,
    JobQueueAllocationFailed,
    Callback {
        error: E,
        stats: FinalizationSafepointStats,
    },
}

#[derive(Debug)]
pub(crate) struct FinalizationJobs {
    entries: VecDeque<PendingFinalization>,
    running: bool,
    initial_capacity: usize,
    growth_count: usize,
    peak_len: usize,
}

impl FinalizationJobs {
    pub const fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            running: false,
            initial_capacity: 0,
            growth_count: 0,
            peak_len: 0,
        }
    }

    /// Transfers only an entry snapshot after reserving its complete destination capacity.
    fn try_schedule_snapshot(&mut self, heap: &mut Heap) -> Result<usize, ()> {
        if !self.entries.is_empty() {
            return Ok(0);
        }
        let snapshot = heap.finalization_queue_stats().pending;
        if snapshot == 0 {
            return Ok(0);
        }
        let old_capacity = self.entries.capacity();
        self.entries.try_reserve_exact(snapshot).map_err(|_| ())?;
        if self.entries.capacity() != old_capacity {
            if self.initial_capacity == 0 {
                self.initial_capacity = self.entries.capacity();
            } else {
                self.growth_count = self.growth_count.saturating_add(1);
            }
        }
        for _ in 0..snapshot {
            let record = heap
                .pop_pending_finalization()
                .expect("the reserved finalization snapshot remains FIFO-stable");
            self.entries.push_back(record);
        }
        self.peak_len = self.peak_len.max(self.entries.len());
        Ok(snapshot)
    }

    fn front(&self) -> Option<FinalizationCleanupJob> {
        self.entries.front().copied().map(FinalizationCleanupJob)
    }

    fn complete_front(&mut self) {
        self.entries
            .pop_front()
            .expect("a callback can complete only the rooted front job");
    }

    fn stats(&self) -> FinalizationJobQueueStats {
        FinalizationJobQueueStats {
            queued: self.entries.len(),
            initial_capacity: self.initial_capacity,
            growth_count: self.growth_count,
            peak_len: self.peak_len,
            retained_capacity: self.entries.capacity(),
            slack_entries: self.entries.capacity() - self.entries.len(),
        }
    }
}

impl Trace for FinalizationJobs {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for job in &mut self.entries {
            job.trace(tracer);
        }
    }
}

impl Isolate {
    /// Runs the eligible FIFO cleanup jobs at a post-collection VM safepoint.
    ///
    /// If an earlier callback left jobs queued, they run before newly pending collector records.
    /// A job stays at the queue front, and therefore in `Isolate`'s exact root set, until its
    /// callback returns. Records enqueued by a callback remain in `Heap` for the next safepoint.
    pub fn run_finalization_cleanup_safepoint<E>(
        &mut self,
        heap: &mut Heap,
        mut callback: impl FnMut(&mut Isolate, &mut Heap, FinalizationCleanupJob) -> Result<(), E>,
    ) -> Result<FinalizationSafepointStats, FinalizationSafepointError<E>> {
        if self.finalization_jobs.running {
            return Err(FinalizationSafepointError::Reentrant);
        }
        let scheduled_from_gc = self
            .finalization_jobs
            .try_schedule_snapshot(heap)
            .map_err(|()| FinalizationSafepointError::JobQueueAllocationFailed)?;
        let eligible = self.finalization_jobs.entries.len();
        let mut callbacks_completed = 0;
        self.finalization_jobs.running = true;

        for _ in 0..eligible {
            let job = self
                .finalization_jobs
                .front()
                .expect("eligible cleanup count matches the rooted FIFO");
            let result = callback(self, heap, job);
            self.finalization_jobs.running = false;
            self.finalization_jobs.complete_front();
            match result {
                Ok(()) => {
                    callbacks_completed += 1;
                    self.finalization_jobs.running = true;
                }
                Err(error) => {
                    return Err(FinalizationSafepointError::Callback {
                        error,
                        stats: self.finalization_safepoint_stats(
                            heap,
                            scheduled_from_gc,
                            callbacks_completed,
                        ),
                    });
                }
            }
        }

        self.finalization_jobs.running = false;
        Ok(self.finalization_safepoint_stats(heap, scheduled_from_gc, callbacks_completed))
    }

    #[must_use]
    pub fn finalization_job_queue_stats(&self) -> FinalizationJobQueueStats {
        self.finalization_jobs.stats()
    }

    fn finalization_safepoint_stats(
        &self,
        heap: &Heap,
        scheduled_from_gc: usize,
        callbacks_completed: usize,
    ) -> FinalizationSafepointStats {
        FinalizationSafepointStats {
            scheduled_from_gc,
            callbacks_completed,
            queued_for_next_safepoint: self.finalization_jobs.entries.len(),
            deferred_in_gc: heap.finalization_queue_stats().pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use tachyon_gc::{
        AllocationSpace, FinalizationRegistration, GcRef, GcType, HeapLimit, SPAN_SIZE_BYTES,
        Trace, Tracer, TypeRegistry,
    };

    use super::{FinalizationSafepointError, Isolate};
    use crate::{AtomHashSeed, AtomTableConfig, IsolateConfig, RealmLimits, StackLimits};
    use tachyon_gc::Heap;
    use tachyon_value::{RawHeapRef, Value};

    struct Leaf;

    struct Registry {
        registration: FinalizationRegistration<Leaf>,
    }

    impl Trace for Leaf {
        fn trace(&mut self, _: &mut dyn Tracer) {}
    }

    impl Trace for Registry {
        fn trace(&mut self, tracer: &mut dyn Tracer) {
            self.registration.trace(tracer);
        }
    }

    struct CallbackRoots<'a> {
        isolate: &'a mut Isolate,
        registry: GcRef<Registry>,
    }

    impl Trace for CallbackRoots<'_> {
        fn trace(&mut self, tracer: &mut dyn Tracer) {
            self.isolate.trace(tracer);
            self.registry.trace(tracer);
        }
    }

    #[derive(Clone, Copy)]
    enum HeldValueKind {
        Integer,
        HeapReference,
    }

    struct PendingFixture {
        heap: Heap,
        expected: Vec<(RawHeapRef, Value)>,
        leaf_type: GcType<Leaf>,
        registry_type: GcType<Registry>,
    }

    fn test_isolate() -> Isolate {
        Isolate::new(IsolateConfig::new(
            AtomTableConfig::new(64, 4 * SPAN_SIZE_BYTES, AtomHashSeed::new(1, 2)),
            HeapLimit::new(4 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            RealmLimits::new(64, 1_024),
        ))
        .expect("test isolate descriptors register")
    }

    /// Builds live registries with dead targets, then runs the selected collector to publish jobs.
    fn heap_with_pending_finalizations(
        count: usize,
        target_space: AllocationSpace,
        held_kind: HeldValueKind,
    ) -> PendingFixture {
        let mut types = TypeRegistry::new();
        let leaf_type = types.try_register::<Leaf>("Leaf").unwrap();
        let registry_type = types.try_register::<Registry>("Registry").unwrap();
        let mut heap = Heap::new(HeapLimit::new(4 * SPAN_SIZE_BYTES), types);
        let mut registries = Vec::with_capacity(count);
        let mut expected = Vec::with_capacity(count);
        for index in 0..count {
            let target = heap
                .try_allocate(leaf_type, 0, 0, Leaf, target_space)
                .unwrap();
            let held_value = match held_kind {
                HeldValueKind::Integer => Value::from_i32(index as i32 + 1),
                HeldValueKind::HeapReference => {
                    let held = heap
                        .try_allocate(leaf_type, 0, 0, Leaf, AllocationSpace::Old)
                        .unwrap();
                    Value::from_heap_ref(held.raw())
                }
            };
            let registry = heap
                .try_allocate(
                    registry_type,
                    0,
                    0,
                    Registry {
                        registration: FinalizationRegistration::new(target, held_value),
                    },
                    AllocationSpace::Old,
                )
                .unwrap();
            registries.push(registry);
            expected.push((registry.raw(), held_value));
        }
        match target_space {
            AllocationSpace::Young => {
                heap.collect_minor(&mut registries).unwrap();
            }
            AllocationSpace::Old => {
                heap.collect_major(&mut registries).unwrap();
            }
        }
        assert_eq!(heap.finalization_queue_stats().pending, count);
        PendingFixture {
            heap,
            expected,
            leaf_type,
            registry_type,
        }
    }

    #[test]
    /// A callback-triggered major must see all scheduled registry and held-value roots.
    fn major_cleanup_jobs_remain_rooted_during_callback_collection() {
        let PendingFixture {
            mut heap, expected, ..
        } = heap_with_pending_finalizations(3, AllocationSpace::Old, HeldValueKind::HeapReference);
        let mut isolate = test_isolate();
        let mut observed = Vec::new();

        let stats = isolate
            .run_finalization_cleanup_safepoint(&mut heap, |isolate, heap, job| {
                heap.collect_major(isolate).unwrap();
                heap.verify_reference(job.registry(), None).unwrap();
                heap.verify_reference(job.held_value().as_heap_ref().unwrap(), None)
                    .unwrap();
                observed.push((job.registry(), job.held_value()));
                Ok::<_, ()>(())
            })
            .unwrap();

        observed.sort_by_key(|(registry, _)| registry.offset());
        let mut expected = expected;
        expected.sort_by_key(|(registry, _)| registry.offset());
        assert_eq!(observed, expected);
        assert_eq!(stats.scheduled_from_gc, 3);
        assert_eq!(stats.callbacks_completed, 3);
        assert_eq!(stats.queued_for_next_safepoint, 0);
        assert_eq!(stats.deferred_in_gc, 0);
        let queue = isolate.finalization_job_queue_stats();
        assert_eq!(queue.peak_len, 3);
        assert_eq!(queue.queued, 0);
        assert!(queue.retained_capacity >= 3);
    }

    #[test]
    /// Minor records preserve FIFO, and a thrown callback consumes only its own front job.
    fn minor_cleanup_throw_keeps_older_jobs_ahead_of_new_gc_records() {
        let PendingFixture { mut heap, .. } =
            heap_with_pending_finalizations(3, AllocationSpace::Young, HeldValueKind::Integer);
        let mut isolate = test_isolate();
        let mut observed = Vec::new();

        let error = isolate
            .run_finalization_cleanup_safepoint(&mut heap, |_, _, job| {
                let held = job.held_value().as_i32().unwrap();
                observed.push(held);
                if held == 2 {
                    Err("cleanup throw")
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        assert_eq!(
            error,
            FinalizationSafepointError::Callback {
                error: "cleanup throw",
                stats: super::FinalizationSafepointStats {
                    scheduled_from_gc: 3,
                    callbacks_completed: 1,
                    queued_for_next_safepoint: 1,
                    deferred_in_gc: 0,
                },
            }
        );
        let resumed = isolate
            .run_finalization_cleanup_safepoint(&mut heap, |_, _, job| {
                observed.push(job.held_value().as_i32().unwrap());
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(observed, [1, 2, 3]);
        assert_eq!(resumed.scheduled_from_gc, 0);
        assert_eq!(resumed.callbacks_completed, 1);
    }

    #[test]
    fn recursive_cleanup_safepoint_is_rejected_without_disturbing_the_front_job() {
        let PendingFixture { mut heap, .. } =
            heap_with_pending_finalizations(1, AllocationSpace::Old, HeldValueKind::Integer);
        let mut isolate = test_isolate();

        let stats = isolate
            .run_finalization_cleanup_safepoint(&mut heap, |isolate, heap, _| {
                let nested =
                    isolate.run_finalization_cleanup_safepoint(heap, |_, _, _| Ok::<_, ()>(()));
                assert_eq!(nested, Err(FinalizationSafepointError::Reentrant));
                Ok::<_, ()>(())
            })
            .unwrap();

        assert_eq!(stats.callbacks_completed, 1);
        assert_eq!(isolate.finalization_job_queue_stats().queued, 0);
    }

    #[test]
    /// Callback-created records remain in the collector queue until a later VM safepoint.
    fn callback_collection_defers_new_finalizations_to_the_next_safepoint() {
        let PendingFixture {
            mut heap,
            leaf_type,
            registry_type,
            ..
        } = heap_with_pending_finalizations(1, AllocationSpace::Old, HeldValueKind::Integer);
        let mut isolate = test_isolate();

        let first = isolate
            .run_finalization_cleanup_safepoint(&mut heap, |isolate, heap, job| {
                assert_eq!(job.held_value().as_i32(), Some(1));
                let target = heap
                    .try_allocate(leaf_type, 0, 0, Leaf, AllocationSpace::Old)
                    .unwrap();
                let registry = heap
                    .try_allocate(
                        registry_type,
                        0,
                        0,
                        Registry {
                            registration: FinalizationRegistration::new(
                                target,
                                Value::from_i32(99),
                            ),
                        },
                        AllocationSpace::Old,
                    )
                    .unwrap();
                heap.collect_major(&mut CallbackRoots { isolate, registry })
                    .unwrap();
                Ok::<_, ()>(())
            })
            .unwrap();

        assert_eq!(first.callbacks_completed, 1);
        assert_eq!(first.queued_for_next_safepoint, 0);
        assert_eq!(first.deferred_in_gc, 1);
        let mut observed = Vec::new();
        let second = isolate
            .run_finalization_cleanup_safepoint(&mut heap, |_, _, job| {
                observed.push(job.held_value().as_i32().unwrap());
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(observed, [99]);
        assert_eq!(second.scheduled_from_gc, 1);
        assert_eq!(second.deferred_in_gc, 0);
    }
}
