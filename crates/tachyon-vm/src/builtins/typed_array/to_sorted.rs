//! Fixed same-kind `%TypedArray.prototype.toSorted%` preparation.

use super::*;

impl Isolate {
    /// Creates an independent same-kind copy, then enters the shared stable sort machinery.
    pub(crate) fn begin_typed_array_to_sorted(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let comparator = self.call_argument(site, 0)?.unwrap_or(undefined);
        if comparator.as_immediate() != Some(Immediate::Undefined)
            && !self.is_callable_value(comparator)?
        {
            return Err(ExecutionError::NonCallable(comparator));
        }

        let source = site.this_value;
        let snapshot = self.typed_array_snapshot(source)?;
        self.typed_array_backing(snapshot.buffer)?;
        self.write(site.caller_base, site.destination, source)?;
        let target = self.create_fixed_typed_array_same_kind(snapshot.kind, snapshot.length)?;
        let source = self.read(site.caller_base, site.destination)?;
        self.copy_same_kind_typed_array(source, target)?;
        self.write(site.caller_base, site.destination, target)?;

        let mut target_site = *site;
        target_site.this_value = self.read(site.caller_base, site.destination)?;
        self.begin_typed_array_sort(&target_site)
    }

    /// Allocates the default active-Realm fixed TypedArray for one kind and element length.
    pub(super) fn create_fixed_typed_array_same_kind(
        &mut self,
        kind: TypedArrayKind,
        length: usize,
    ) -> Result<Value, ExecutionError> {
        let byte_length = length
            .checked_mul(kind.byte_width())
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let buffer_prototype = self
            .realm
            .array_buffer_prototype
            .expect("ArrayBuffer prototype initializes before TypedArray copy methods");
        let buffer =
            self.allocate_array_buffer_object(byte_length, byte_length, false, buffer_prototype)?;
        let prototype = self.realm.typed_array_prototypes[kind.index()]
            .expect("TypedArray prototype initializes before copy methods");
        self.allocate_typed_array_view(buffer, 0, length, kind, prototype)
    }
}
