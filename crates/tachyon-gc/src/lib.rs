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
mod layout;
mod span;
mod trace;

pub use descriptor::{DropObjectFn, TraceObjectFn, TypeDescriptor};
pub use epoch::{CollectionEpoch, CollectionEpochOverflow};
pub use handle::GcRef;
pub use layout::{
    CARD_BITMAP_WORDS, CARD_SIZE_BYTES, CARDS_PER_SPAN, GC_HEADER_SIZE_BYTES, GcHeader, GcTypeId,
    LOGICAL_ADDRESS_SPACE_BYTES, MAX_LOGICAL_HEAP_ADDRESS, MAX_LOGICAL_SPANS,
    MAX_SMALL_OBJECT_SLOTS, MINIMUM_SLOT_SIZE_BYTES, SLOT_BITMAP_WORDS, SPAN_SIZE_BYTES,
};
pub use span::{AllocationBitmap, CardBitmap, MarkBitmap, SizeClass, SlotIndex, SpanSpace};
pub use tachyon_value::{SpanId, SpanOffset};
pub use trace::{Trace, Tracer};
