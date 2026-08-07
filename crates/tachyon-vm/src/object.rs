//! Ordinary-object shapes and exactly accounted contiguous property storage.

use core::mem::{size_of, size_of_val};
use std::sync::{Arc, Mutex};
use tachyon_gc::{GcExternalMemory, GcRef, Trace, Tracer};
use tachyon_value::{RawHeapRef, Value};

use crate::{
    AtomId, CodeId, IntlCollatorBackend, IntlCollatorCaseFirst, IntlCollatorSensitivity,
    IntlCollatorUsage, IntlDateTimeFormatBackend, IntlDateTimeFormatResolved, IntlListFormatStyle,
    IntlListFormatType, IntlNumberFormatBackend, IntlNumberFormatResolved, IntlPluralCategory,
    IntlPluralRulesBackend, IntlPluralRulesResolved, tuning::objects,
};
use tachyon_bytecode::FunctionId;

/// Stable isolate-local identity for one Symbol property key.
///
/// The upper word is a never-reused isolate serial and the lower word retains the logical heap
/// reference needed to publish the original Symbol value. Shapes do not trace this reference;
/// live object storage owns the exact GC edge instead.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub(crate) struct SymbolId(core::num::NonZeroU64);

impl SymbolId {
    pub(crate) const fn new(serial: core::num::NonZeroU32, reference: RawHeapRef) -> Self {
        let bits = ((serial.get() as u64) << 32) | reference.offset() as u64;
        Self(core::num::NonZeroU64::new(bits).expect("a non-zero serial produces a Symbol ID"))
    }

    #[inline(always)]
    pub(crate) const fn reference(self) -> RawHeapRef {
        RawHeapRef::new(self.0.get() as u32)
            .expect("a Symbol ID retains a non-zero logical heap reference")
    }

    #[inline(always)]
    pub(crate) const fn serial(self) -> u32 {
        (self.0.get() >> 32) as u32
    }

    #[inline(always)]
    pub(crate) const fn value(self) -> Value {
        Value::from_heap_ref(self.reference())
    }

    #[inline(always)]
    const fn with_reference(self, reference: RawHeapRef) -> Self {
        let serial = (self.0.get() >> 32) as u32;
        Self::new(
            core::num::NonZeroU32::new(serial).expect("Symbol IDs always retain a serial"),
            reference,
        )
    }

    #[cfg(test)]
    const fn from_test_parts(serial: u32, reference: u32) -> Self {
        Self::new(
            core::num::NonZeroU32::new(serial).expect("test Symbol serial is non-zero"),
            RawHeapRef::new(reference).expect("test Symbol reference is valid"),
        )
    }
}

/// Closed ECMAScript property-key identity used by shapes and ordinary property operations.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum PropertyKey {
    Atom(AtomId),
    Symbol(SymbolId),
    Private(SymbolId),
}

impl PropertyKey {
    #[inline(always)]
    pub(crate) const fn atom(self) -> Option<AtomId> {
        match self {
            Self::Atom(atom) => Some(atom),
            Self::Symbol(_) | Self::Private(_) => None,
        }
    }

    #[inline(always)]
    pub(crate) const fn symbol(self) -> Option<SymbolId> {
        match self {
            Self::Atom(_) | Self::Private(_) => None,
            Self::Symbol(symbol) => Some(symbol),
        }
    }

    #[inline(always)]
    pub(crate) const fn symbol_identity(self) -> Option<SymbolId> {
        match self {
            Self::Atom(_) => None,
            Self::Symbol(symbol) | Self::Private(symbol) => Some(symbol),
        }
    }
}

impl From<AtomId> for PropertyKey {
    #[inline(always)]
    fn from(atom: AtomId) -> Self {
        Self::Atom(atom)
    }
}

impl Trace for PropertyKey {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        if let Self::Symbol(symbol) | Self::Private(symbol) = self {
            let mut reference = symbol.reference();
            tracer.trace_raw_heap_ref(&mut reference);
            *symbol = symbol.with_reference(reference);
        }
    }
}

/// Stable isolate-local hidden-class identifier; zero is the shared empty shape.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub(crate) struct ShapeId(u32);

impl ShapeId {
    pub(crate) const EMPTY: Self = Self(0);

    const fn index(self) -> usize {
        self.0 as usize
    }
}

/// ECMAScript data-property flags kept compact for shape guards and descriptor work.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PropertyAttributes(u8);

