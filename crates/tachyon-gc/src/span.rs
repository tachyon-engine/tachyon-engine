//! Small-object span side metadata shared by young and major collection.

use crate::{
    CARD_BITMAP_WORDS, CARD_SIZE_BYTES, CARDS_PER_SPAN, CollectionEpoch, MAX_SMALL_OBJECT_SLOTS,
    MINIMUM_SLOT_SIZE_BYTES, SLOT_BITMAP_WORDS, SPAN_SIZE_BYTES, SpanOffset,
};

/// A homogeneous non-moving generation assigned to an entire small-object span.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpanSpace {
    Eden,
    Survivor { age: u8 },
    Old,
}

/// Monotonic debug identity for each reuse of one stable logical table entry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct SpanReuseGeneration(u32);

impl SpanReuseGeneration {
    /// The first use of a newly appended logical table entry.
    pub const INITIAL: Self = Self(0);

    /// Advances with wrapping semantics; this generation is diagnostic, not part of `GcRef` identity.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }

    /// Returns the number of completed releases for this logical entry.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Incremental sweep progress retained in side metadata rather than the object header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SweepState {
    Swept,
    Pending(CollectionEpoch),
    InProgress(CollectionEpoch),
}

/// A bitmap index validated against the largest possible small-object slot count.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct SlotIndex(u16);

impl SlotIndex {
    /// Creates a slot index shared by allocation and mark bitmaps.
    #[must_use]
    pub const fn new(index: u16) -> Option<Self> {
        if index as usize >= MAX_SMALL_OBJECT_SLOTS {
            return None;
        }
        Some(Self(index))
    }

    /// Returns the checked per-span bitmap index.
    #[must_use]
    pub const fn index(self) -> u16 {
        self.0
    }
}

/// A validated homogeneous allocation size for one small-object span.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct SizeClass(u16);

impl SizeClass {
    /// Accepts representable 16-byte-multiple slots that leave room after the reserved offset.
    #[must_use]
    pub const fn new(slot_size: u16) -> Option<Self> {
        if slot_size < MINIMUM_SLOT_SIZE_BYTES as u16
            || !(slot_size as usize).is_multiple_of(MINIMUM_SLOT_SIZE_BYTES)
        {
            return None;
        }
        Some(Self(slot_size))
    }

    /// Returns the bytes occupied by each slot, including its object header.
    #[must_use]
    pub const fn slot_size(self) -> u16 {
        self.0
    }

    /// Returns the number of complete slots after the reserved zero-offset region.
    #[must_use]
    pub const fn slot_count(self) -> u16 {
        ((SPAN_SIZE_BYTES - MINIMUM_SLOT_SIZE_BYTES) / self.slot_size() as usize) as u16
    }

    /// Resolves a slot index to its checked in-span byte offset.
    #[must_use]
    pub const fn offset_for_slot(self, slot: SlotIndex) -> Option<SpanOffset> {
        if slot.index() >= self.slot_count() {
            return None;
        }
        let offset = MINIMUM_SLOT_SIZE_BYTES + slot.index() as usize * self.slot_size() as usize;
        SpanOffset::new(offset as u16)
    }

    /// Resolves an aligned object offset to its slot index for verifier and bitmap access.
    #[must_use]
    pub const fn slot_for_offset(self, offset: SpanOffset) -> Option<SlotIndex> {
        let offset = offset.get() as usize;
        if offset < MINIMUM_SLOT_SIZE_BYTES {
            return None;
        }
        let relative = offset - MINIMUM_SLOT_SIZE_BYTES;
        if !relative.is_multiple_of(self.slot_size() as usize) {
            return None;
        }
        let slot = (relative / self.slot_size() as usize) as u16;
        if slot < self.slot_count() {
            SlotIndex::new(slot)
        } else {
            None
        }
    }
}

/// Fixed-capacity allocation bitmap sized for the worst-case 16-byte size class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllocationBitmap {
    bits: SlotBitmap,
}

