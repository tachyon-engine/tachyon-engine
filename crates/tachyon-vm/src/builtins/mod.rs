//! ECMAScript builtin slow-path implementations.

mod array;
mod array_buffer;
mod boolean;
mod collections;
mod data_view;
mod date;
mod finalization_registry;
mod global;
mod json;
mod map_upsert;
mod math;
pub(crate) mod object;
mod promise_combinator;
mod regexp;
mod string;
mod symbol;
pub(crate) mod typed_array;
mod uri;
mod weak_collections;
mod weak_ref;

pub(crate) use date::PendingDateNumericArguments;