impl PropertyAttributes {
    const WRITABLE: u8 = 1 << 0;
    const ENUMERABLE: u8 = 1 << 1;
    const CONFIGURABLE: u8 = 1 << 2;
    const VIRTUAL_ORIGIN: u8 = 1 << 3;

    pub(crate) const DEFAULT_DATA: Self = Self(0b111);

    pub(crate) const fn data(writable: bool, enumerable: bool, configurable: bool) -> Self {
        Self(
            ((writable as u8) * Self::WRITABLE)
                | ((enumerable as u8) * Self::ENUMERABLE)
                | ((configurable as u8) * Self::CONFIGURABLE),
        )
    }

    pub(crate) const fn accessor(enumerable: bool, configurable: bool) -> Self {
        Self::data(false, enumerable, configurable)
    }

    pub(crate) const fn writable(self) -> bool {
        self.0 & Self::WRITABLE != 0
    }

    pub(crate) const fn enumerable(self) -> bool {
        self.0 & Self::ENUMERABLE != 0
    }

    pub(crate) const fn configurable(self) -> bool {
        self.0 & Self::CONFIGURABLE != 0
    }

    pub(crate) const fn with_virtual_origin(mut self) -> Self {
        self.0 |= Self::VIRTUAL_ORIGIN;
        self
    }

    pub(crate) const fn virtual_origin(self) -> bool {
        self.0 & Self::VIRTUAL_ORIGIN != 0
    }
}

/// Stored ordinary-property payload kind; generic descriptors never enter shape metadata.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub(crate) enum PropertyKind {
    Data,
    Accessor,
}