impl AllocationBitmap {
    /// Creates an empty bitmap without a later capacity growth path.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bits: SlotBitmap::new(),
        }
    }

    /// Returns whether a slot currently contains an initialized object.
    #[must_use]
    #[inline(always)]
    pub fn is_allocated(&self, slot: SlotIndex) -> bool {
        self.bits.contains(slot)
    }

    /// Records a newly initialized slot and reports whether it was previously free.
    #[inline(always)]
    pub fn allocate(&mut self, slot: SlotIndex) -> bool {
        self.bits.insert(slot)
    }

    /// Clears an allocated slot and reports whether it previously held an object.
    #[inline(always)]
    pub fn free(&mut self, slot: SlotIndex) -> bool {
        self.bits.remove(slot)
    }
}

impl Default for AllocationBitmap {
    fn default() -> Self {
        Self::new()
    }
}

/// Epoch-qualified mark bits; an epoch mismatch makes all stored bits logically white.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkBitmap {
    epoch: Option<CollectionEpoch>,
    bits: SlotBitmap,
}

impl MarkBitmap {
    /// Creates a logically white bitmap without assigning a collection epoch.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            epoch: None,
            bits: SlotBitmap::new(),
        }
    }

    /// Returns whether the slot is marked in exactly the requested collection epoch.
    #[must_use]
    #[inline(always)]
    pub fn is_marked(&self, slot: SlotIndex, epoch: CollectionEpoch) -> bool {
        self.epoch == Some(epoch) && self.bits.contains(slot)
    }

    /// Lazily clears stale bits, marks a slot, and reports whether it changed white to gray.
    #[inline(always)]
    pub fn mark(&mut self, slot: SlotIndex, epoch: CollectionEpoch) -> bool {
        if self.epoch != Some(epoch) {
            self.bits.clear();
            self.epoch = Some(epoch);
        }
        self.bits.insert(slot)
    }

    /// Physically clears this span during the coordinated `CollectionEpoch` overflow path.
    pub fn reset_for_epoch_overflow(&mut self) {
        self.bits.clear();
        self.epoch = None;
    }
}

impl Default for MarkBitmap {
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed 512-byte-granularity remembered-set cards for one logical span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CardBitmap {
    words: [u64; CARD_BITMAP_WORDS],
}

impl CardBitmap {
    /// Creates a clean card table with its final per-span capacity.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            words: [0; CARD_BITMAP_WORDS],
        }
    }

    /// Marks the card containing an already validated in-span address.
    #[inline(always)]
    pub fn mark(&mut self, offset: SpanOffset) -> bool {
        let card = offset.get() as usize / CARD_SIZE_BYTES;
        debug_assert!(card < CARDS_PER_SPAN);
        insert(&mut self.words, card)
    }

    /// Returns whether the card containing an address must be scanned by minor GC.
    #[must_use]
    #[inline(always)]
    pub fn is_dirty(&self, offset: SpanOffset) -> bool {
        let card = offset.get() as usize / CARD_SIZE_BYTES;
        debug_assert!(card < CARDS_PER_SPAN);
        contains(&self.words, card)
    }

    /// Clears all remembered cards after their edges have been rebuilt or proven young-free.
    pub fn clear(&mut self) {
        self.words.fill(0);
    }
}

impl Default for CardBitmap {
    fn default() -> Self {
        Self::new()
    }
}

/// Complete side metadata for one homogeneous small-object span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmallSpanMetadata {
    size_class: SizeClass,
    space: SpanSpace,
    allocations: AllocationBitmap,
    marks: MarkBitmap,
    cards: CardBitmap,
    bump_cursor: u16,
    free_list_head: Option<SlotIndex>,
    sweep_state: SweepState,
    reuse_generation: SpanReuseGeneration,
    allocated_slots: u16,
    allocated_bytes: u32,
}

impl SmallSpanMetadata {
    /// Creates fully reserved side metadata without a per-object allocation or bitmap growth path.
    #[must_use]
    pub const fn new(
        size_class: SizeClass,
        space: SpanSpace,
        reuse_generation: SpanReuseGeneration,
    ) -> Self {
        Self {
            size_class,
            space,
            allocations: AllocationBitmap::new(),
            marks: MarkBitmap::new(),
            cards: CardBitmap::new(),
            bump_cursor: 0,
            free_list_head: None,
            sweep_state: SweepState::Swept,
            reuse_generation,
            allocated_slots: 0,
            allocated_bytes: 0,
        }
    }

