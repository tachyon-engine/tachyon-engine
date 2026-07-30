use std::sync::Arc;

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{
    fixtures::{test_isolate, test_isolate_with_heap_spans},
    *,
};

mod cases;
mod fixtures_api;
mod fixtures_graph;
mod gc;
mod helpers;
mod resources;

use fixtures_api::*;
use fixtures_graph::*;
use gc::*;
use helpers::*;
use resources::*;
