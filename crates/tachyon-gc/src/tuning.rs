//! Central GC capacity guesses that may change after benchmark evidence.

/// First table reservation: eight entries cost little while covering tiny scripts without regrowth.
pub(crate) const INITIAL_SPAN_TABLE_CAPACITY: usize = 8;
/// First free-range reservation; most heaps retain one or two coalesced ranges.
pub(crate) const INITIAL_FREE_RANGE_CAPACITY: usize = 4;
/// First immutable descriptor reservation; builtins and a modest extension set fit without regrowth.
pub(crate) const INITIAL_TYPE_DESCRIPTOR_CAPACITY: usize = 64;
/// First gray reservation balances tiny-script footprint against common graph breadth.
pub(crate) const INITIAL_GRAY_QUEUE_CAPACITY: usize = 256;
/// First major-sweep reservation; each entry is one span owner rather than one object.
pub(crate) const INITIAL_SWEEP_WORKLIST_CAPACITY: usize = 64;
/// First temporary-root reservation; 64 four-byte entries retain only 256 bytes for small scopes.
pub(crate) const INITIAL_TEMPORARY_ROOT_CAPACITY: usize = 64;
/// First persistent-root slab reservation; one entry is retained per long-lived host root.
pub(crate) const INITIAL_PERSISTENT_ROOT_CAPACITY: usize = 64;
/// First weak-owner reservation; ordinary heaps contain few weak containers.
pub(crate) const INITIAL_WEAK_OWNER_CAPACITY: usize = 64;
/// First kept-object reservation; one job normally dereferences few WeakRef targets.
pub(crate) const INITIAL_KEPT_OBJECT_CAPACITY: usize = 64;
/// First pending-finalization reservation; cleanup jobs normally drain promptly after collection.
pub(crate) const INITIAL_FINALIZATION_QUEUE_CAPACITY: usize = 64;
/// Numerator for bounded 1.5x metadata-container growth.
pub(crate) const CAPACITY_GROWTH_NUMERATOR: usize = 3;
/// Denominator for bounded 1.5x metadata-container growth.
pub(crate) const CAPACITY_GROWTH_DENOMINATOR: usize = 2;
/// Initial whole-span promotion age; corpus and pause/fragmentation benchmarks may retune it.
pub(crate) const YOUNG_PROMOTION_AGE: u8 = 2;

/// Initial 1.0 size classes; benchmark evidence may refine spacing without changing `GcRef`.
pub(crate) const SMALL_SIZE_CLASSES: [u16; 28] = [
    16, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024,
    1280, 1536, 1792, 2048, 2560, 3072, 3584, 4096,
];
