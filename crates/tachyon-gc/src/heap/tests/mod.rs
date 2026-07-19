use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::{AllocationSpace, GcExternalMemory, Heap, HeapAllocationError, HeapLimit};
use crate::{
    BarrierVerificationError, CardBitmap, Ephemeron, FinalizationRegistration,
    ForcedCollectionMode, GC_HEADER_EXTERNAL_BYTES_FLAG, GcRef, GcTriggerConfig,
    HeapReferenceError, ManagedAllocationError, MinorCollectionError, RawHeapRef, SPAN_SIZE_BYTES,
    SpanSpace, Trace, Tracer, TypeRegistrationError, TypeRegistry, WeakGcRef,
};
use tachyon_value::Value;

mod allocation;
mod barriers;
mod fixtures;
mod major;
mod minor;
mod stress;
mod triggers;
mod weak_finalization;

use fixtures::*;
