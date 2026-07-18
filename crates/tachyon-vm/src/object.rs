//! Ordinary-object shapes and exactly accounted contiguous property storage.

use core::mem::size_of;
use std::vec::IntoIter;

use tachyon_gc::{GcExternalMemory, GcRef, Trace, Tracer};
use tachyon_value::Value;

use crate::{AtomId, tuning::objects};

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

    pub(crate) const DEFAULT_DATA: Self = Self(0b111);

    pub(crate) const fn data(writable: bool, enumerable: bool, configurable: bool) -> Self {
        Self(
            ((writable as u8) * Self::WRITABLE)
                | ((enumerable as u8) * Self::ENUMERABLE)
                | ((configurable as u8) * Self::CONFIGURABLE),
        )
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
}

#[derive(Clone, Copy, Debug)]
struct Shape {
    parent: Option<ShapeId>,
    key: Option<AtomId>,
    slot: u32,
    property_count: u32,
    attributes: PropertyAttributes,
    version: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ShapeTransition {
    from: ShapeId,
    key: AtomId,
    attributes: PropertyAttributes,
    to: ShapeId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PropertyLookup {
    pub(crate) slot: u32,
    pub(crate) attributes: PropertyAttributes,
}

pub(crate) struct OwnPropertyKeys {
    slots: IntoIter<Option<AtomId>>,
}

impl Iterator for OwnPropertyKeys {
    type Item = AtomId;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.slots
            .next()
            .map(|key| key.expect("every property slot has one latest shape entry"))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.slots.size_hint()
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
                    *slot = Some(key);
                }
            }
            current = entry.parent.expect("non-root shapes have a parent");
        }
        debug_assert!(keys.iter().all(Option::is_some));
        Ok(OwnPropertyKeys {
            slots: keys.into_iter(),
        })
    }

    /// Walks the immutable parent chain; M13 replaces this slow path with guarded caches.
    #[inline]
    pub(crate) fn lookup(&self, mut shape: ShapeId, key: AtomId) -> Option<PropertyLookup> {
        while shape != ShapeId::EMPTY {
            let entry = &self.shapes[shape.index()];
            if entry.key == Some(key) {
                return Some(PropertyLookup {
                    slot: entry.slot,
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
        key: AtomId,
        attributes: PropertyAttributes,
    ) -> Result<ShapeId, ShapeError> {
        let slot = self.property_count(from);
        let property_count = slot.checked_add(1).ok_or(ShapeError::IdOverflow)?;
        self.transition(from, key, slot, property_count, attributes)
    }

    /// Overlays new flags for one existing slot without changing insertion order or storage size.
    pub(crate) fn transition_reconfigure(
        &mut self,
        from: ShapeId,
        key: AtomId,
        attributes: PropertyAttributes,
    ) -> Result<ShapeId, ShapeError> {
        let current = self
            .lookup(from, key)
            .expect("reconfiguration requires an own property");
        if current.attributes == attributes {
            return Ok(from);
        }
        self.transition(
            from,
            key,
            current.slot,
            self.property_count(from),
            attributes,
        )
    }

    /// Reuses or appends one immutable shape edge with explicit slot/count semantics.
    fn transition(
        &mut self,
        from: ShapeId,
        key: AtomId,
        slot: u32,
        property_count: u32,
        attributes: PropertyAttributes,
    ) -> Result<ShapeId, ShapeError> {
        if let Some(transition) = self
            .transitions
            .iter()
            .find(|edge| edge.from == from && edge.key == key && edge.attributes == attributes)
        {
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
            attributes,
            version,
        });
        self.transitions.push(ShapeTransition {
            from,
            key,
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
}

impl Trace for PropertyStorage {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.slots.trace(tracer);
    }
}

impl GcExternalMemory for PropertyStorage {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.slots.len() * size_of::<Value>()
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

#[cfg(test)]
mod tests {
    use core::mem::size_of;

    use super::{OrdinaryObject, PropertyAttributes, ShapeId, ShapeTable};
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
            .transition_reconfigure(two, first, PropertyAttributes::data(false, false, false))
            .unwrap();

        assert_eq!(table.property_count(reconfigured), 2);
        assert_eq!(table.lookup(reconfigured, first).unwrap().slot, 0);
        assert_eq!(
            table.lookup(reconfigured, first).unwrap().attributes,
            PropertyAttributes::data(false, false, false)
        );
        assert_eq!(
            table.own_keys(reconfigured).unwrap().collect::<Vec<_>>(),
            [first, second]
        );
    }

    #[test]
    fn extensibility_uses_existing_object_alignment_padding() {
        assert_eq!(size_of::<OrdinaryObject>(), 24);
    }
}
