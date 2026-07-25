//! Object-level `OrdinaryOwnPropertyKeys` ordering over structural and virtual properties.

use super::super::*;

const STRING_KEY_RANK: u64 = 1_u64 << 62;
const SYMBOL_KEY_RANK: u64 = 2_u64 << 62;
const VIRTUAL_KEY_COUNT: u64 = 3;
const MAX_ARRAY_INDEX: u32 = u32::MAX - 1;

#[derive(Clone, Copy)]
struct RankedPropertyKey {
    key: PropertyKey,
    property: Option<PropertyLookup>,
    rank: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct OrdinaryOwnPropertyEntry {
    pub(crate) key: PropertyKey,
    pub(crate) property: Option<PropertyLookup>,
}

/// Exact-capacity snapshot in ECMAScript index, String, then Symbol order.
pub(crate) struct OrdinaryOwnPropertyKeys {
    keys: std::vec::IntoIter<RankedPropertyKey>,
}

impl Iterator for OrdinaryOwnPropertyKeys {
    type Item = PropertyKey;

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        self.next_entry().map(|entry| entry.key)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.keys.size_hint()
    }
}

impl OrdinaryOwnPropertyKeys {
    #[inline(always)]
    pub(crate) fn next_entry(&mut self) -> Option<OrdinaryOwnPropertyEntry> {
        self.keys.next().map(|entry| OrdinaryOwnPropertyEntry {
            key: entry.key,
            property: entry.property,
        })
    }
}

impl ExactSizeIterator for OrdinaryOwnPropertyKeys {}

impl Isolate {
    /// Merges live structural keys with zero-backing function keys, then sorts without allocation.
    pub(crate) fn ordinary_own_property_keys(
        &mut self,
        receiver: Value,
        snapshot: OrdinaryObject,
    ) -> Result<OrdinaryOwnPropertyKeys, ExecutionError> {
        let virtual_keys = self.function_virtual_property_keys(receiver)?;
        let string_length = self
            .is_string_wrapper(receiver)
            .then(|| self.string_value_length(receiver))
            .transpose()?;
        let typed_array_length = self
            .is_typed_array_value(receiver)
            .then(|| {
                self.typed_array_snapshot(receiver)
                    .map(|array| array.length)
            })
            .transpose()?;
        let structural = self
            .shapes
            .own_keys(snapshot.shape)
            .map_err(ExecutionError::Shape)?;
        let missing_virtuals = virtual_keys
            .iter()
            .flatten()
            .filter(|(_, key)| self.shapes.lookup(snapshot.shape, *key).is_none())
            .count();
        let string_virtual_count = string_length
            .map(|length| length.saturating_add(1))
            .unwrap_or(0);
        let capacity = structural
            .len()
            .checked_add(missing_virtuals)
            .and_then(|capacity| capacity.checked_add(string_virtual_count))
            .and_then(|capacity| capacity.checked_add(typed_array_length.unwrap_or(0)))
            .ok_or(ExecutionError::OwnPropertyKeyAllocationFailed)?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(capacity)
            .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
        for &(ordinal, key) in virtual_keys.iter().flatten() {
            if self.shapes.lookup(snapshot.shape, key).is_none() {
                keys.push(RankedPropertyKey {
                    key,
                    property: None,
                    rank: STRING_KEY_RANK | u64::from(ordinal),
                });
            }
        }
        if let Some(length) = string_length {
            for index in 0..length {
                let atom = self.safe_integer_property_atom(index as u64)?;
                keys.push(RankedPropertyKey {
                    key: PropertyKey::Atom(atom),
                    property: None,
                    rank: index as u64,
                });
            }
            keys.push(RankedPropertyKey {
                key: PropertyKey::Atom(self.length_atom()?),
                property: None,
                rank: STRING_KEY_RANK | (1_u64 << 32),
            });
        }
        if let Some(length) = typed_array_length {
            for index in 0..length {
                let atom = self.safe_integer_property_atom(index as u64)?;
                keys.push(RankedPropertyKey {
                    key: PropertyKey::Atom(atom),
                    property: None,
                    rank: index as u64,
                });
            }
        }
        let mut structural = structural;
        let mut order = 0_usize;
        while let Some(entry) = structural.next_entry() {
            let key = entry.key;
            let property = entry.property;
            let order_rank =
                u64::try_from(order).map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
            order = order
                .checked_add(1)
                .ok_or(ExecutionError::OwnPropertyKeyAllocationFailed)?;
            if !self.property_is_present_from_snapshot(snapshot, property)? {
                continue;
            }
            let rank =
                self.property_key_rank(key, property.attributes, &virtual_keys, order_rank)?;
            keys.push(RankedPropertyKey {
                key,
                property: Some(property),
                rank,
            });
        }
        keys.sort_unstable_by_key(|entry| entry.rank);
        Ok(OrdinaryOwnPropertyKeys {
            keys: keys.into_iter(),
        })
    }