    /// Returns the homogeneous allocation size for this span.
    #[must_use]
    pub const fn size_class(&self) -> SizeClass {
        self.size_class
    }

    /// Returns the current non-moving cohort.
    #[must_use]
    pub const fn space(&self) -> SpanSpace {
        self.space
    }

    /// Returns the next never-used slot index for Eden bump allocation.
    #[must_use]
    pub const fn bump_cursor(&self) -> u16 {
        self.bump_cursor
    }

    /// Returns the first reusable dead slot for old-space allocation.
    #[must_use]
    pub const fn free_list_head(&self) -> Option<SlotIndex> {
        self.free_list_head
    }

    /// Returns this span's incremental sweep progress.
    #[must_use]
    pub const fn sweep_state(&self) -> SweepState {
        self.sweep_state
    }

    /// Returns the diagnostic generation of this table entry's storage.
    #[must_use]
    pub const fn reuse_generation(&self) -> SpanReuseGeneration {
        self.reuse_generation
    }

    /// Returns the number of initialized object slots.
    #[must_use]
    pub const fn allocated_slots(&self) -> u16 {
        self.allocated_slots
    }

    /// Returns initialized slot bytes for isolate accounting.
    #[must_use]
    pub const fn allocated_bytes(&self) -> u32 {
        self.allocated_bytes
    }

    /// Returns the lifetime bitmap independently from collection marks.
    #[must_use]
    pub const fn allocations(&self) -> &AllocationBitmap {
        &self.allocations
    }

    /// Returns epoch-qualified collection marks.
    #[must_use]
    pub const fn marks(&self) -> &MarkBitmap {
        &self.marks
    }

