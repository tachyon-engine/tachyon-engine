//! Promise state, reaction records, and the isolate-owned FIFO microtask substrate.

use std::collections::VecDeque;

use super::*;

struct PromiseCapabilityRoots<'a> {
    vm: VmRoots<'a>,
    promise: Value,
    cell: Option<GcRef<PromiseResolutionCell>>,
    resolve: Value,
    reject: Value,
}

impl Trace for PromiseCapabilityRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.promise.trace(tracer);
        self.cell.trace(tracer);
        self.resolve.trace(tracer);
        self.reject.trace(tracer);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum PromiseState {
    #[allow(dead_code, reason = "constructed by the Promise executor slice")]
    Pending,
    Fulfilled,
    Rejected,
}

/// One fixed-size reaction node. Linked nodes avoid reallocating a `Vec` inside a managed object.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromiseReaction {
    pub(crate) handler: Value,
    pub(crate) capability: Value,
    pub(crate) next: Option<GcRef<Self>>,
}

/// Shared one-shot state captured by the resolve and reject functions of one capability.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromiseResolutionCell {
    pub(crate) promise: Value,
    pub(crate) already_resolved: bool,
}

impl Trace for PromiseResolutionCell {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.promise.trace(tracer);
    }
}

impl Trace for PromiseReaction {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.handler.trace(tracer);
        self.capability.trace(tracer);
        self.next.trace(tracer);
    }
}

/// Promise exotic payload with an ordinary property base and allocation-stable reaction lists.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromiseObject {
    pub(crate) state: PromiseState,
    pub(crate) result: Value,
    pub(crate) fulfill_head: Option<GcRef<PromiseReaction>>,
    pub(crate) fulfill_tail: Option<GcRef<PromiseReaction>>,
    pub(crate) reject_head: Option<GcRef<PromiseReaction>>,
    pub(crate) reject_tail: Option<GcRef<PromiseReaction>>,
    pub(crate) ordinary: OrdinaryObject,
}

impl Trace for PromiseObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.result.trace(tracer);
        self.fulfill_head.trace(tracer);
        self.fulfill_tail.trace(tracer);
        self.reject_head.trace(tracer);
        self.reject_tail.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

#[allow(
    dead_code,
    reason = "consumed by the next Promise reaction execution slice"
)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum PromiseJob {
    Reaction {
        handler: Value,
        capability: Value,
        argument: Value,
        rejected: bool,
    },
    Thenable {
        promise: Value,
        thenable: Value,
        then: Value,
    },
}

impl Trace for PromiseJob {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        match self {
            Self::Reaction {
                handler,
                capability,
                argument,
                ..
            } => {
                handler.trace(tracer);
                capability.trace(tracer);
                argument.trace(tracer);
            }
            Self::Thenable {
                promise,
                thenable,
                then,
            } => {
                promise.trace(tracer);
                thenable.trace(tracer);
                then.trace(tracer);
            }
        }
    }
}

/// FIFO jobs are isolate-local and traced as roots until a checkpoint consumes them.
#[derive(Debug)]
pub(crate) struct PromiseJobQueue {
    jobs: VecDeque<PromiseJob>,
    active: Option<PromiseJob>,
}

impl PromiseJobQueue {
    pub(crate) fn new() -> Self {
        Self {
            jobs: VecDeque::with_capacity(tuning::promises::INITIAL_PROMISE_JOB_CAPACITY),
            active: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.jobs.len()
    }

    #[allow(
        dead_code,
        reason = "consumed by the next Promise reaction execution slice"
    )]
    pub(crate) fn push(&mut self, job: PromiseJob) {
        self.jobs.push_back(job);
    }

    /// Moves one job into a separately traced slot before any handler can allocate.
    #[allow(
        dead_code,
        reason = "consumed by the next Promise reaction execution slice"
    )]
    pub(crate) fn begin_next(&mut self) -> Option<PromiseJob> {
        debug_assert!(self.active.is_none());
        self.active = self.jobs.pop_front();
        self.active
    }

    #[allow(
        dead_code,
        reason = "consumed by the next Promise reaction execution slice"
    )]
    pub(crate) fn finish_active(&mut self) {
        self.active = None;
    }
}

impl Trace for PromiseJobQueue {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.active.trace(tracer);
        for job in &mut self.jobs {
            job.trace(tracer);
        }
    }
}

impl Isolate {
    /// Allocates one Promise with its state/result initialized before publication.
    pub(crate) fn create_promise(
        &mut self,
        state: PromiseState,
        result: Value,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .promise_prototype
            .expect("Promise prototype initializes before Promise allocation");
        self.create_promise_with_prototype(state, result, prototype)
    }

