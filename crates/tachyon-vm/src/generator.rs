//! Generator instance state and the retained call payload used by first resume.

use tachyon_gc::{GcRef, Trace, Tracer};
use tachyon_value::Value;

use crate::{
    AllocationSpace, ExecutionError, FunctionKind, Immediate, Isolate, ShapeId,
    bound_function::BoundFunctionData,
    object::OrdinaryObject,
    runtime::{
        callable::{CallSite, ResolvedCallTarget},
        code::CodeId,
        environment::Environment,
        fiber::{NativeContinuation, NativeContinuationSite, VmRoots},
    },
};
use tachyon_bytecode::FunctionId;

/// Spec-visible generator execution state. `SuspendedYield` is reserved for the yield slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum GeneratorState {
    SuspendedStart,
    #[allow(
        dead_code,
        reason = "reserved for the verified Yield opcode integration"
    )]
    SuspendedYield,
    Executing,
    Completed,
}

/// Fixed-size roots retained only until the first bytecode frame is successfully published.
#[derive(Clone, Copy, Debug)]
struct GeneratorActivation {
    environment: Option<GcRef<Environment>>,
    this_value: Value,
    callee: Value,
    argument_prefix: GcRef<BoundFunctionData>,
    argument_count: u32,
}

impl Trace for GeneratorActivation {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.environment.trace(tracer);
        self.this_value.trace(tracer);
        self.callee.trace(tracer);
        self.argument_prefix.trace(tracer);
    }
}

/// GC-managed generator instance with a one-shot retained activation.
///
/// The first vertical slice retains the initial activation inputs until `.next()` publishes the
/// explicit bytecode frame. A later yield implementation will add a paused-fiber handle without
/// changing the public state machine or replaying the function body.
#[derive(Debug)]
pub(crate) struct GeneratorObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) code: CodeId,
    pub(crate) function: FunctionId,
    activation: Option<GeneratorActivation>,
    pub(crate) state: GeneratorState,
}

impl GeneratorObject {
    /// Creates one suspended-start activation with fixed backing and no hidden capacity.
    fn new(
        ordinary: OrdinaryObject,
        code: CodeId,
        function: FunctionId,
        activation: GeneratorActivation,
    ) -> Self {
        Self {
            ordinary,
            code,
            function,
            activation: Some(activation),
            state: GeneratorState::SuspendedStart,
        }
    }

    #[inline(always)]
    fn header(&self) -> GeneratorHeader {
        GeneratorHeader {
            code: self.code,
            function: self.function,
            state: self.state,
        }
    }
}

impl Trace for GeneratorObject {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.activation.trace(tracer);
    }
}

#[derive(Clone, Copy, Debug)]
struct GeneratorHeader {
    code: CodeId,
    function: FunctionId,
    state: GeneratorState,
}