#[derive(Clone, Copy, Debug)]
struct Shape {
    parent: Option<ShapeId>,
    key: Option<PropertyKey>,
    slot: u32,
    property_count: u32,
    kind: PropertyKind,
    attributes: PropertyAttributes,
    version: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ShapeTransition {
    from: ShapeId,
    key: PropertyKey,
    kind: PropertyKind,
    attributes: PropertyAttributes,
    to: ShapeId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PropertyLookup {
    pub(crate) slot: u32,
    pub(crate) kind: PropertyKind,
    pub(crate) attributes: PropertyAttributes,
}

pub(crate) struct OwnPropertyKeys {
    slots: Box<[Option<OwnPropertyEntry>]>,
    index: usize,
    symbols: bool,
    remaining: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct OwnPropertyEntry {
    pub(crate) key: PropertyKey,
    pub(crate) property: PropertyLookup,
}

impl OwnPropertyKeys {
    #[inline]
    pub(crate) fn next_entry(&mut self) -> Option<OwnPropertyEntry> {
        loop {
            while let Some(entry) = self.slots.get(self.index).copied().flatten() {
                self.index += 1;
                if matches!(entry.key, PropertyKey::Private(_)) {
                    continue;
                }
                if matches!(entry.key, PropertyKey::Symbol(_)) == self.symbols {
                    self.remaining -= 1;
                    return Some(entry);
                }
            }
            if self.symbols {
                return None;
            }
            self.symbols = true;
            self.index = 0;
        }
    }
}

impl Iterator for OwnPropertyKeys {
    type Item = PropertyKey;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.next_entry().map(|entry| entry.key)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for OwnPropertyKeys {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShapeError {
    LimitExceeded { limit: u32 },
    AllocationFailed,
    IdOverflow,
}

/// Append-only shape owner. Transition misses are cold and explicitly chunk-reserved.
#[derive(Debug)]
pub(crate) struct ShapeTable {
    shapes: Vec<Shape>,
    transitions: Vec<ShapeTransition>,
    limit: u32,
}

impl ShapeTable {
    /// Builds the empty root shape and bounded initial construction buffers.
    pub(crate) fn new(limit: u32) -> Result<Self, ShapeError> {
        if limit == 0 {
            return Err(ShapeError::LimitExceeded { limit });
        }
        let shape_capacity = (limit as usize).min(objects::INITIAL_SHAPE_CAPACITY);
        let transition_limit = limit.saturating_sub(1) as usize;
        let transition_capacity = transition_limit.min(objects::INITIAL_TRANSITION_CAPACITY);
        let mut shapes = Vec::new();
        shapes
            .try_reserve_exact(shape_capacity)
            .map_err(|_| ShapeError::AllocationFailed)?;
        let mut transitions = Vec::new();
        transitions
            .try_reserve_exact(transition_capacity)
            .map_err(|_| ShapeError::AllocationFailed)?;
        shapes.push(Shape {
            parent: None,
            key: None,
            slot: 0,
            property_count: 0,
            kind: PropertyKind::Data,
            attributes: PropertyAttributes::DEFAULT_DATA,
            version: 0,
        });
        Ok(Self {
            shapes,
            transitions,
            limit,
        })
    }

    #[inline(always)]
    pub(crate) fn property_count(&self, shape: ShapeId) -> u32 {
        self.shapes[shape.index()].property_count
    }

    /// Returns own property keys in insertion order for ordinary data-property enumeration.
    pub(crate) fn own_keys(&self, shape: ShapeId) -> Result<OwnPropertyKeys, ShapeError> {
        let count =
            usize::try_from(self.property_count(shape)).map_err(|_| ShapeError::IdOverflow)?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(count)
            .map_err(|_| ShapeError::AllocationFailed)?;
        keys.resize(count, None);
        let mut current = shape;
        while current != ShapeId::EMPTY {
            let entry = &self.shapes[current.index()];
            if let Some(key) = entry.key {
                let slot = &mut keys[entry.slot as usize];
                if slot.is_none() {
                    *slot = Some(OwnPropertyEntry {
                        key,
                        property: PropertyLookup {
                            slot: entry.slot,
                            kind: entry.kind,
                            attributes: entry.attributes,
                        },
                    });
                }
            }
            current = entry.parent.expect("non-root shapes have a parent");
        }
        debug_assert!(keys.iter().all(Option::is_some));
        let public_count = keys
            .iter()
            .flatten()
            .filter(|entry| !matches!(entry.key, PropertyKey::Private(_)))
            .count();
        Ok(OwnPropertyKeys {
            slots: keys.into_boxed_slice(),
            index: 0,
            symbols: false,
            remaining: public_count,
        })
    }

    /// Walks the immutable parent chain; M13 replaces this slow path with guarded caches.
    #[inline]
    pub(crate) fn lookup(
        &self,
        mut shape: ShapeId,
        key: impl Into<PropertyKey>,
    ) -> Option<PropertyLookup> {
        let key = key.into();
        while shape != ShapeId::EMPTY {
            let entry = &self.shapes[shape.index()];
            if entry.key == Some(key) {
                return Some(PropertyLookup {
                    slot: entry.slot,
                    kind: entry.kind,
                    attributes: entry.attributes,
                });
            }
            shape = entry.parent.expect("non-root shapes have a parent");
        }
        None
    }

    /// Reuses an existing hidden-class edge or publishes one bounded append-only transition.
    pub(crate) fn transition_add(
        &mut self,
        from: ShapeId,
        key: impl Into<PropertyKey>,
        attributes: PropertyAttributes,
    ) -> Result<ShapeId, ShapeError> {
        self.transition_add_kind(from, key, PropertyKind::Data, attributes)
    }

    /// Adds one payload-kind-specific transition without widening existing data-property callers.
    pub(crate) fn transition_add_kind(
        &mut self,
        from: ShapeId,
        key: impl Into<PropertyKey>,
        kind: PropertyKind,
        attributes: PropertyAttributes,
    ) -> Result<ShapeId, ShapeError> {
        let key = key.into();
        let slot = self.property_count(from);
        let property_count = slot.checked_add(1).ok_or(ShapeError::IdOverflow)?;
        self.transition(from, key, slot, property_count, kind, attributes)
    }

    /// Overlays payload kind and flags while retaining the property's original logical slot.
    pub(crate) fn transition_reconfigure_kind(
        &mut self,
        from: ShapeId,
        key: impl Into<PropertyKey>,
        kind: PropertyKind,
        attributes: PropertyAttributes,
    ) -> Result<ShapeId, ShapeError> {
        let key = key.into();
        let current = self
            .lookup(from, key)
            .expect("reconfiguration requires an own property");
        if current.kind == kind && current.attributes == attributes {
            return Ok(from);
        }
        self.transition(
            from,
            key,
            current.slot,
            self.property_count(from),
            kind,
            attributes,
        )
    }

    /// Reuses or appends one immutable shape edge with explicit slot/count semantics.
    fn transition(
        &mut self,
        from: ShapeId,
        key: PropertyKey,
        slot: u32,
        property_count: u32,
        kind: PropertyKind,
        attributes: PropertyAttributes,
    ) -> Result<ShapeId, ShapeError> {
        if let Some(transition) = self.transitions.iter().find(|edge| {
            edge.from == from
                && edge.key == key
                && edge.kind == kind
                && edge.attributes == attributes
        }) {
            return Ok(transition.to);
        }
        if self.shapes.len() >= self.limit as usize {
            return Err(ShapeError::LimitExceeded { limit: self.limit });
        }
        reserve_chunked(
            &mut self.shapes,
            self.limit as usize,
            objects::SHAPE_GROWTH_CHUNK,
        )?;
        reserve_chunked(
            &mut self.transitions,
            self.limit.saturating_sub(1) as usize,
            objects::TRANSITION_GROWTH_CHUNK,
        )?;
        let id = ShapeId(u32::try_from(self.shapes.len()).map_err(|_| ShapeError::IdOverflow)?);
        let version = self.shapes[from.index()]
            .version
            .checked_add(1)
            .ok_or(ShapeError::IdOverflow)?;
        self.shapes.push(Shape {
            parent: Some(from),
            key: Some(key),
            slot,
            property_count,
            kind,
            attributes,
            version,
        });
        self.transitions.push(ShapeTransition {
            from,
            key,
            kind,
            attributes,
            to: id,
        });
        Ok(id)
    }
}

/// Reserves one named tuning chunk only when the current cold-path push would grow.
fn reserve_chunked<T>(items: &mut Vec<T>, limit: usize, chunk: usize) -> Result<(), ShapeError> {
    if items.len() < items.capacity() {
        return Ok(());
    }
    let additional = limit.saturating_sub(items.len()).min(chunk);
    items
        .try_reserve_exact(additional)
        .map_err(|_| ShapeError::AllocationFailed)
}

/// Fixed-length replacement storage; no property write can resize it in place.
#[derive(Debug)]
pub(crate) struct PropertyStorage {
    pub(crate) slots: Box<[Value]>,
    symbol_keys: Box<[SymbolPropertyKey]>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SymbolPropertyKey {
    slot: u32,
    id: SymbolId,
    value: Value,
}

impl SymbolPropertyKey {
    pub(crate) const fn new(slot: u32, id: SymbolId, value: Value) -> Self {
        Self { slot, id, value }
    }
}

impl PropertyStorage {
    pub(crate) fn new(slots: Box<[Value]>) -> Self {
        Self {
            slots,
            symbol_keys: Box::default(),
        }
    }

    pub(crate) fn with_symbol_keys(
        slots: Box<[Value]>,
        symbol_keys: Box<[SymbolPropertyKey]>,
    ) -> Self {
        Self { slots, symbol_keys }
    }

    pub(crate) fn symbol_key_count(&self) -> usize {
        self.symbol_keys.len()
    }

    pub(crate) fn append_symbol_keys(&self, output: &mut Vec<SymbolPropertyKey>) {
        output.extend_from_slice(&self.symbol_keys);
    }

    #[cfg(test)]
    pub(crate) fn symbol_value(&self, slot: u32, id: SymbolId) -> Option<Value> {
        self.symbol_keys
            .iter()
            .find(|key| key.slot == slot && key.id == id)
            .map(|key| key.value)
            .filter(|value| value.as_immediate() != Some(tachyon_value::Immediate::Hole))
    }

    pub(crate) fn set_symbol_presence(&mut self, slot: u32, key: PropertyKey, present: bool) {
        let Some(id) = key.symbol_identity() else {
            return;
        };
        let entry = self
            .symbol_keys
            .iter_mut()
            .find(|entry| entry.slot == slot && entry.id == id)
            .expect("every Symbol shape slot retains one storage key edge");
        entry.value = if present {
            id.value()
        } else {
            Value::from_immediate(tachyon_value::Immediate::Hole)
        };
    }
}

impl Trace for PropertyStorage {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.slots.trace(tracer);
        for key in &mut self.symbol_keys {
            key.value.trace(tracer);
        }
    }
}

impl GcExternalMemory for PropertyStorage {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.slots.len() * size_of::<Value>()
            + self.symbol_keys.len() * size_of::<SymbolPropertyKey>()
    }
}

/// Ordinary data-property object. Exotic kinds remain separate slow-path payload types.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct OrdinaryObject {
    pub(crate) shape: ShapeId,
    pub(crate) extensible: bool,
    pub(crate) storage: Option<GcRef<PropertyStorage>>,
    pub(crate) prototype: Value,
}

impl Trace for OrdinaryObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.storage.trace(tracer);
        self.prototype.trace(tracer);
    }
}

/// Ordinary wrapper carrying the specification's private `[[NumberData]]` slot.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct NumberObject {
    pub(crate) number_data: Value,
    pub(crate) ordinary: OrdinaryObject,
}

