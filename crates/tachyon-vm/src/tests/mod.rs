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
mod bigint;
mod call_spread;
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
mod finalization_registry;
mod finally;
mod fixtures;
mod generator;
mod grouping;
mod object_prototype;
mod promise;
mod properties;
mod property_keys;
mod proxy;
mod realm_gc;
mod regexp_accessors;
mod regexp_escape;
mod regexp_exec;
mod regexp_match_all;
mod regexp_replace;
mod regexp_search;
mod signals;
mod string_case;
mod string_replace_all;
mod string_split;
mod typed_array;
mod typed_array_at;
mod typed_array_callback;
mod typed_array_copy_within;
mod typed_array_fill;
mod typed_array_includes;
mod typed_array_join;
mod typed_array_reverse;
mod typed_array_search;
mod typed_array_set;
mod typed_array_slice;
mod typed_array_subarray;
mod typed_array_with;
mod uri;
mod weak_ref;
