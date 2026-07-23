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
mod array_filter;
mod assign;
mod calls;
mod class;
mod control;
mod conversion;
mod date;
mod define_properties;
mod dispatch;
mod environments;
mod errors;
mod eval;
mod finally;
mod fixtures;
mod grouping;
mod object_prototype;
mod promise;
mod properties;
mod property_keys;
mod proxy;
mod realm_gc;
mod string_case;
mod uri;
