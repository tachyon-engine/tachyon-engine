//! Agent-wide identities shared by every Realm in one isolate.

use super::super::*;

/// One entry in the ECMAScript Agent's global Symbol registry.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RegisteredSymbol {
    pub(crate) key: AtomId,
    pub(crate) serial: NonZeroU32,
    pub(crate) root: PersistentRootId<SymbolValue>,
}

/// Stable indices for the well-known Symbol set shared by every Realm in one Agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum WellKnownSymbolId {
    AsyncDispose,
    AsyncIterator,
    Dispose,
    HasInstance,
    IsConcatSpreadable,
    Iterator,
    Match,
    MatchAll,
    Replace,
    Search,
    Species,
    Split,
    ToPrimitive,
    ToStringTag,
    Unscopables,
}

/// Hot Value view of the Agent's persistent well-known Symbol roots.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct WellKnownSymbols {
    pub(crate) async_dispose: Option<Value>,
    pub(crate) async_iterator: Option<Value>,
    pub(crate) dispose: Option<Value>,
    pub(crate) has_instance: Option<Value>,
    pub(crate) is_concat_spreadable: Option<Value>,
    pub(crate) iterator: Option<Value>,
    pub(crate) r#match: Option<Value>,
    pub(crate) match_all: Option<Value>,
    pub(crate) replace: Option<Value>,
    pub(crate) search: Option<Value>,
    pub(crate) species: Option<Value>,
    pub(crate) split: Option<Value>,
    pub(crate) to_primitive: Option<Value>,
    pub(crate) to_string_tag: Option<Value>,
    pub(crate) unscopables: Option<Value>,
}

impl WellKnownSymbols {
    /// Selects the one closed-set slot published together with its persistent root.
    fn slot_mut(&mut self, id: WellKnownSymbolId) -> &mut Option<Value> {
        match id {
            WellKnownSymbolId::AsyncDispose => &mut self.async_dispose,
            WellKnownSymbolId::AsyncIterator => &mut self.async_iterator,
            WellKnownSymbolId::Dispose => &mut self.dispose,
            WellKnownSymbolId::HasInstance => &mut self.has_instance,
            WellKnownSymbolId::IsConcatSpreadable => &mut self.is_concat_spreadable,
            WellKnownSymbolId::Iterator => &mut self.iterator,
            WellKnownSymbolId::Match => &mut self.r#match,
            WellKnownSymbolId::MatchAll => &mut self.match_all,
            WellKnownSymbolId::Replace => &mut self.replace,
            WellKnownSymbolId::Search => &mut self.search,
            WellKnownSymbolId::Species => &mut self.species,
            WellKnownSymbolId::Split => &mut self.split,
            WellKnownSymbolId::ToPrimitive => &mut self.to_primitive,
            WellKnownSymbolId::ToStringTag => &mut self.to_string_tag,
            WellKnownSymbolId::Unscopables => &mut self.unscopables,
        }
    }
}

impl WellKnownSymbolId {
    pub(crate) const COUNT: usize = Self::Unscopables as usize + 1;
    #[cfg(test)]
    pub(crate) const ALL: [Self; Self::COUNT] = [
        Self::AsyncDispose,
        Self::AsyncIterator,
        Self::Dispose,
        Self::HasInstance,
        Self::IsConcatSpreadable,
        Self::Iterator,
        Self::Match,
        Self::MatchAll,
        Self::Replace,
        Self::Search,
        Self::Species,
        Self::Split,
        Self::ToPrimitive,
        Self::ToStringTag,
        Self::Unscopables,
    ];

    #[inline(always)]
    const fn index(self) -> usize {
        self as usize
    }
}

/// Isolate-owned ECMAScript Agent state; managed identities live in persistent GC roots.
#[derive(Debug)]
pub(crate) struct AgentState {
    pub(crate) registered_symbols: Vec<RegisteredSymbol>,
    pub(crate) well_known_symbols: WellKnownSymbols,
    well_known_roots: [Option<PersistentRootId<SymbolValue>>; WellKnownSymbolId::COUNT],
}

impl AgentState {
    #[inline(always)]
    pub(crate) const fn well_known(
        &self,
        id: WellKnownSymbolId,
    ) -> Option<PersistentRootId<SymbolValue>> {
        self.well_known_roots[id.index()]
    }

    #[inline(always)]
    pub(crate) fn set_well_known(
        &mut self,
        id: WellKnownSymbolId,
        value: Value,
        root: PersistentRootId<SymbolValue>,
    ) {
        *self.well_known_symbols.slot_mut(id) = Some(value);
        self.well_known_roots[id.index()] = Some(root);
    }
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            registered_symbols: Vec::new(),
            well_known_symbols: WellKnownSymbols::default(),
            well_known_roots: [None; WellKnownSymbolId::COUNT],
        }
    }
}
