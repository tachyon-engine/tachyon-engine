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

mod barrier;
mod descriptor;
mod eden;
mod epoch;
mod finalization;
mod gray;
mod handle;
mod heap;
mod layout;
mod mark;
mod pause;
mod persistent;
mod registry;
mod roots;
mod scope;
mod span;
mod storage;
mod sweep;
mod table;
mod trace;
mod trigger;
mod tuning;
mod weak;

pub use barrier::{BarrierVerificationError, BarrierVerificationStats};
pub use descriptor::{DropObjectFn, GcAllocationPolicy, GcType, TraceObjectFn, TypeDescriptor};
pub use eden::EdenPoolStats;
pub use epoch::{CollectionEpoch, CollectionEpochOverflow};
pub use finalization::{
    FinalizationQueueError, FinalizationQueueStats, FinalizationRegistration, PendingFinalization,
};
pub use gray::{GrayQueueError, GrayQueueStats};
pub use handle::GcRef;
pub use heap::{
    AllocationSpace, Heap, HeapAllocationError, HeapLimit, MajorCollectionError,
    MajorCollectionStats, ManagedAllocationError, MinorCollectionError, MinorCollectionStats,
};
pub use layout::{
    CARD_BITMAP_WORDS, CARD_SIZE_BYTES, CARDS_PER_SPAN, GC_HEADER_SIZE_BYTES, GcHeader, GcTypeId,
    LOGICAL_ADDRESS_SPACE_BYTES, MAX_LOGICAL_HEAP_ADDRESS, MAX_LOGICAL_OBJECT_COUNT,
    MAX_LOGICAL_SPANS, MAX_SMALL_OBJECT_SLOTS, MINIMUM_SLOT_SIZE_BYTES, ObjectLayout,
    SLOT_BITMAP_WORDS, SPAN_SIZE_BYTES, SmallObjectLayout, SmallObjectLayoutError,
};
pub use mark::{MarkError, MarkStats, YoungMarkStats};
pub use pause::{CollectionKind, GcPauseStats, PauseHistogramStats};
pub use persistent::{PersistentRootError, PersistentRootId, PersistentRootStats};
pub use registry::{TypeRegistrationError, TypeRegistry};
pub use roots::{KeptObjectError, KeptObjectStats, TemporaryRootError, TemporaryRootStats};
pub use scope::{
    Local, NoGcBorrowError, NoGcScope, PersistentResolveError, RootError, RunningScope,
    ScopedAllocationError,
};
pub use span::{
    AllocationBitmap, CardBitmap, LargeSpanMetadata, MarkBitmap, SizeClass, SlotIndex,
    SmallSpanMetadata, SpanReuseGeneration, SpanSpace, SweepState,
};
pub use storage::{SpanStorage, SpanStorageAccessError, SpanStorageAllocationError};
pub use sweep::{MinorSweepStats, SweepError, SweepStats, SweepWorklistError, SweepWorklistStats};
pub use table::{
    HeapReferenceError, LargeAllocationError, LargeReclaim, SmallAllocationError, SpanTable,
    SpanTableError,
};
pub use tachyon_value::{RawHeapRef, SpanId, SpanOffset};
pub use trace::{Trace, Tracer};
pub use trigger::{
    CollectionAction, CollectionReason, ForcedCollectionMode, GcTriggerConfig,
    GcTriggerConfigError, GcTriggerStats,
};
pub use weak::{Ephemeron, WeakGcRef, WeakOwnerError, WeakOwnerStats};
