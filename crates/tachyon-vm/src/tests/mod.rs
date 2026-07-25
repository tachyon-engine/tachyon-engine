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
mod array_buffer;
mod array_concat;
mod array_copy;
mod array_copy_within;
mod array_fill;
mod array_filter;
mod array_find;
mod array_flat;
mod array_flat_map;
mod array_insert;
mod array_iterator;
mod array_join;
mod array_map;
mod array_predicate;
mod array_reduce;
mod array_remove;
mod array_reverse;
mod array_search;
mod array_slice;
mod array_splice;
mod array_static;
mod array_to_sorted;
mod assign;
mod calls;
mod class;
mod control;
mod conversion;
mod data_view;
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
mod typed_array;
mod uri;
