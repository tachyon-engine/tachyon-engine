//! Fixed same-kind `%TypedArray.prototype.toReversed%` implementation.

use super::*;

impl Isolate {
    /// Copies one fixed view and reverses only the independent result backing.
    pub(crate) fn begin_typed_array_to_reversed(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let source = site.this_value;
        let snapshot = self.validated_typed_array_snapshot(source)?;

        // The destination register is the moving root for source until both result allocations
        // finish. The raw byte copy itself cannot trigger an engine collection.
        self.write(site.caller_base, site.destination, source)?;
        let target = self.create_fixed_typed_array_same_kind(snapshot.kind, snapshot.length)?;
        let source = self.read(site.caller_base, site.destination)?;
        self.copy_same_kind_typed_array(source, target)?;
        self.write(site.caller_base, site.destination, target)?;

        let mut target_site = *site;
        target_site.this_value = self.read(site.caller_base, site.destination)?;
        self.begin_typed_array_reverse(&target_site)
    }
}
