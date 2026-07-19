//! Lexical environment records and direct captured-binding storage.

use super::super::*;

/// Runtime record category; object-environment behavior remains a future slow-path concern.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum EnvironmentKind {
    Declarative,
    Function,
    Global,
    Module,
}

impl EnvironmentKind {
    #[inline(always)]
    pub(crate) const fn for_activation(kind: FunctionKind, has_parent: bool) -> Self {
        match kind {
            FunctionKind::Script if has_parent => Self::Declarative,
            FunctionKind::Script => Self::Global,
            FunctionKind::Module => Self::Module,
            FunctionKind::Ordinary
            | FunctionKind::Generator
            | FunctionKind::Async
            | FunctionKind::AsyncGenerator => Self::Function,
        }
    }
}

/// Compact per-binding state used only when a record needs TDZ or immutable semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(crate) struct BindingState(u8);

impl BindingState {
    const MUTABLE: u8 = 1 << 0;
    const INITIALIZED: u8 = 1 << 1;
    #[inline(always)]
    pub(crate) const fn new(mutable: bool, initialized: bool) -> Self {
        Self(((mutable as u8) * Self::MUTABLE) | ((initialized as u8) * Self::INITIALIZED))
    }

    #[inline(always)]
    pub(crate) const fn is_mutable(self) -> bool {
        self.0 & Self::MUTABLE != 0
    }

    #[inline(always)]
    pub(crate) const fn is_initialized(self) -> bool {
        self.0 & Self::INITIALIZED != 0
    }

    #[inline(always)]
    const fn initialize(self) -> Self {
        Self(self.0 | Self::INITIALIZED)
    }
}

const _: [(); 1] = [(); core::mem::size_of::<BindingState>()];

/// Captured slots retain the existing direct array path; semantic bindings add parallel flags.
#[derive(Debug)]
enum EnvironmentStorage {
    Captured(Box<[Value]>),
    Bindings {
        values: Box<[Value]>,
        states: Box<[BindingState]>,
    },
}

/// A structured failure independent of source-name materialization on the direct slot path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EnvironmentAccessError {
    InvalidSlot,
    Uninitialized,
    Immutable,
    AlreadyInitialized,
}

#[derive(Debug)]
pub(crate) struct Environment {
    parent: Option<GcRef<Environment>>,
    kind: EnvironmentKind,
    storage: EnvironmentStorage,
}

impl Environment {
    /// Allocates the exact current captured-slot layout without per-slot metadata or name lookup.
    pub(crate) fn try_captured(
        kind: EnvironmentKind,
        parent: Option<GcRef<Self>>,
        slot_count: NonZeroU32,
    ) -> Result<Self, std::collections::TryReserveError> {
        let slot_count = usize::try_from(slot_count.get()).expect("u32 fits supported usize");
        let mut values = Vec::new();
        values.try_reserve_exact(slot_count)?;
        values.resize(slot_count, Value::from_immediate(Immediate::Undefined));
        Ok(Self {
            parent,
            kind,
            storage: EnvironmentStorage::Captured(values.into_boxed_slice()),
        })
    }

    /// Allocates exact binding values and state bytes for declarative TDZ/const semantics.
    pub(crate) fn try_bindings(
        kind: EnvironmentKind,
        parent: Option<GcRef<Self>>,
        slot_count: NonZeroU32,
        mut state_for_slot: impl FnMut(u32) -> BindingState,
    ) -> Result<Self, std::collections::TryReserveError> {
        let slot_count = usize::try_from(slot_count.get()).expect("u32 fits supported usize");
        let mut values = Vec::new();
        values.try_reserve_exact(slot_count)?;
        values.resize(slot_count, Value::from_immediate(Immediate::Undefined));
        let mut owned_states = Vec::new();
        owned_states.try_reserve_exact(slot_count)?;
        for index in 0..slot_count {
            owned_states.push(state_for_slot(index as u32));
        }
        Ok(Self {
            parent,
            kind,
            storage: EnvironmentStorage::Bindings {
                values: values.into_boxed_slice(),
                states: owned_states.into_boxed_slice(),
            },
        })
    }