/// Ordinary wrapper carrying the specification's private `[[BooleanData]]` slot.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct BooleanObject {
    pub(crate) boolean_data: Value,
    pub(crate) ordinary: OrdinaryObject,
}

/// Ordinary wrapper carrying the specification's private `[[BigIntData]]` slot.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct BigIntObject {
    pub(crate) bigint_data: Value,
    pub(crate) ordinary: OrdinaryObject,
}

/// Ordinary wrapper carrying the specification's private `[[StringData]]` slot.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct StringObject {
    pub(crate) string_data: Value,
    pub(crate) ordinary: OrdinaryObject,
}

/// Branded `Intl.Locale` object carrying its canonical language tag.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct IntlLocaleObject {
    pub(crate) locale: Value,
    pub(crate) ordinary: OrdinaryObject,
}

/// Branded `Intl.ListFormat` object carrying resolved scalar slots and ordinary state.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct IntlListFormatObject {
    pub(crate) locale: Value,
    pub(crate) list_type: IntlListFormatType,
    pub(crate) style: IntlListFormatStyle,
    pub(crate) ordinary: OrdinaryObject,
}

/// Ordinary wrapper carrying the specification's optional `[[SymbolData]]` slot.
///
/// `%Symbol.prototype%` is itself a Symbol exotic object with an absent slot, while values
/// produced by `Object(symbol)` retain the primitive Symbol edge here.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct SymbolObject {
    pub(crate) symbol_data: Option<Value>,
    pub(crate) ordinary: OrdinaryObject,
}

