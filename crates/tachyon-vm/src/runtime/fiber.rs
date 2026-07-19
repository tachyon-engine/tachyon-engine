//! Explicit fiber, frame, continuation, and GC root state.

use super::super::*;
use super::completion::CompletionStack;

pub(crate) struct VmRoots<'a> {
    pub(crate) fiber: &'a mut Fiber,
    pub(crate) finalization_jobs: &'a mut finalization::FinalizationJobs,
    pub(crate) realm: &'a mut Realm,
    pub(crate) loaded_code: &'a mut Vec<LoadedCode>,
}

pub(crate) struct PropertyMutationRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) receiver: Value,
    pub(crate) value: Value,
    pub(crate) symbol_key: Option<Value>,
}

pub(crate) struct SymbolAllocationRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) description: Option<Value>,
}

pub(crate) struct PrototypeInitializationRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) function: Value,
}

pub(crate) struct ArrayAllocationRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) prototype: Value,
}

impl Trace for PropertyMutationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.receiver.trace(tracer);
        self.value.trace(tracer);
        self.symbol_key.trace(tracer);
    }
}

impl Trace for SymbolAllocationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.description.trace(tracer);
    }
}

impl Trace for PrototypeInitializationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.function.trace(tracer);
    }
}

impl Trace for ArrayAllocationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.prototype.trace(tracer);
    }
}

impl Trace for VmRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.fiber.trace_roots(tracer);
        self.finalization_jobs.trace(tracer);
        self.realm.trace(tracer);
        for code in self.loaded_code.iter_mut() {
            code.trace(tracer);
        }
    }
}

pub(crate) struct CodeLoadRoots<'a> {
    pub(crate) vm: VmRoots<'a>,
    pub(crate) constant_values: &'a mut Vec<Option<Value>>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NativeContinuationSite {
    pub(crate) caller_base: u32,
    pub(crate) destination: u32,
    pub(crate) call_site: WordOffset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToPrimitiveStage {
    ValueOf,
    ToString,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConversionCallbackStage {
    Getter,
    MethodCall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConversionConsumer {
    NativeCall(NativeFunction),
    NativeConstruct(NativeFunction),
    ToNumber,
    Negate,
    BitwiseNot,
    BinaryLeft(Opcode),
    BinaryRight(Opcode),
    AddLeft,
    AddRight,
    RelationalLeft(Opcode),
    RelationalRight(Opcode),
}

impl ConversionConsumer {
    #[inline]
    pub(crate) const fn native(self) -> Option<NativeFunction> {
        match self {
            Self::NativeCall(native) | Self::NativeConstruct(native) => Some(native),
            Self::ToNumber
            | Self::Negate
            | Self::BitwiseNot
            | Self::BinaryLeft(_)
            | Self::BinaryRight(_)
            | Self::AddLeft
            | Self::AddRight
            | Self::RelationalLeft(_)
            | Self::RelationalRight(_) => None,
        }
    }

    #[inline]
    pub(crate) const fn uses_string_hint(self) -> bool {
        matches!(self, Self::NativeCall(NativeFunction::StringConstructor))
    }

    #[inline]
    pub(crate) const fn is_opcode_conversion(self) -> bool {
        matches!(
            self,
            Self::ToNumber
                | Self::Negate
                | Self::BitwiseNot
                | Self::BinaryLeft(_)
                | Self::BinaryRight(_)
                | Self::AddLeft
                | Self::AddRight
                | Self::RelationalLeft(_)
                | Self::RelationalRight(_)
        )
    }
}

#[inline]
pub(crate) fn next_to_primitive_stage(
    consumer: ConversionConsumer,
    stage: ToPrimitiveStage,
) -> Option<ToPrimitiveStage> {
    if consumer.uses_string_hint() {
        match stage {
            ToPrimitiveStage::ToString => Some(ToPrimitiveStage::ValueOf),
            ToPrimitiveStage::ValueOf => None,
        }
    } else {
        match stage {
            ToPrimitiveStage::ValueOf => Some(ToPrimitiveStage::ToString),
            ToPrimitiveStage::ToString => None,
        }
    }
}

/// Resumable ordinary conversion state retained while one JavaScript method callback executes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConversionContinuation {
    pub(crate) site: NativeContinuationSite,
    pub(crate) consumer: ConversionConsumer,
    pub(crate) receiver: Value,
    pub(crate) object: Value,
    pub(crate) stage: ToPrimitiveStage,
    pub(crate) callback_stage: ConversionCallbackStage,
}

/// Typed callback work owned by a JavaScript frame instead of the Rust call stack.
#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeContinuation {
    Conversion(ConversionContinuation),
    PropertyGet {
        site: NativeContinuationSite,
        receiver: Value,
        callee: Value,
    },
    PropertySet {
        site: NativeContinuationSite,
        receiver: Value,
        value: Value,
        callee: Value,
    },
}

