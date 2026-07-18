//! Isolate-local atom interning with explicit entropy, quotas, and open-addressing capacity.

use core::num::NonZeroU32;

use crate::{
    string::JsString,
    tuning::strings::{ATOM_LOAD_DENOMINATOR, ATOM_LOAD_NUMERATOR, INITIAL_ATOM_BUCKETS},
};
use tachyon_gc::GcExternalMemory;

/// Host-provided keyed hash entropy; tests and reproducible benchmarks may use fixed keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtomHashSeed {
    key0: u64,
    key1: u64,
}

impl AtomHashSeed {
    #[must_use]
    pub const fn new(key0: u64, key1: u64) -> Self {
        Self { key0, key1 }
    }

    pub(crate) const fn key0(self) -> u64 {
        self.key0
    }

    pub(crate) const fn key1(self) -> u64 {
        self.key1
    }
}

/// A stable non-zero index owned by one `AtomTable`; it is not portable across isolates.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct AtomId(NonZeroU32);

const _: [(); 4] = [(); core::mem::size_of::<AtomId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<AtomId>>()];

impl AtomId {
    #[cfg(test)]
    pub(crate) fn from_test_index(index: usize) -> Self {
        Self::from_index(index)
    }
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0.get() - 1
    }

    fn from_index(index: usize) -> Self {
        let value = u32::try_from(index + 1).expect("atom quotas stay below u32::MAX");
        Self(NonZeroU32::new(value).expect("one-based atom IDs are non-zero"))
    }
}

/// Host resource policy; hash entropy is explicit and never discovered by the engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtomTableConfig {
    max_entries: u32,
    max_string_bytes: usize,
    hash_seed: AtomHashSeed,
}