/// Ordinary object carrying the specification's private `[[DateValue]]` slot.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct DateObject {
    pub(crate) date_value: f64,
    pub(crate) ordinary: OrdinaryObject,
}

/// Resolved scalar slots passed across the host/backend allocation boundary as one compact unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IntlCollatorResolvedOptions {
    pub(crate) usage: IntlCollatorUsage,
    pub(crate) sensitivity: IntlCollatorSensitivity,
    pub(crate) case_first: IntlCollatorCaseFirst,
    pub(crate) ignore_punctuation: bool,
    pub(crate) numeric: bool,
}

/// Host-backed comparator payload with no isolate-local JavaScript edges.
pub(crate) struct IntlCollatorBackendPayload {
    pub(crate) backend: Box<dyn IntlCollatorBackend>,
}

impl Trace for IntlCollatorBackendPayload {
    #[inline(always)]
    fn trace(&mut self, _tracer: &mut dyn Tracer) {}
}

impl GcExternalMemory for IntlCollatorBackendPayload {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        size_of_val(&*self.backend).saturating_add(self.backend.external_memory_bytes())
    }
}

/// Branded `Intl.Collator` object with resolved slots and a lazily cached bound comparator.
#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct IntlCollatorObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) backend: GcRef<IntlCollatorBackendPayload>,
    pub(crate) locale: Value,
    pub(crate) collation: Value,
    pub(crate) cached_bound_compare: Value,
    pub(crate) usage: IntlCollatorUsage,
    pub(crate) sensitivity: IntlCollatorSensitivity,
    pub(crate) case_first: IntlCollatorCaseFirst,
    pub(crate) ignore_punctuation: bool,
    pub(crate) numeric: bool,
}

impl Trace for IntlCollatorObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.backend.trace(tracer);
        self.locale.trace(tracer);
        self.collation.trace(tracer);
        self.cached_bound_compare.trace(tracer);
    }
}

/// Provider-owned NumberFormat backend and resolved slots with no managed JavaScript edges.
pub(crate) struct IntlNumberFormatPayload {
    pub(crate) backend: Box<dyn IntlNumberFormatBackend>,
    pub(crate) resolved: IntlNumberFormatResolved,
}

impl Trace for IntlNumberFormatPayload {
    #[inline(always)]
    fn trace(&mut self, _tracer: &mut dyn Tracer) {}
}

impl GcExternalMemory for IntlNumberFormatPayload {
    fn external_memory_bytes(&self) -> usize {
        let options = &self.resolved.options;
        size_of_val(&*self.backend)
            .saturating_add(self.backend.external_memory_bytes())
            .saturating_add(self.resolved.locale.len())
            .saturating_add(self.resolved.numbering_system.len())
            .saturating_add(options.currency.as_deref().map_or(0, str::len))
            .saturating_add(options.unit.as_deref().map_or(0, str::len))
    }
}

/// Branded NumberFormat object with a lazily cached bound `format` function.
#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct IntlNumberFormatObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) payload: GcRef<IntlNumberFormatPayload>,
    pub(crate) cached_bound_format: Value,
}