    #[inline(always)]
    pub(crate) const fn kind(&self) -> EnvironmentKind {
        self.kind
    }

    #[inline(always)]
    pub(crate) const fn parent(&self) -> Option<GcRef<Self>> {
        self.parent
    }

    #[inline(always)]
    /// Reads a direct slot while enforcing TDZ for state-bearing records.
    pub(crate) fn load(&self, slot: u32) -> Result<Value, EnvironmentAccessError> {
        let index = slot as usize;
        match &self.storage {
            EnvironmentStorage::Captured(values) => values
                .get(index)
                .copied()
                .ok_or(EnvironmentAccessError::InvalidSlot),
            EnvironmentStorage::Bindings { values, states } => {
                let state = states
                    .get(index)
                    .copied()
                    .ok_or(EnvironmentAccessError::InvalidSlot)?;
                if !state.is_initialized() {
                    return Err(EnvironmentAccessError::Uninitialized);
                }
                Ok(values[index])
            }
        }
    }

    #[inline(always)]
    /// Assigns an initialized mutable slot; published environments require a caller write barrier.
    pub(crate) fn store(&mut self, slot: u32, value: Value) -> Result<(), EnvironmentAccessError> {
        let index = slot as usize;
        match &mut self.storage {
            EnvironmentStorage::Captured(values) => {
                let target = values
                    .get_mut(index)
                    .ok_or(EnvironmentAccessError::InvalidSlot)?;
                *target = value;
            }
            EnvironmentStorage::Bindings { values, states } => {
                let state = states
                    .get(index)
                    .copied()
                    .ok_or(EnvironmentAccessError::InvalidSlot)?;
                if !state.is_initialized() {
                    return Err(EnvironmentAccessError::Uninitialized);
                }
                if !state.is_mutable() {
                    return Err(EnvironmentAccessError::Immutable);
                }
                values[index] = value;
            }
        }
        Ok(())
    }

    /// Initializes one TDZ slot exactly once; published environments require a caller write barrier.
    pub(crate) fn initialize(
        &mut self,
        slot: u32,
        value: Value,
    ) -> Result<(), EnvironmentAccessError> {
        let index = slot as usize;
        let EnvironmentStorage::Bindings { values, states } = &mut self.storage else {
            return if index < self.slot_count() {
                Err(EnvironmentAccessError::AlreadyInitialized)
            } else {
                Err(EnvironmentAccessError::InvalidSlot)
            };
        };
        let state = states
            .get_mut(index)
            .ok_or(EnvironmentAccessError::InvalidSlot)?;
        if state.is_initialized() {
            return Err(EnvironmentAccessError::AlreadyInitialized);
        }
        values[index] = value;
        *state = state.initialize();
        Ok(())
    }

    #[inline(always)]
    fn slot_count(&self) -> usize {
        match &self.storage {
            EnvironmentStorage::Captured(values) | EnvironmentStorage::Bindings { values, .. } => {
                values.len()
            }
        }
    }
}

impl Trace for Environment {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.parent.trace(tracer);
        match &mut self.storage {
            EnvironmentStorage::Captured(values) | EnvironmentStorage::Bindings { values, .. } => {
                values.trace(tracer)
            }
        }
    }
}

impl GcExternalMemory for Environment {
    fn external_memory_bytes(&self) -> usize {
        let value_bytes = self
            .slot_count()
            .saturating_mul(core::mem::size_of::<Value>());
        let state_bytes = match &self.storage {
            EnvironmentStorage::Captured(_) => 0,
            EnvironmentStorage::Bindings { states, .. } => states
                .len()
                .saturating_mul(core::mem::size_of::<BindingState>()),
        };
        value_bytes.saturating_add(state_bytes)
    }
}
