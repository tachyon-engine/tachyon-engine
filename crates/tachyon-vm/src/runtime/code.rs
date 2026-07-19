//! Loaded bytecode identities and verified execution cursor state.

use super::super::*;

/// An isolate-local immutable-code index; zero stays reserved for niche optimization and validation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct CodeId(NonZeroU32);

impl CodeId {
    pub(crate) fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
    }

    pub(crate) const fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

const _: [(); 4] = [(); core::mem::size_of::<CodeId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<CodeId>>()];

#[derive(Debug)]
pub(crate) struct LoadedCode {
    pub(crate) module: CompiledModule,
    pub(crate) scope_resolutions: Box<[ScopeResolution]>,
    pub(crate) constant_values: Box<[Option<Value>]>,
}

/// A batch-local view into verified immutable bytecode retained by the active `LoadedCode` module.
#[derive(Clone, Copy)]
pub(crate) struct BytecodeCursor {
    pub(crate) decoder: VerifiedInstructionDecoder<'static>,
    #[cfg(test)]
    pub(crate) bytecode: NonNull<VerifiedBytecode>,
}

impl BytecodeCursor {
    /// Captures one stable verified function without incrementing its backing reference counts.
    ///
    /// # Safety
    ///
    /// The owner of `bytecode` and its immutable word backing must outlive every use of the returned
    /// cursor. Moving the owner is allowed only when its verified functions remain in stable Arc
    /// storage; dropping or replacing that backing invalidates the cursor.
    pub(crate) unsafe fn new(bytecode: &VerifiedBytecode) -> Self {
        let decoder = VerifiedInstructionDecoder::new(bytecode);
        // SAFETY: This erases only the type-level borrow so mutable isolate slow paths can run. The
        // caller guarantees the backing owner outlives every use of the erased decoder.
        let decoder = unsafe {
            core::mem::transmute::<
                VerifiedInstructionDecoder<'_>,
                VerifiedInstructionDecoder<'static>,
            >(decoder)
        };
        Self {
            decoder,
            #[cfg(test)]
            bytecode: NonNull::from(bytecode),
        }
    }

    /// Decodes one verifier-proven instruction while the loaded module retains its immutable owner.
    ///
    /// # Safety
    ///
    /// `offset` must be an instruction start in the same verified bytecode passed to `new`, and that
    /// bytecode's owner must still be alive.
    #[inline(always)]
    pub(crate) unsafe fn decode(self, offset: WordOffset) -> DecodedInstruction {
        #[cfg(test)]
        {
            // SAFETY: `BytecodeCursor::new` requires the verified owner to outlive this cursor use.
            let bytecode = unsafe { self.bytecode.as_ref() };
            assert!(bytecode.is_instruction_start(offset));
        }
        // SAFETY: active frame PCs originate from verified fallthrough/jump/handler targets. Slow
        // exits publish one such PC before mutation; the caller carries that instruction-start proof.
        unsafe { self.decoder.decode_unchecked(offset) }
    }
}

/// Raw view of one verified activation's register window during a no-reallocation kernel epoch.
pub(crate) struct RegisterWindow {
    pub(crate) start: NonNull<Value>,
    pub(crate) len: usize,
}

impl RegisterWindow {
    /// Checks the activation boundary once before verified operands use unchecked slot access.
    pub(crate) fn new(registers: &mut [Value], base: usize, len: usize) -> Option<Self> {
        let end = base.checked_add(len)?;
        let window = registers.get_mut(base..end)?;
        Some(Self {
            start: NonNull::new(window.as_mut_ptr())
                .expect("slice pointers are non-null even for empty slices"),
            len,
        })
    }

    /// Reads an operand already proven in range by module verification and cursor entry.
    ///
    /// # Safety
    ///
    /// `register` must be below this window's verified length, and the owning register storage must
    /// not have been resized, reserved, truncated, or dropped since `RegisterWindow::new`.
    #[inline(always)]
    pub(crate) unsafe fn read(&self, register: u32) -> Value {
        let index = register as usize;
        debug_assert!(index < self.len);
        // SAFETY: The caller upholds the verified operand and no-reallocation epoch invariants.
        unsafe { *self.start.as_ptr().add(index) }
    }

    /// Writes an operand already proven in range without exposing a reference outside the cursor.
    ///
    /// # Safety
    ///
    /// `register` must be below this window's verified length, and this cursor must retain exclusive
    /// write access to the owning register storage for the complete no-reallocation epoch.
    #[inline(always)]
    pub(crate) unsafe fn write(&mut self, register: u32, value: Value) {
        let index = register as usize;
        debug_assert!(index < self.len);
        // SAFETY: The caller upholds the verified operand, exclusivity, and storage lifetime rules.
        unsafe { self.start.as_ptr().add(index).write(value) };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HotControl {
    Continue,
    Slow,
}

#[cfg(feature = "opcode-profile")]
#[inline(always)]
pub(crate) const fn is_conditional_branch(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::JumpIfFalse | Opcode::JumpIfTrue | Opcode::JumpIfNotNullish
    )
}

impl Trace for LoadedCode {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for value in self.constant_values.iter_mut().flatten() {
            value.trace(tracer);
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ScopeResolution {
    pub(crate) atom: AtomId,
    pub(crate) lexical_slot: Option<GlobalLexicalSlotId>,
    pub(crate) intrinsic_slot: Option<IntrinsicSlotId>,
    pub(crate) global_slot: Option<GlobalSlotId>,
}