/// Provider-owned PluralRules backend and resolved slots with no managed JavaScript edges.
pub(crate) struct IntlPluralRulesPayload {
    pub(crate) backend: Box<dyn IntlPluralRulesBackend>,
    pub(crate) resolved: IntlPluralRulesResolved,
}

impl Trace for IntlPluralRulesPayload {
    #[inline(always)]
    fn trace(&mut self, _tracer: &mut dyn Tracer) {}
}

impl GcExternalMemory for IntlPluralRulesPayload {
    fn external_memory_bytes(&self) -> usize {
        size_of_val(&*self.backend)
            .saturating_add(self.backend.external_memory_bytes())
            .saturating_add(self.resolved.locale.len())
            .saturating_add(
                self.resolved
                    .categories
                    .len()
                    .saturating_mul(size_of::<IntlPluralCategory>()),
            )
    }
}

/// Branded `Intl.PluralRules` object retaining only ordinary state and provider payload.
#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct IntlPluralRulesObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) payload: GcRef<IntlPluralRulesPayload>,
}

impl Trace for IntlPluralRulesObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.payload.trace(tracer);
    }
}

impl Trace for IntlNumberFormatObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.payload.trace(tracer);
        self.cached_bound_format.trace(tracer);
    }
}

/// Provider-owned DateTimeFormat backend and resolved slots with no managed JavaScript edges.
pub(crate) struct IntlDateTimeFormatPayload {
    pub(crate) backend: Box<dyn IntlDateTimeFormatBackend>,
    pub(crate) resolved: IntlDateTimeFormatResolved,
}

impl Trace for IntlDateTimeFormatPayload {
    #[inline(always)]
    fn trace(&mut self, _tracer: &mut dyn Tracer) {}
}

impl GcExternalMemory for IntlDateTimeFormatPayload {
    fn external_memory_bytes(&self) -> usize {
        size_of_val(&*self.backend)
            .saturating_add(self.backend.external_memory_bytes())
            .saturating_add(self.resolved.locale.len())
            .saturating_add(self.resolved.calendar.len())
            .saturating_add(self.resolved.numbering_system.len())
            .saturating_add(self.resolved.time_zone.len())
    }
}

/// Branded DateTimeFormat object with a lazily cached bound `format` function.
#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct IntlDateTimeFormatObject {
    pub(crate) ordinary: OrdinaryObject,
    pub(crate) payload: GcRef<IntlDateTimeFormatPayload>,
    pub(crate) cached_bound_format: Value,
}

impl Trace for IntlDateTimeFormatObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
        self.payload.trace(tracer);
        self.cached_bound_format.trace(tracer);
    }
}

/// Externally-accounted byte storage shared by ArrayBuffer views in one isolate.
#[derive(Debug)]
pub(crate) struct ArrayBufferData {
    pub(crate) bytes: Box<[u8]>,
    pub(crate) byte_length: usize,
    pub(crate) max_byte_length: usize,
    pub(crate) resizable: bool,
}

/// Shared byte storage whose mutation is serialized only for SharedArrayBuffer access paths.
#[derive(Debug)]
pub(crate) struct SharedArrayBufferBacking {
    pub(crate) bytes: Box<[u8]>,
    pub(crate) byte_length: usize,
    pub(crate) max_byte_length: usize,
    pub(crate) growable: bool,
}

/// Cloneable host handle for sharing one SAB backing across isolate boundaries.
#[derive(Clone, Debug)]
pub struct SharedArrayBufferHandle {
    pub(crate) backing: Arc<Mutex<SharedArrayBufferBacking>>,
}

/// GC-owned reference-counted SharedArrayBuffer backing suitable for cross-agent cloning.
#[derive(Debug)]
pub(crate) struct SharedArrayBufferData {
    pub(crate) backing: Arc<Mutex<SharedArrayBufferBacking>>,
    pub(crate) external_bytes: usize,
}

impl Trace for SharedArrayBufferData {
    #[inline(always)]
    fn trace(&mut self, _tracer: &mut dyn Tracer) {}
}

impl GcExternalMemory for SharedArrayBufferData {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.external_bytes
    }
}

impl Trace for ArrayBufferData {
    #[inline(always)]
    fn trace(&mut self, _tracer: &mut dyn Tracer) {}
}

impl GcExternalMemory for ArrayBufferData {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.bytes.len()
    }
}

