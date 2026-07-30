//! `Iterator.prototype.take` surface entry point.

use super::super::*;
use super::IteratorHelperKind;

impl Isolate {
    /// Converts take's limit before observing and caching the direct next method.
    pub(crate) fn begin_iterator_take(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_iterator_limit_helper(site, IteratorHelperKind::Take)
    }
}