impl Isolate {
    /// Captures a generator call without entering its body or publishing a JavaScript frame.
    pub(crate) fn create_generator_from_site(
        &mut self,
        site: &CallSite,
        target: ResolvedCallTarget,
    ) -> Result<Value, ExecutionError> {
        let prototype = self.ensure_function_prototype(site.callee)?;
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(site.argument_count as usize)
            .map_err(|_| ExecutionError::GeneratorArgumentAllocationFailed)?;
        for index in 0..site.argument_count {
            arguments.push(
                self.call_argument(site, index)?
                    .expect("generator call argument remains in the call view"),
            );
        }
        let this_value = self.bind_ordinary_this(target.strictness, site.this_value);
        let argument_prefix =
            self.create_apply_argument_prefix(site.callee, this_value, arguments)?;
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.generator_object,
                0,
                0,
                GeneratorObject::new(
                    OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                    target.code,
                    target.function,
                    GeneratorActivation {
                        environment: target.environment,
                        this_value,
                        callee: site.callee,
                        argument_prefix,
                        argument_count: site.argument_count,
                    },
                ),
                AllocationSpace::Young,
                roots,
            )
            .map(|generator| Value::from_heap_ref(generator.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Starts a suspended generator on the existing iterative frame machine.
    pub(crate) fn begin_generator_next(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let generator = self.generator_reference(site.this_value)?;
        let header = self.generator_header(generator)?;
        match header.state {
            GeneratorState::Completed => {
                let result =
                    self.create_iterator_result(Value::from_immediate(Immediate::Undefined), true)?;
                return self.write(site.caller_base, site.destination, result);
            }
            GeneratorState::Executing => return Err(ExecutionError::GeneratorExecuting),
            GeneratorState::SuspendedYield => {
                return Err(ExecutionError::UnsupportedGeneratorYieldResume);
            }
            GeneratorState::SuspendedStart => {}
        }
        let activation = self.generator_activation(generator)?;
        let (layout, strictness) = {
            let function = self
                .loaded_code(header.code)?
                .module
                .function(header.function)
                .ok_or(ExecutionError::MissingEntryFunction(header.function))?;
            (function.layout(), function.strictness())
        };
        let continuation = NativeContinuation::generator_resume(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            site.this_value,
        );
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        self.set_generator_state(generator, GeneratorState::Executing)?;
        let pushed = self.push_call_frame(
            ResolvedCallTarget {
                code: header.code,
                function: header.function,
                environment: activation.environment,
                kind: FunctionKind::Generator,
                layout,
                strictness,
            },
            CallSite {
                caller_base: site.caller_base,
                destination: site.destination,
                callee: activation.callee,
                argument_base: 0,
                argument_source: None,
                argument_prefix: Some(activation.argument_prefix),
                argument_prefix_offset: 0,
                argument_prefix_count: activation.argument_count,
                argument_count: activation.argument_count,
                this_value: activation.this_value,
                new_target: Value::from_immediate(Immediate::Undefined),
                construct_receiver: None,
                call_site: site.call_site,
            },
        );
        if let Err(error) = pushed {
            self.set_generator_state(generator, GeneratorState::SuspendedStart)?;
            self.pop_native_continuation()?;
            return Err(error);
        }
        self.clear_generator_activation(generator)?;
        let frame = self
            .fiber
            .frames
            .last_mut()
            .expect("generator resume publishes one bytecode frame");
        frame.return_register = None;
        frame.return_continuation = true;
        Ok(())
    }

    /// Completes a generator return before creating the observable iterator result object.
    pub(crate) fn finish_generator_return(
        &mut self,
        continuation: NativeContinuation,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let generator = self.generator_reference(continuation.first())?;
        self.set_generator_state(generator, GeneratorState::Completed)?;
        let result = self.create_iterator_result(value, true)?;
        let site = continuation.site();
        self.write(site.caller_base, site.destination, result)
    }

    /// Marks an abruptly completed generator before the original throw continues in its caller.
    pub(crate) fn finish_generator_throw(
        &mut self,
        continuation: NativeContinuation,
    ) -> Result<(), ExecutionError> {
        let generator = self.generator_reference(continuation.first())?;
        self.set_generator_state(generator, GeneratorState::Completed)
    }

    fn generator_reference(&self, value: Value) -> Result<GcRef<GeneratorObject>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::GeneratorBrand(value))?;
        self.heap
            .checked_reference(raw, self.types.generator_object)
            .map_err(|_| ExecutionError::GeneratorBrand(value))
    }

    /// Reads only the compact execution header used by every `.next()` state branch.
    fn generator_header(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<GeneratorHeader, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let generator = no_gc
                    .borrow(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(generator.header())
            })
        })
    }

    /// Copies the fixed activation header only for a verified suspended-start generator.
    fn generator_activation(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<GeneratorActivation, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .activation
                    .ok_or(ExecutionError::UnsupportedGeneratorYieldResume)
            })
        })
    }

    /// Releases the creation-time roots after their ownership is visible through the frame.
    fn clear_generator_activation(
        &mut self,
        generator: GcRef<GeneratorObject>,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .activation = None;
                Ok(())
            })
        })
    }

    /// Reports the exact immutable argument backing retained by a generator in VM tests.
    #[cfg(test)]
    pub(crate) fn generator_retained_argument_capacity(
        &mut self,
        value: Value,
    ) -> Result<usize, ExecutionError> {
        let generator = self.generator_reference(value)?;
        let argument_prefix = self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let generator = no_gc
                    .borrow(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(generator
                    .activation
                    .map(|activation| activation.argument_prefix))
            })
        })?;
        let Some(argument_prefix) = argument_prefix else {
            return Ok(0);
        };
        self.heap.with_running_scope(|scope| {
            let argument_prefix = scope.root(argument_prefix).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(argument_prefix, self.types.bound_function)
                    .map(|prefix| prefix.arguments.len())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn set_generator_state(
        &mut self,
        generator: GcRef<GeneratorObject>,
        state: GeneratorState,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let generator = scope.root(generator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(generator, self.types.generator_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .state = state;
                Ok(())
            })
        })
    }
}
