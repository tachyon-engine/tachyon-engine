//! `Iterator.prototype.filter` surface entry point.

use super::super::*;
use super::IteratorHelperKind;

impl Isolate {
    /// Validates filter's receiver and predicate before observing the direct next method.
    pub(crate) fn begin_iterator_filter(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_iterator_callback_helper(site, IteratorHelperKind::Filter)
    }
}