/// Ordinary object carrying a detachable `[[ArrayBufferData]]` edge.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct ArrayBufferObject {
    pub(crate) data: Option<GcRef<ArrayBufferData>>,
    pub(crate) shared_data: Option<GcRef<SharedArrayBufferData>>,
    pub(crate) ordinary: OrdinaryObject,
}

impl Trace for ArrayBufferObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.data.trace(tracer);
        self.shared_data.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

/// Ordinary object carrying a fixed view into an ArrayBuffer.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct DataViewObject {
    pub(crate) buffer: Value,
    pub(crate) byte_offset: u32,
    pub(crate) byte_length: u32,
    pub(crate) length_mode: ViewLengthMode,
    pub(crate) ordinary: OrdinaryObject,
}

/// Whether an ArrayBuffer view retains an explicit length or tracks its backing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ViewLengthMode {
    Fixed,
    Tracking,
}

impl Trace for DataViewObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.buffer.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

/// ECMAScript numeric domains accepted by one TypedArray element kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ContentType {
    Number,
    BigInt,
}

/// Element formats supported by fixed TypedArray views.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum TypedArrayKind {
    Int8,
    Uint8,
    Uint8Clamped,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float32,
    Float64,
    BigInt64,
    BigUint64,
}

impl TypedArrayKind {
    pub(crate) const ALL: [Self; 11] = [
        Self::Int8,
        Self::Uint8,
        Self::Uint8Clamped,
        Self::Int16,
        Self::Uint16,
        Self::Int32,
        Self::Uint32,
        Self::Float32,
        Self::Float64,
        Self::BigInt64,
        Self::BigUint64,
    ];

    #[inline(always)]
    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    #[inline(always)]
    pub(crate) const fn byte_width(self) -> usize {
        match self {
            Self::Int8 | Self::Uint8 | Self::Uint8Clamped => 1,
            Self::Int16 | Self::Uint16 => 2,
            Self::Int32 | Self::Uint32 | Self::Float32 => 4,
            Self::Float64 | Self::BigInt64 | Self::BigUint64 => 8,
        }
    }

    #[inline(always)]
    pub(crate) const fn content_type(self) -> ContentType {
        match self {
            Self::BigInt64 | Self::BigUint64 => ContentType::BigInt,
            _ => ContentType::Number,
        }
    }

    #[inline(always)]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Int8 => "Int8Array",
            Self::Uint8 => "Uint8Array",
            Self::Uint8Clamped => "Uint8ClampedArray",
            Self::Int16 => "Int16Array",
            Self::Uint16 => "Uint16Array",
            Self::Int32 => "Int32Array",
            Self::Uint32 => "Uint32Array",
            Self::Float32 => "Float32Array",
            Self::Float64 => "Float64Array",
            Self::BigInt64 => "BigInt64Array",
            Self::BigUint64 => "BigUint64Array",
        }
    }
}

/// Integer-indexed exotic object carrying a fixed view into an ArrayBuffer.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct TypedArrayObject {
    pub(crate) buffer: Value,
    pub(crate) byte_offset: u32,
    pub(crate) length: u32,
    pub(crate) kind: TypedArrayKind,
    pub(crate) length_mode: ViewLengthMode,
    pub(crate) ordinary: OrdinaryObject,
}

impl Trace for TypedArrayObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.buffer.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

/// Function activation arguments with an ordinary property base and stable exotic identity.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct ArgumentsObject {
    pub(crate) ordinary: OrdinaryObject,
    /// Active sloppy simple-parameter mapping; `u32::MAX` denotes an unmapped object.
    pub(crate) mapped_frame_depth: u32,
    pub(crate) mapped_base: u32,
    pub(crate) mapped_parameter_count: u32,
    pub(crate) mapped_code: Option<CodeId>,
    pub(crate) mapped_function: Option<FunctionId>,
    pub(crate) strict_restricted_properties: bool,
}

/// RegExp state whose observable properties remain in the ordinary-object base.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct RegExpObject {
    pub(crate) source: Value,
    pub(crate) flags: Value,
    pub(crate) ordinary: OrdinaryObject,
}

impl Trace for RegExpObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.source.trace(tracer);
        self.flags.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

impl Trace for StringObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.string_data.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

impl Trace for IntlLocaleObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.locale.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

impl Trace for IntlListFormatObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.locale.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

impl Trace for NumberObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.number_data.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

impl Trace for BooleanObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.boolean_data.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

impl Trace for BigIntObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.bigint_data.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

impl Trace for SymbolObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.symbol_data.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

impl Trace for DateObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
    }
}

