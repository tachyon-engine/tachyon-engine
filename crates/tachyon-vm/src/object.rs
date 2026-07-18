//! Ordinary-object shapes and exactly accounted contiguous property storage.

use core::mem::size_of;

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
    pub(crate) const DEFAULT_DATA: Self = Self(0b111);
}

#[derive(Clone, Copy, Debug)]
struct Shape {
    parent: Option<ShapeId>,
    key: Option<AtomId>,
    slot: u32,
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
        self.shapes[shape.index()].slot
    }

    /// Walks the immutable parent chain; M13 replaces this slow path with guarded caches.
    #[inline]
    pub(crate) fn lookup(&self, mut shape: ShapeId, key: AtomId) -> Option<PropertyLookup> {
        while shape != ShapeId::EMPTY {
            let entry = &self.shapes[shape.index()];
            if entry.key == Some(key) {
                return Some(PropertyLookup {
                    slot: entry.slot - 1,
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
        let slot = self
            .property_count(from)
            .checked_add(1)
            .ok_or(ShapeError::IdOverflow)?;
        let version = self.shapes[from.index()]
            .version
            .checked_add(1)
            .ok_or(ShapeError::IdOverflow)?;
        self.shapes.push(Shape {
            parent: Some(from),
            key: Some(key),
            slot,
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
pub(crate) struct OrdinaryObject {
    pub(crate) shape: ShapeId,
    pub(crate) storage: Option<GcRef<PropertyStorage>>,
}

impl Trace for OrdinaryObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.storage.trace(tracer);
    }
}

#[cfg(test)]
mod tests {
    use super::{PropertyAttributes, ShapeId, ShapeTable};
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
}