impl AtomTableConfig {
    #[must_use]
    pub const fn new(max_entries: u32, max_string_bytes: usize, hash_seed: AtomHashSeed) -> Self {
        Self {
            max_entries,
            max_string_bytes,
            hash_seed,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtomTableError {
    EntryLimitExceeded {
        limit: u32,
    },
    StringBytesLimitExceeded {
        limit: usize,
        used: usize,
        requested: usize,
    },
    AllocationFailed,
    CapacityOverflow,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AtomTableStats {
    pub entries: usize,
    pub retained_string_bytes: usize,
    pub initial_bucket_capacity: usize,
    pub growth_count: usize,
    pub peak_entries: usize,
    pub retained_bucket_capacity: usize,
    pub retained_entry_capacity: usize,
    pub vacant_buckets: usize,
}

#[derive(Debug)]
struct AtomEntry {
    string: JsString,
    hash: u64,
}

/// Atoms live until their isolate drops; explicit quotas prevent immortal unbounded growth.
#[derive(Debug)]
pub struct AtomTable {
    config: AtomTableConfig,
    entries: Vec<AtomEntry>,
    buckets: Vec<Option<AtomId>>,
    retained_string_bytes: usize,
    initial_bucket_capacity: usize,
    growth_count: usize,
    peak_entries: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct AtomTableCheckpoint {
    entries_len: usize,
    retained_string_bytes: usize,
}

impl AtomTable {
    #[must_use]
    pub const fn new(config: AtomTableConfig) -> Self {
        Self {
            config,
            entries: Vec::new(),
            buckets: Vec::new(),
            retained_string_bytes: 0,
            initial_bucket_capacity: 0,
            growth_count: 0,
            peak_entries: 0,
        }
    }

    /// Returns an existing atom or publishes one only after every quota and reserve succeeds.
    pub fn try_intern(&mut self, string: JsString) -> Result<AtomId, AtomTableError> {
        let hash = string.hash_with_seed(self.config.hash_seed);
        if let Some(atom) = self.find_hashed(&string, hash) {
            return Ok(atom);
        }
        if self.entries.len() >= self.config.max_entries as usize {
            return Err(AtomTableError::EntryLimitExceeded {
                limit: self.config.max_entries,
            });
        }
        let string_bytes = string.external_memory_bytes();
        let retained = self
            .retained_string_bytes
            .checked_add(string_bytes)
            .ok_or(AtomTableError::CapacityOverflow)?;
        if retained > self.config.max_string_bytes {
            return Err(AtomTableError::StringBytesLimitExceeded {
                limit: self.config.max_string_bytes,
                used: self.retained_string_bytes,
                requested: string_bytes,
            });
        }
        self.ensure_insert_capacity()?;
        let atom = AtomId::from_index(self.entries.len());
        let bucket = find_vacant_bucket(&self.buckets, hash);
        self.entries.push(AtomEntry { string, hash });
        self.buckets[bucket] = Some(atom);
        self.retained_string_bytes = retained;
        self.peak_entries = self.peak_entries.max(self.entries.len());
        Ok(atom)
    }

    pub(crate) fn checkpoint(&self) -> AtomTableCheckpoint {
        AtomTableCheckpoint {
            entries_len: self.entries.len(),
            retained_string_bytes: self.retained_string_bytes,
        }
    }

    /// Removes only atoms published after a module-load checkpoint and rebuilds buckets in place.
    pub(crate) fn rollback(&mut self, checkpoint: AtomTableCheckpoint) {
        debug_assert!(checkpoint.entries_len <= self.entries.len());
        self.entries.truncate(checkpoint.entries_len);
        self.retained_string_bytes = checkpoint.retained_string_bytes;
        self.buckets.fill(None);
        for (index, entry) in self.entries.iter().enumerate() {
            let bucket = find_vacant_bucket(&self.buckets, entry.hash);
            self.buckets[bucket] = Some(AtomId::from_index(index));
        }
    }

    #[must_use]
    pub fn find(&self, string: &JsString) -> Option<AtomId> {
        self.find_hashed(string, string.hash_with_seed(self.config.hash_seed))
    }

    #[must_use]
    pub fn get(&self, atom: AtomId) -> Option<&JsString> {
        self.entries
            .get(atom.index() as usize)
            .map(|entry| &entry.string)
    }

    #[must_use]
    pub fn stats(&self) -> AtomTableStats {
        AtomTableStats {
            entries: self.entries.len(),
            retained_string_bytes: self.retained_string_bytes,
            initial_bucket_capacity: self.initial_bucket_capacity,
            growth_count: self.growth_count,
            peak_entries: self.peak_entries,
            retained_bucket_capacity: self.buckets.len(),
            retained_entry_capacity: self.entries.capacity(),
            vacant_buckets: self.buckets.len().saturating_sub(self.entries.len()),
        }
    }

    fn find_hashed(&self, string: &JsString, hash: u64) -> Option<AtomId> {
        if self.buckets.is_empty() {
            return None;
        }
        let mask = self.buckets.len() - 1;
        let mut bucket = hash as usize & mask;
        loop {
            let atom = self.buckets[bucket]?;
            let entry = &self.entries[atom.index() as usize];
            if entry.hash == hash && entry.string == *string {
                return Some(atom);
            }
            bucket = (bucket + 1) & mask;
        }
    }

    /// Grows buckets and entry backing together before a push can mutate either published table.
    fn ensure_insert_capacity(&mut self) -> Result<(), AtomTableError> {
        let required = self.entries.len() + 1;
        let needs_growth = self.buckets.is_empty()
            || required.saturating_mul(ATOM_LOAD_DENOMINATOR)
                > self.buckets.len().saturating_mul(ATOM_LOAD_NUMERATOR);
        if !needs_growth {
            return Ok(());
        }
        let target = if self.buckets.is_empty() {
            INITIAL_ATOM_BUCKETS
        } else {
            self.buckets
                .len()
                .checked_mul(2)
                .ok_or(AtomTableError::CapacityOverflow)?
        };
        let entry_target = target
            .saturating_mul(ATOM_LOAD_NUMERATOR)
            .div_ceil(ATOM_LOAD_DENOMINATOR)
            .min(self.config.max_entries as usize)
            .max(required);
        if entry_target > self.entries.capacity() {
            self.entries
                .try_reserve_exact(entry_target - self.entries.len())
                .map_err(|_| AtomTableError::AllocationFailed)?;
        }
        let mut buckets = Vec::new();
        buckets
            .try_reserve_exact(target)
            .map_err(|_| AtomTableError::AllocationFailed)?;
        buckets.resize(target, None);
        for (index, entry) in self.entries.iter().enumerate() {
            let bucket = find_vacant_bucket(&buckets, entry.hash);
            buckets[bucket] = Some(AtomId::from_index(index));
        }
        self.buckets = buckets;
        if self.initial_bucket_capacity == 0 {
            self.initial_bucket_capacity = target;
        } else {
            self.growth_count = self.growth_count.saturating_add(1);
        }
        Ok(())
    }
}

fn find_vacant_bucket(buckets: &[Option<AtomId>], hash: u64) -> usize {
    debug_assert!(buckets.len().is_power_of_two());
    let mask = buckets.len() - 1;
    let mut bucket = hash as usize & mask;
    while buckets[bucket].is_some() {
        bucket = (bucket + 1) & mask;
    }
    bucket
}

#[cfg(test)]
mod tests {
    use super::{AtomHashSeed, AtomTable, AtomTableConfig, AtomTableError};
    use crate::JsString;

    fn table(max_entries: u32, max_string_bytes: usize) -> AtomTable {
        AtomTable::new(AtomTableConfig::new(
            max_entries,
            max_string_bytes,
            AtomHashSeed::new(0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210),
        ))
    }

    #[test]
    fn atoms_deduplicate_equal_code_units_across_backing_widths() {
        let mut atoms = table(8, 64);
        let latin1 = atoms
            .try_intern(JsString::try_from_latin1(&[0xe9]).unwrap())
            .unwrap();
        let utf16 = atoms
            .try_intern(JsString::try_from_utf16(&[0x00e9]).unwrap())
            .unwrap();

        assert_eq!(latin1, utf16);
        assert_eq!(latin1.index(), 0);
        assert_eq!(atoms.get(latin1).unwrap().code_unit_at(0), Some(0x00e9));
        assert_eq!(atoms.stats().entries, 1);
        assert_eq!(atoms.stats().retained_string_bytes, 1);
    }

    #[test]
    fn atom_quotas_apply_after_duplicate_lookup_and_before_publication() {
        let mut atoms = table(1, 1);
        let first = atoms
            .try_intern(JsString::try_from_latin1(b"a").unwrap())
            .unwrap();
        assert_eq!(
            atoms
                .try_intern(JsString::try_from_latin1(b"a").unwrap())
                .unwrap(),
            first
        );
        assert_eq!(
            atoms.try_intern(JsString::try_from_latin1(b"b").unwrap()),
            Err(AtomTableError::EntryLimitExceeded { limit: 1 })
        );
        assert_eq!(atoms.stats().entries, 1);

        let mut byte_limited = table(4, 1);
        assert_eq!(
            byte_limited.try_intern(JsString::try_from_utf16(&[1]).unwrap()),
            Err(AtomTableError::StringBytesLimitExceeded {
                limit: 1,
                used: 0,
                requested: 2,
            })
        );
        assert_eq!(byte_limited.stats().entries, 0);
    }

    #[test]
    /// Growth keeps load bounded, preserves IDs, and exposes retained capacity evidence.
    fn atom_table_rehashes_without_changing_stable_ids() {
        let mut atoms = table(64, 1024);
        let mut ids = Vec::with_capacity(40);
        for index in 0..40 {
            let string = JsString::try_from_str(&format!("key-{index}")).unwrap();
            ids.push(atoms.try_intern(string).unwrap());
        }

        for (index, id) in ids.iter().copied().enumerate() {
            let probe = JsString::try_from_str(&format!("key-{index}")).unwrap();
            assert_eq!(atoms.find(&probe), Some(id));
        }
        let stats = atoms.stats();
        assert_eq!(stats.entries, 40);
        assert_eq!(stats.initial_bucket_capacity, 16);
        assert_eq!(stats.growth_count, 2);
        assert_eq!(stats.retained_bucket_capacity, 64);
        assert!(stats.retained_entry_capacity >= 40);
        assert_eq!(stats.peak_entries, 40);
    }

    #[test]
    fn rollback_removes_only_atoms_published_after_checkpoint() {
        let mut table = AtomTable::new(AtomTableConfig::new(16, 1_024, AtomHashSeed::new(7, 11)));
        let retained = table
            .try_intern(JsString::try_from_str("retained").unwrap())
            .unwrap();
        let checkpoint = table.checkpoint();
        table
            .try_intern(JsString::try_from_str("rolled-back").unwrap())
            .unwrap();

        table.rollback(checkpoint);

        assert_eq!(
            table.find(&JsString::try_from_str("retained").unwrap()),
            Some(retained)
        );
        assert_eq!(
            table.find(&JsString::try_from_str("rolled-back").unwrap()),
            None
        );
        assert_eq!(table.stats().entries, 1);
    }
}
