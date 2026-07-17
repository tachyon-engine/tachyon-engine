#![deny(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout,
    unsafe_op_in_unsafe_fn
)]
//! Exact garbage-collection primitives independent of JavaScript object semantics.
//!
//! This crate intentionally has no host I/O surface.

mod descriptor;
mod epoch;
mod handle;
mod heap;
mod layout;
mod registry;
mod span;
mod storage;
mod table;
mod trace;
mod tuning;

pub use descriptor::{DropObjectFn, GcType, TraceObjectFn, TypeDescriptor};
pub use epoch::{CollectionEpoch, CollectionEpochOverflow};
pub use handle::GcRef;
pub use heap::{AllocationSpace, HeapAllocationError, SmallHeap, SmallHeapLimit};
pub use layout::{
    CARD_BITMAP_WORDS, CARD_SIZE_BYTES, CARDS_PER_SPAN, GC_HEADER_SIZE_BYTES, GcHeader, GcTypeId,
    LOGICAL_ADDRESS_SPACE_BYTES, MAX_LOGICAL_HEAP_ADDRESS, MAX_LOGICAL_SPANS,
    MAX_SMALL_OBJECT_SLOTS, MINIMUM_SLOT_SIZE_BYTES, SLOT_BITMAP_WORDS, SPAN_SIZE_BYTES,
    SmallObjectLayout, SmallObjectLayoutError,
};
pub use registry::{TypeRegistrationError, TypeRegistry};
pub use span::{
    AllocationBitmap, CardBitmap, MarkBitmap, SizeClass, SlotIndex, SmallSpanMetadata,
    SpanReuseGeneration, SpanSpace, SweepState,
};
pub use storage::{SpanStorage, SpanStorageAccessError, SpanStorageAllocationError};
pub use table::{HeapReferenceError, SmallAllocationError, SpanTable, SpanTableError};
pub use tachyon_value::{RawHeapRef, SpanId, SpanOffset};
pub use trace::{Trace, Tracer};