    /// Returns fixed creation ordinals without materializing metadata values or lazy prototypes.
    fn function_virtual_property_keys(
        &mut self,
        receiver: Value,
    ) -> Result<[Option<(u8, PropertyKey)>; 3], ExecutionError> {
        if self.resolve_function_object(receiver).is_err() {
            return Ok([None, None, None]);
        }
        let length = self.length_atom()?.into();
        let name = self.name_atom()?.into();
        let prototype = self.prototype_atom()?;
        let prototype = self
            .is_function_prototype_property(receiver, prototype)
            .then_some((2, prototype.into()));
        Ok([Some((0, length)), Some((1, name)), prototype])
    }

    /// Encodes a total order while preserving chronology within non-index String and Symbol groups.
    fn property_key_rank(
        &self,
        key: PropertyKey,
        attributes: PropertyAttributes,
        virtual_keys: &[Option<(u8, PropertyKey)>; 3],
        order: u64,
    ) -> Result<u64, ExecutionError> {
        let PropertyKey::Atom(atom) = key else {
            return Ok(SYMBOL_KEY_RANK | order);
        };
        let string = self
            .atoms
            .get(atom)
            .ok_or(ExecutionError::OwnPropertyKeyAllocationFailed)?;
        if let Some(index) = array_index(string.as_view()) {
            return Ok(u64::from(index));
        }
        if attributes.virtual_origin()
            && let Some((ordinal, _)) = virtual_keys
                .iter()
                .flatten()
                .find(|(_, virtual_key)| *virtual_key == key)
        {
            return Ok(STRING_KEY_RANK | u64::from(*ordinal));
        }
        Ok(STRING_KEY_RANK | VIRTUAL_KEY_COUNT | order)
    }
}

/// Recognizes only canonical decimal spellings in the `0..=2^32-2` ArrayIndex range.
pub(crate) fn array_index(string: JsStringView<'_>) -> Option<u32> {
    let length = string.len();
    if length == 0 || length > 10 {
        return None;
    }
    let first = string.code_unit_at(0)?;
    if first == u16::from(b'0') {
        return (length == 1).then_some(0);
    }
    if !(u16::from(b'1')..=u16::from(b'9')).contains(&first) {
        return None;
    }
    let mut value = u32::from(first - u16::from(b'0'));
    for index in 1..length {
        let unit = string.code_unit_at(index)?;
        if !(u16::from(b'0')..=u16::from(b'9')).contains(&unit) {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(u32::from(unit - u16::from(b'0')))?;
    }
    (value <= MAX_ARRAY_INDEX).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::array_index;
    use crate::JsString;

    fn parse(value: &[u8]) -> Option<u32> {
        array_index(JsString::try_from_latin1(value).unwrap().as_view())
    }

    #[test]
    fn array_index_accepts_only_canonical_uint32_minus_one_spellings() {
        assert_eq!(parse(b"0"), Some(0));
        assert_eq!(parse(b"9"), Some(9));
        assert_eq!(parse(b"4294967294"), Some(4_294_967_294));
        for value in [
            b"".as_slice(),
            b"00",
            b"01",
            b"-0",
            b"+1",
            b"1.0",
            b"4294967295",
        ] {
            assert_eq!(parse(value), None, "{value:?}");
        }
    }
}