impl Trace for ArgumentsObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
    }
}

#[cfg(test)]
mod tests {
    use core::mem::size_of;

    use super::{
        DataViewObject, DateObject, OrdinaryObject, PropertyAttributes, PropertyKey, PropertyKind,
        ShapeId, ShapeTable, SymbolId, TypedArrayObject,
    };
    use crate::AtomId;

    #[test]
    fn identical_add_sequences_share_shapes_and_slots() {
        let mut table = ShapeTable::new(16).unwrap();
        let first = AtomId::from_test_index(0);
        let second = AtomId::from_test_index(1);
        let one = table
            .transition_add(ShapeId::EMPTY, first, PropertyAttributes::DEFAULT_DATA)
            .unwrap();
        let two = table
            .transition_add(one, second, PropertyAttributes::DEFAULT_DATA)
            .unwrap();
        assert_eq!(
            table
                .transition_add(ShapeId::EMPTY, first, PropertyAttributes::DEFAULT_DATA)
                .unwrap(),
            one
        );
        assert_eq!(table.lookup(two, first).unwrap().slot, 0);
        assert_eq!(table.lookup(two, second).unwrap().slot, 1);
        assert_eq!(table.property_count(two), 2);
    }

    #[test]
    fn data_and_accessor_edges_do_not_share_a_shape_transition() {
        let mut table = ShapeTable::new(16).unwrap();
        let key = AtomId::from_test_index(0);
        let attributes = PropertyAttributes::data(false, true, true);
        let data = table
            .transition_add_kind(ShapeId::EMPTY, key, PropertyKind::Data, attributes)
            .unwrap();
        let accessor = table
            .transition_add_kind(ShapeId::EMPTY, key, PropertyKind::Accessor, attributes)
            .unwrap();

        assert_ne!(data, accessor);
        assert_eq!(table.lookup(data, key).unwrap().kind, PropertyKind::Data);
        assert_eq!(
            table.lookup(accessor, key).unwrap().kind,
            PropertyKind::Accessor
        );
    }

    #[test]
    fn reconfiguration_preserves_slots_count_and_insertion_order() {
        let mut table = ShapeTable::new(16).unwrap();
        let first = AtomId::from_test_index(0);
        let second = AtomId::from_test_index(1);
        let one = table
            .transition_add(ShapeId::EMPTY, first, PropertyAttributes::DEFAULT_DATA)
            .unwrap();
        let two = table
            .transition_add(one, second, PropertyAttributes::DEFAULT_DATA)
            .unwrap();
        let reconfigured = table
            .transition_reconfigure_kind(
                two,
                first,
                PropertyKind::Data,
                PropertyAttributes::data(false, false, false),
            )
            .unwrap();

        assert_eq!(table.property_count(reconfigured), 2);
        assert_eq!(table.lookup(reconfigured, first).unwrap().slot, 0);
        assert_eq!(
            table.lookup(reconfigured, first).unwrap().attributes,
            PropertyAttributes::data(false, false, false)
        );
        assert_eq!(
            table.own_keys(reconfigured).unwrap().collect::<Vec<_>>(),
            [PropertyKey::Atom(first), PropertyKey::Atom(second)]
        );
    }

    #[test]
    fn own_keys_keep_atoms_before_symbols_and_preserve_each_insertion_order() {
        let mut table = ShapeTable::new(16).unwrap();
        let first = AtomId::from_test_index(0);
        let second = AtomId::from_test_index(1);
        let symbol = SymbolId::from_test_parts(1, 16);
        let one = table
            .transition_add(ShapeId::EMPTY, first, PropertyAttributes::DEFAULT_DATA)
            .unwrap();
        let two = table
            .transition_add(
                one,
                PropertyKey::Symbol(symbol),
                PropertyAttributes::DEFAULT_DATA,
            )
            .unwrap();
        let three = table
            .transition_add(two, second, PropertyAttributes::DEFAULT_DATA)
            .unwrap();

        assert_eq!(
            table.own_keys(three).unwrap().collect::<Vec<_>>(),
            [
                PropertyKey::Atom(first),
                PropertyKey::Atom(second),
                PropertyKey::Symbol(symbol),
            ]
        );
    }

    #[test]
    fn extensibility_uses_existing_object_alignment_padding() {
        assert_eq!(size_of::<OrdinaryObject>(), 24);
        assert_eq!(size_of::<DateObject>(), 32);
        assert_eq!(size_of::<DataViewObject>(), 48);
        assert_eq!(size_of::<TypedArrayObject>(), 48);
    }
}