impl NativeContinuation {
    #[inline(always)]
    pub(crate) const fn site(self) -> NativeContinuationSite {
        match self {
            Self::Conversion(continuation) => continuation.site,
            Self::PropertyGet { site, .. } | Self::PropertySet { site, .. } => site,
        }
    }
}

impl Trace for NativeContinuation {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        match self {
            Self::Conversion(continuation) => {
                continuation.receiver.trace(tracer);
                continuation.object.trace(tracer);
            }
            Self::PropertyGet {
                receiver, callee, ..
            } => {
                receiver.trace(tracer);
                callee.trace(tracer);
            }
            Self::PropertySet {
                receiver,
                value,
                callee,
                ..
            } => {
                receiver.trace(tracer);
                value.trace(tracer);
                callee.trace(tracer);
            }
        }
    }
}

impl Trace for CodeLoadRoots<'_> {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        for value in self.constant_values.iter_mut().flatten() {
            value.trace(tracer);
        }
    }
}

/// One explicit JavaScript activation. Rust stack frames never represent JavaScript calls.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame {
    pub(crate) code: CodeId,
    pub(crate) function: FunctionId,
    pub(crate) pc: WordOffset,
    pub(crate) base: u32,
    pub(crate) environment: Option<GcRef<Environment>>,
    pub(crate) return_register: Option<RegisterId>,
    pub(crate) return_continuation: bool,
    pub(crate) this_value: Value,
    pub(crate) new_target: Value,
    pub(crate) construct_receiver: Option<Value>,
    pub(crate) strictness: FunctionStrictness,
    pub(crate) has_finally: bool,
    pub(crate) argument_base: u32,
    pub(crate) argument_prefix: Option<GcRef<BoundFunctionData>>,
    pub(crate) argument_prefix_offset: u32,
    pub(crate) argument_prefix_count: u32,
    pub(crate) argument_count: u32,
    pub(crate) handler_base: u32,
    pub(crate) completion_base: u32,
    pub(crate) call_site: Option<WordOffset>,
}

const _: [(); 104] = [(); core::mem::size_of::<Frame>()];

/// The dynamic handler state selected from immutable bytecode handler metadata.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ActiveHandler {
    pub(crate) handler_index: u32,
    pub(crate) frame_depth: u32,
    pub(crate) environment_depth: u32,
}

#[derive(Debug, Default)]
pub(crate) struct Fiber {
    pub(crate) frames: Vec<Frame>,
    pub(crate) registers: Vec<Value>,
    pub(crate) handlers: Vec<ActiveHandler>,
    pub(crate) completions: CompletionStack,
    pub(crate) pending_exception: Option<Value>,
}

impl Fiber {
    /// Traces every mutable reference reachable from an active, yielded, or suspended fiber.
    ///
    /// Frame control indices are validated when handlers are installed. They do not themselves
    /// own heap references, while registers, frame context, and abrupt completion payloads do.
    pub(crate) fn trace_roots(&mut self, tracer: &mut dyn Tracer) {
        self.registers.trace(tracer);
        for frame in &mut self.frames {
            frame.environment.trace(tracer);
            frame.this_value.trace(tracer);
            frame.new_target.trace(tracer);
            frame.construct_receiver.trace(tracer);
            frame.argument_prefix.trace(tracer);
            if let Some(return_register) = frame.return_register {
                debug_assert!((return_register.index() as usize) < self.registers.len());
            }
            debug_assert!(frame.argument_prefix_count <= frame.argument_count);
            debug_assert!(
                frame.argument_prefix.is_some()
                    || (frame.argument_prefix_offset == 0 && frame.argument_prefix_count == 0)
            );
            debug_assert!(
                frame
                    .argument_base
                    .checked_add(
                        frame
                            .argument_count
                            .saturating_sub(frame.argument_prefix_count),
                    )
                    .is_some_and(|end| end as usize <= self.registers.len())
            );
            let _is_strict = matches!(frame.strictness, FunctionStrictness::Strict);
        }
        for handler in &self.handlers {
            debug_assert!(
                usize::try_from(handler.frame_depth).is_ok_and(|depth| depth <= self.frames.len())
            );
            debug_assert!(
                usize::try_from(handler.environment_depth)
                    .is_ok_and(|depth| depth <= self.frames.len())
            );
            let _ = handler.handler_index;
        }
        self.completions.trace(tracer);
        self.pending_exception.trace(tracer);
    }
}