    /// Allocates a Promise with the prototype selected from the active constructor/newTarget.
    fn create_promise_with_prototype(
        &mut self,
        state: PromiseState,
        result: Value,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.promise_object,
                0,
                0,
                PromiseObject {
                    state,
                    result,
                    fulfill_head: None,
                    fulfill_tail: None,
                    reject_head: None,
                    reject_tail: None,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map(|promise| Value::from_heap_ref(promise.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Copies Promise state without retaining a heap borrow across an allocation.
    pub(crate) fn promise_snapshot(
        &mut self,
        value: Value,
    ) -> Result<PromiseObject, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let promise = self
            .heap
            .checked_reference(raw, self.types.promise_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let promise = scope.root(promise).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(promise, self.types.promise_object)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Creates the shared one-shot cell and the two strict native resolving callables.
    fn create_promise_capability_arguments(
        &mut self,
        promise: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut roots = PromiseCapabilityRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            promise,
            cell: None,
            resolve: undefined,
            reject: undefined,
        };
        let cell = self
            .heap
            .try_allocate_with_gc(
                self.types.promise_resolution_cell,
                0,
                0,
                PromiseResolutionCell {
                    promise: roots.promise,
                    already_resolved: false,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.cell = Some(cell);
        let prototype = roots
            .vm
            .realm
            .function_prototype
            .expect("Function prototype initializes before Promise resolvers");
        roots.resolve = allocate_promise_resolver(
            &mut self.heap,
            self.types.function,
            cell,
            false,
            prototype,
            &mut roots,
        )?;
        roots.reject = allocate_promise_resolver(
            &mut self.heap,
            self.types.function,
            cell,
            true,
            prototype,
            &mut roots,
        )?;
        let values = [
            roots.resolve,
            roots.reject,
            roots.promise,
            undefined,
            undefined,
        ];
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                NativeCallState { values, count: 2 },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Calls the executor through the VM trampoline and retains the result Promise in its continuation.
    pub(crate) fn begin_promise_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let executor = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.resolve_function_object(executor)?;
        let prototype_atom = self.prototype_atom()?;
        let prototype = self
            .get_data_property(site.new_target, prototype_atom)?
            .filter(|prototype| self.is_object_value(*prototype))
            .unwrap_or(
                self.realm
                    .promise_prototype
                    .expect("Promise prototype initializes before construction"),
            );
        let promise = self.create_promise_with_prototype(
            PromiseState::Pending,
            Value::from_immediate(Immediate::Undefined),
            prototype,
        )?;
        self.write(site.caller_base, site.destination, promise)?;
        let arguments = self.create_promise_capability_arguments(promise)?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.fiber
            .completions
            .push_native(NativeContinuation::promise_executor(
                continuation_site,
                promise,
                Value::from_heap_ref(arguments.raw()),
            ))
            .map_err(Isolate::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = self.call(CallSite {
            caller_base: site.caller_base,
            destination: site.destination,
            callee: executor,
            argument_base: 0,
            argument_source: Some(arguments),
            argument_prefix: None,
            argument_prefix_offset: 0,
            argument_prefix_count: 0,
            argument_count: 2,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: Value::from_immediate(Immediate::Undefined),
            construct_receiver: None,
            call_site: site.call_site,
        }) {
            self.pop_native_continuation()?;
            if let Some(kind) = execution_error_kind(&error) {
                let thrown = self.create_native_error(kind, None)?;
                self.settle_promise(promise, PromiseState::Rejected, thrown)?;
                return self.write(site.caller_base, site.destination, promise);
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            let frame = self
                .fiber
                .frames
                .last_mut()
                .expect("Promise executor bytecode call publishes a frame");
            frame.return_register = None;
            frame.return_continuation = true;
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        debug_assert_eq!(continuation.kind(), NativeContinuationKind::PromiseExecutor);
        self.write(site.caller_base, site.destination, promise)
    }

    /// Applies the shared already-resolved guard and settles primitive resolutions immediately.
    pub(crate) fn call_promise_resolver(
        &mut self,
        cell: GcRef<PromiseResolutionCell>,
        reject: bool,
        resolution: Value,
    ) -> Result<(), ExecutionError> {
        let promise = self.heap.with_running_scope(|scope| {
            let cell = scope.root(cell).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let cell = no_gc
                    .borrow_mut(cell, self.types.promise_resolution_cell)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if cell.already_resolved {
                    return Ok(None);
                }
                cell.already_resolved = true;
                Ok(Some(cell.promise))
            })
        })?;
        let Some(promise) = promise else {
            return Ok(());
        };
        if reject {
            return self.settle_promise(promise, PromiseState::Rejected, resolution);
        }
        if promise == resolution {
            let error = self.create_native_error(NativeErrorKind::Type, None)?;
            return self.settle_promise(promise, PromiseState::Rejected, error);
        }
        self.settle_promise(promise, PromiseState::Fulfilled, resolution)
    }

    /// Transitions a pending Promise exactly once and publishes its result through the GC barrier.
    pub(crate) fn settle_promise(
        &mut self,
        promise: Value,
        state: PromiseState,
        result: Value,
    ) -> Result<(), ExecutionError> {
        debug_assert_ne!(state, PromiseState::Pending);
        let raw = promise
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(promise))?;
        let promise = self
            .heap
            .checked_reference(raw, self.types.promise_object)
            .map_err(|_| ExecutionError::NotObject(promise))?;
        self.heap.with_running_scope(|scope| {
            let promise = scope.root(promise).map_err(ExecutionError::Root)?;
            let changed = scope.with_no_gc_scope(|no_gc| {
                let promise = no_gc
                    .borrow_mut(promise, self.types.promise_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                if promise.state != PromiseState::Pending {
                    return Ok(false);
                }
                promise.state = state;
                promise.result = result;
                Ok(true)
            })?;
            if changed {
                scope
                    .write_value_barrier(promise, result)
                    .map_err(ExecutionError::HeapReference)?;
            }
            Ok(())
        })
    }
}

/// Allocates one resolver while capability siblings remain in the caller-owned root set.
fn allocate_promise_resolver(
    heap: &mut Heap,
    function_type: GcType<FunctionObject>,
    cell: GcRef<PromiseResolutionCell>,
    reject: bool,
    prototype: Value,
    roots: &mut PromiseCapabilityRoots<'_>,
) -> Result<Value, ExecutionError> {
    heap.try_allocate_with_gc(
        function_type,
        0,
        0,
        FunctionObject {
            executable: FunctionExecutable::PromiseResolver { cell, reject },
            function_prototype: None,
            ordinary: OrdinaryObject {
                shape: ShapeId::EMPTY,
                extensible: true,
                storage: None,
                prototype,
            },
        },
        AllocationSpace::Young,
        roots,
    )
    .map(|function| Value::from_heap_ref(function.raw()))
    .map_err(ExecutionError::HeapAllocation)
}

#[cfg(test)]
mod tests {
    use tachyon_gc::{ForcedCollectionMode, SPAN_SIZE_BYTES};

    use super::*;

    fn test_isolate() -> Isolate {
        Isolate::new(IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
            HeapLimit::new(16 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            RealmLimits::new(64, 1_024).with_max_shapes(384),
        ))
        .unwrap()
    }

    #[test]
    fn promise_jobs_move_through_the_traced_active_slot_in_fifo_order() {
        let mut queue = PromiseJobQueue::new();
        queue.push(PromiseJob::Reaction {
            handler: Value::from_i32(1),
            capability: Value::from_i32(2),
            argument: Value::from_i32(3),
            rejected: false,
        });
        queue.push(PromiseJob::Thenable {
            promise: Value::from_i32(4),
            thenable: Value::from_i32(5),
            then: Value::from_i32(6),
        });
        assert_eq!(queue.len(), 2);
        assert!(matches!(
            queue.begin_next(),
            Some(PromiseJob::Reaction { argument, .. }) if argument.as_i32() == Some(3)
        ));
        assert_eq!(queue.len(), 1);
        queue.finish_active();
        assert!(matches!(
            queue.begin_next(),
            Some(PromiseJob::Thenable { then, .. }) if then.as_i32() == Some(6)
        ));
    }

    #[test]
    fn resolving_functions_share_the_first_call_guard_across_forced_major() {
        let mut isolate = test_isolate();
        let promise = isolate
            .create_promise(
                PromiseState::Pending,
                Value::from_immediate(Immediate::Undefined),
            )
            .unwrap();
        let arguments = isolate
            .create_promise_capability_arguments(promise)
            .unwrap();
        let arguments = isolate.native_call_state_snapshot(arguments).unwrap();
        let resolve = arguments.values[0];
        let reject = arguments.values[1];
        isolate.fiber.registers = vec![promise, resolve, reject];
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let FunctionExecutable::PromiseResolver {
            cell,
            reject: false,
        } = isolate.resolve_function_object(resolve).unwrap().executable
        else {
            panic!("resolve capability must use the shared cell")
        };
        isolate
            .call_promise_resolver(cell, false, Value::from_i32(7))
            .unwrap();
        let FunctionExecutable::PromiseResolver { cell, reject: true } =
            isolate.resolve_function_object(reject).unwrap().executable
        else {
            panic!("reject capability must use the shared cell")
        };
        isolate
            .call_promise_resolver(cell, true, Value::from_i32(9))
            .unwrap();
        let snapshot = isolate.promise_snapshot(promise).unwrap();
        assert_eq!(snapshot.state, PromiseState::Fulfilled);
        assert_eq!(snapshot.result.as_i32(), Some(7));
    }
}
