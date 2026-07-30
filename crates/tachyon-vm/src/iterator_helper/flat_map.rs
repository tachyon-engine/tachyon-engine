//! `Iterator.prototype.flatMap` surface entry point.

use super::super::*;
use super::IteratorHelperKind;

impl Isolate {
    /// Validates flatMap's receiver and mapper before caching the outer next method.
    pub(crate) fn begin_iterator_flat_map(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_iterator_callback_helper(site, IteratorHelperKind::FlatMap)
    }
}
