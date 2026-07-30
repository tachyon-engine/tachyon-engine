//! `Iterator.prototype.drop` surface entry point.

use super::super::*;
use super::IteratorHelperKind;

impl Isolate {
    /// Converts drop's limit before observing and caching the direct next method.
    pub(crate) fn begin_iterator_drop(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.begin_iterator_limit_helper(site, IteratorHelperKind::Drop)
    }
}
