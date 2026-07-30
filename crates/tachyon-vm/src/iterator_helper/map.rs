//! `Iterator.prototype.map` surface entry point.

use super::super::*;
use super::IteratorHelperKind;

impl Isolate {
    /// Validates map's receiver and mapper before observing the direct next method.
    pub(crate) fn begin_iterator_map(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_iterator_callback_helper(site, IteratorHelperKind::Map)
    }
}
