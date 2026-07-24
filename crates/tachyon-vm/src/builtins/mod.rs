//! ECMAScript builtin slow-path implementations.

mod array;
mod array_buffer;
mod boolean;
mod collections;
mod date;
mod global;
mod json;
mod map_upsert;
mod math;
pub(crate) mod object;
mod regexp;
mod string;
mod symbol;
mod uri;
mod weak_collections;

pub(crate) use date::PendingDateNumericArguments;
