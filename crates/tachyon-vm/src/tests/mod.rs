use std::sync::Arc;

use tachyon_bytecode::{
    BindingLocation, BindingPlanEntry, Bytecode, BytecodeBuilder, BytecodeConstant,
    CompiledFunctionTemplate, CompiledModule, EnvironmentRecordKind, EnvironmentSlotMetadata,
    FunctionId, FunctionKind, FunctionLayout, FunctionMetadata, OperandWidth, SourceMapEntry,
    SourceSpan, encode_instruction,
};
use tachyon_gc::{ForcedCollectionMode, GcRef, HeapLimit, SPAN_SIZE_BYTES, Tracer};
use tachyon_value::RawHeapRef;

use super::*;

mod accessors;
mod calls;
mod control;
mod dispatch;
mod environments;
mod fixtures;
mod properties;
mod realm_gc;