    /// Returns old-to-young remembered cards.
    #[must_use]
    pub const fn cards(&self) -> &CardBitmap {
        &self.cards
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SlotBitmap {
    words: [u64; SLOT_BITMAP_WORDS],
}

impl SlotBitmap {
    const fn new() -> Self {
        Self {
            words: [0; SLOT_BITMAP_WORDS],
        }
    }

    #[inline(always)]
    fn contains(&self, slot: SlotIndex) -> bool {
        let slot = slot.index() as usize;
        contains(&self.words, slot)
    }

    #[inline(always)]
    fn insert(&mut self, slot: SlotIndex) -> bool {
        let slot = slot.index() as usize;
        insert(&mut self.words, slot)
    }

    #[inline(always)]
    fn remove(&mut self, slot: SlotIndex) -> bool {
        let slot = slot.index() as usize;
        let (word, mask) = word_and_mask(slot);
        let was_present = self.words[word] & mask != 0;
        self.words[word] &= !mask;
        was_present
    }

    fn clear(&mut self) {
        self.words.fill(0);
    }
}

#[inline(always)]
fn contains<const N: usize>(words: &[u64; N], bit: usize) -> bool {
    let (word, mask) = word_and_mask(bit);
    words[word] & mask != 0
}

#[inline(always)]
fn insert<const N: usize>(words: &mut [u64; N], bit: usize) -> bool {
    let (word, mask) = word_and_mask(bit);
    let was_present = words[word] & mask != 0;
    words[word] |= mask;
    !was_present
}

#[inline(always)]
const fn word_and_mask(bit: usize) -> (usize, u64) {
    (
        bit / u64::BITS as usize,
        1_u64 << (bit % u64::BITS as usize),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        AllocationBitmap, CardBitmap, MarkBitmap, SizeClass, SlotIndex, SmallSpanMetadata,
        SpanReuseGeneration, SpanSpace, SweepState,
    };
    use crate::{CollectionEpoch, MAX_SMALL_OBJECT_SLOTS, SpanOffset};

    #[test]
    /// Covers validation and both directions of the size-class slot mapping contract.
    fn size_classes_resolve_only_their_own_slot_boundaries() {
        assert_eq!(SizeClass::new(0), None);
        assert_eq!(SizeClass::new(15), None);
        assert_eq!(SizeClass::new(17), None);

        let class = SizeClass::new(16).expect("minimum size class");
        assert_eq!(class.slot_count() as usize, MAX_SMALL_OBJECT_SLOTS);
        assert_eq!(
            class
                .offset_for_slot(SlotIndex::new(0).unwrap())
                .unwrap()
                .get(),
            16
        );
        assert_eq!(SlotIndex::new(class.slot_count()), None);
        assert_eq!(
            class.slot_for_offset(SpanOffset::new(16).unwrap()),
            SlotIndex::new(0)
        );
        assert_eq!(class.slot_for_offset(SpanOffset::new(17).unwrap()), None);
    }

    #[test]
    fn allocation_and_mark_bits_have_distinct_lifetimes() {
        let epoch = CollectionEpoch::INITIAL;
        let mut allocations = AllocationBitmap::new();
        let mut marks = MarkBitmap::new();
        let first = SlotIndex::new(0).unwrap();

        assert!(allocations.allocate(first));
        assert!(!allocations.allocate(first));
        assert!(marks.mark(first, epoch));
        assert!(!marks.mark(first, epoch));
        assert!(allocations.is_allocated(first));
        assert!(marks.is_marked(first, epoch));
        assert!(allocations.free(first));
        assert!(!allocations.is_allocated(first));
        assert!(marks.is_marked(first, epoch));
    }

    #[test]
    fn new_epoch_lazily_clears_stale_mark_bits() {
        let first = CollectionEpoch::INITIAL;
        let second = first.next().unwrap();
        let mut marks = MarkBitmap::new();
        let slot_one = SlotIndex::new(1).unwrap();
        let slot_two = SlotIndex::new(2).unwrap();

        assert!(marks.mark(slot_one, first));
        assert!(!marks.is_marked(slot_one, second));
        assert!(marks.mark(slot_two, second));
        assert!(!marks.is_marked(slot_one, second));
        assert!(marks.is_marked(slot_two, second));

        marks.reset_for_epoch_overflow();
        assert!(!marks.is_marked(slot_two, second));
        assert!(marks.mark(slot_one, CollectionEpoch::INITIAL));
    }

    #[test]
    fn cards_cover_both_ends_of_a_span_without_growth() {
        let first = SpanOffset::new(1).unwrap();
        let last = SpanOffset::new(u16::MAX).unwrap();
        let mut cards = CardBitmap::new();

        assert!(cards.mark(first));
        assert!(cards.mark(last));
        assert!(!cards.mark(last));
        assert!(cards.is_dirty(first));
        assert!(cards.is_dirty(last));
        cards.clear();
        assert!(!cards.is_dirty(first));
        assert!(!cards.is_dirty(last));
    }

    #[test]
    fn span_spaces_encode_permanent_non_moving_cohorts() {
        assert_ne!(SpanSpace::Eden, SpanSpace::Survivor { age: 1 });
        assert_ne!(SpanSpace::Survivor { age: 1 }, SpanSpace::Old);
    }

    #[test]
    /// Verifies every lifecycle, accounting, and fixed-bitmap field before first allocation.
    fn small_span_metadata_starts_fully_reserved_and_empty() {
        let size_class = SizeClass::new(32).unwrap();
        let generation = SpanReuseGeneration::INITIAL.next();
        let metadata =
            SmallSpanMetadata::new(size_class, SpanSpace::Survivor { age: 2 }, generation);

        assert_eq!(metadata.size_class(), size_class);
        assert_eq!(metadata.space(), SpanSpace::Survivor { age: 2 });
        assert_eq!(metadata.bump_cursor(), 0);
        assert_eq!(metadata.free_list_head(), None);
        assert_eq!(metadata.sweep_state(), SweepState::Swept);
        assert_eq!(metadata.reuse_generation(), generation);
        assert_eq!(metadata.allocated_slots(), 0);
        assert_eq!(metadata.allocated_bytes(), 0);
        assert!(
            !metadata
                .allocations()
                .is_allocated(SlotIndex::new(0).unwrap())
        );
        assert!(
            !metadata
                .marks()
                .is_marked(SlotIndex::new(0).unwrap(), CollectionEpoch::INITIAL)
        );
        assert!(!metadata.cards().is_dirty(SpanOffset::new(16).unwrap()));
    }
}
