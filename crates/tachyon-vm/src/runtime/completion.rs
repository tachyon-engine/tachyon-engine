//! Explicit ECMAScript completion records and native continuation storage.

use tachyon_bytecode::WordOffset;
use tachyon_gc::{Trace, Tracer};
use tachyon_value::Value;

use super::fiber::NativeContinuation;

/// The five completion kinds defined by ECMAScript evaluation algorithms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompletionKind {
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "M6 finally lowering saves normal completions")
    )]
    Normal,
    Throw,
    Return,
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "M6 finally lowering saves targeted break completions"
        )
    )]
    Break,
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "M6 finally lowering saves targeted continue completions"
        )
    )]
    Continue,
}

/// One validated language completion; native callback state is deliberately separate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CompletionRecord {
    kind: CompletionKind,
    value: Option<Value>,
    target: Option<WordOffset>,
}

impl CompletionRecord {
    #[inline(always)]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "M6 finally lowering saves normal completions")
    )]
    pub(crate) const fn normal(value: Option<Value>) -> Self {
        Self {
            kind: CompletionKind::Normal,
            value,
            target: None,
        }
    }

    #[inline(always)]
    pub(crate) const fn throw(value: Value) -> Self {
        Self {
            kind: CompletionKind::Throw,
            value: Some(value),
            target: None,
        }
    }

    #[inline(always)]
    pub(crate) const fn return_value(value: Value) -> Self {
        Self {
            kind: CompletionKind::Return,
            value: Some(value),
            target: None,
        }
    }

    #[inline(always)]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "M6 finally lowering saves break targets")
    )]
    pub(crate) const fn break_target(value: Option<Value>, target: WordOffset) -> Self {
        Self {
            kind: CompletionKind::Break,
            value,
            target: Some(target),
        }
    }

    #[inline(always)]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "M6 finally lowering saves continue targets")
    )]
    pub(crate) const fn continue_target(value: Option<Value>, target: WordOffset) -> Self {
        Self {
            kind: CompletionKind::Continue,
            value,
            target: Some(target),
        }
    }

    #[inline(always)]
    pub(crate) const fn kind(self) -> CompletionKind {
        self.kind
    }

    #[inline(always)]
    pub(crate) const fn value(self) -> Option<Value> {
        self.value
    }

    #[inline(always)]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "M6 finally replay consumes completion targets")
    )]
    pub(crate) const fn target(self) -> Option<WordOffset> {
        self.target
    }
}

impl Trace for CompletionRecord {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.value.trace(tracer);
    }
}

/// One slot in the shared completion/native-continuation stack.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CompletionEntry {
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "M6 finally lowering publishes language completions"
        )
    )]
    Language(CompletionRecord),
    Native(NativeContinuation),
}

impl Trace for CompletionEntry {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        match self {
            Self::Language(record) => record.trace(tracer),
            Self::Native(continuation) => continuation.trace(tracer),
        }
    }
}

/// Failure to grow a completion stack within its host-supplied hard bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompletionStackError {
    Limit { limit: u32, requested: u32 },
    AllocationFailed,
}

/// Checked storage shared by saved language completions and native callback trampolines.
#[derive(Debug)]
pub(crate) struct CompletionStack {
    entries: Vec<CompletionEntry>,
    limit: u32,
}

impl Default for CompletionStack {
    fn default() -> Self {
        Self::new(u32::MAX)
    }
}

impl CompletionStack {
    #[inline]
    pub(crate) const fn new(limit: u32) -> Self {
        Self {
            entries: Vec::new(),
            limit,
        }
    }

    /// Reconfigures an empty stack when an isolate begins a fresh execution.
    #[inline]
    pub(crate) fn set_limit(&mut self, limit: u32) {
        debug_assert!(self.entries.is_empty());
        self.limit = limit;
    }

    /// Reserves enough slots for one verified operation without exceeding the host hard limit.
    pub(crate) fn reserve(&mut self, additional: usize) -> Result<(), CompletionStackError> {
        let requested = self
            .entries
            .len()
            .checked_add(additional)
            .and_then(|requested| u32::try_from(requested).ok())
            .ok_or(CompletionStackError::Limit {
                limit: self.limit,
                requested: u32::MAX,
            })?;
        if requested > self.limit {
            return Err(CompletionStackError::Limit {
                limit: self.limit,
                requested,
            });
        }
        if additional > self.entries.capacity() - self.entries.len() {
            self.entries
                .try_reserve_exact(additional)
                .map_err(|_| CompletionStackError::AllocationFailed)?;
        }
        Ok(())
    }

    #[inline]
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "M6 finally lowering publishes language completions"
        )
    )]
    pub(crate) fn push_record(
        &mut self,
        record: CompletionRecord,
    ) -> Result<(), CompletionStackError> {
        self.reserve(1)?;
        self.entries.push(CompletionEntry::Language(record));
        Ok(())
    }

    #[inline]
    pub(crate) fn push_native(
        &mut self,
        continuation: NativeContinuation,
    ) -> Result<(), CompletionStackError> {
        self.reserve(1)?;
        self.entries.push(CompletionEntry::Native(continuation));
        Ok(())
    }

    /// Pops a native trampoline only when it is the top entry, preserving language state.
    #[inline]
    pub(crate) fn pop_native(&mut self) -> Option<NativeContinuation> {
        match self.entries.last() {
            Some(CompletionEntry::Native(_)) => match self.entries.pop() {
                Some(CompletionEntry::Native(continuation)) => Some(continuation),
                _ => unreachable!("the inspected completion entry remains on top"),
            },
            Some(CompletionEntry::Language(_)) | None => None,
        }
    }

    /// Inspects the top native trampoline without changing its rooted lifetime.
    #[inline]
    pub(crate) fn last_native(&self) -> Option<NativeContinuation> {
        match self.entries.last() {
            Some(CompletionEntry::Native(continuation)) => Some(*continuation),
            Some(CompletionEntry::Language(_)) | None => None,
        }
    }

    /// Drops abandoned callback trampolines without crossing a frame or language checkpoint.
    #[inline]
    pub(crate) fn discard_native_suffix(&mut self, frame_completion_base: u32) {
        while self.entries.len() > frame_completion_base as usize
            && matches!(self.entries.last(), Some(CompletionEntry::Native(_)))
        {
            self.entries.pop();
        }
    }

    /// Restores the newest saved language completion without crossing its frame checkpoint.
    #[inline]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "M6 finally replay restores language completions")
    )]
    pub(crate) fn restore_record(
        &mut self,
        frame_completion_base: u32,
    ) -> Option<CompletionRecord> {
        if self.entries.len() <= frame_completion_base as usize {
            return None;
        }
        match self.entries.last() {
            Some(CompletionEntry::Language(_)) => match self.entries.pop() {
                Some(CompletionEntry::Language(record)) => Some(record),
                _ => unreachable!("the inspected completion entry remains on top"),
            },
            Some(CompletionEntry::Native(_)) | None => None,
        }
    }

    #[inline]
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "M6 debugger and finally tests inspect saved completions"
        )
    )]
    pub(crate) fn record(&self, index: usize) -> Option<&CompletionRecord> {
        match self.entries.get(index) {
            Some(CompletionEntry::Language(record)) => Some(record),
            Some(CompletionEntry::Native(_)) | None => None,
        }
    }

    #[inline(always)]
    pub(crate) const fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline(always)]
    #[cfg(test)]
    pub(crate) const fn capacity(&self) -> usize {
        self.entries.capacity()
    }

    #[inline]
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    #[inline]
    pub(crate) fn truncate(&mut self, len: usize) {
        self.entries.truncate(len);
    }
}

impl Trace for CompletionStack {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.entries.trace(tracer);
    }
}

#[cfg(test)]
mod tests {
    use tachyon_value::Immediate;

    use super::*;
    use crate::{
        ConversionCallbackStage, ConversionConsumer, ConversionContinuation,
        NativeContinuationKind, NativeContinuationSite, PropertyCallbackMode, ToPrimitiveStage,
        runtime::fiber::NativeContinuation,
    };

    const fn undefined() -> Value {
        Value::from_immediate(Immediate::Undefined)
    }

    fn native_continuation() -> NativeContinuation {
        NativeContinuation::conversion(ConversionContinuation {
            site: NativeContinuationSite {
                caller_base: 0,
                destination: 0,
                call_site: WordOffset::new(0),
            },
            consumer: ConversionConsumer::ToNumber,
            receiver: undefined(),
            object: undefined(),
            stage: ToPrimitiveStage::ValueOf,
            callback_stage: ConversionCallbackStage::MethodCall,
        })
    }

    #[test]
    /// Locks each public constructor to the ECMAScript value/target invariant it represents.
    fn constructors_enforce_value_and_target_shapes() {
        let value = Value::from_i32(7);
        let target = WordOffset::new(11);
        let records = [
            CompletionRecord::normal(None),
            CompletionRecord::throw(value),
            CompletionRecord::return_value(value),
            CompletionRecord::break_target(Some(value), target),
            CompletionRecord::continue_target(None, target),
        ];
        assert_eq!(records[0].kind(), CompletionKind::Normal);
        assert_eq!(records[0].value(), None);
        assert_eq!(records[0].target(), None);
        assert_eq!(records[1].value(), Some(value));
        assert_eq!(records[2].value(), Some(value));
        assert_eq!(records[3].target(), Some(target));
        assert_eq!(records[4].target(), Some(target));
    }

    #[test]
    fn checked_capacity_reports_exact_requested_depth() {
        let mut stack = CompletionStack::new(1);
        stack.push_record(CompletionRecord::normal(None)).unwrap();
        assert_eq!(
            stack.push_record(CompletionRecord::return_value(undefined())),
            Err(CompletionStackError::Limit {
                limit: 1,
                requested: 2,
            })
        );
    }

    #[test]
    /// Locks compact native constructors and conversion reconstruction to their two traced slots.
    fn native_continuation_constructors_preserve_typed_state() {
        let site = NativeContinuationSite {
            caller_base: 1,
            destination: 2,
            call_site: WordOffset::new(3),
        };
        let receiver = Value::from_i32(7);
        let assigned = Value::from_i32(9);
        let get =
            NativeContinuation::property_get(site, PropertyCallbackMode::Descriptor, receiver);
        assert_eq!(
            get.kind(),
            NativeContinuationKind::PropertyGet(PropertyCallbackMode::Descriptor)
        );
        assert_eq!(get.first(), receiver);
        assert_eq!(get.second(), undefined());

        let set = NativeContinuation::property_set(site, receiver, assigned);
        assert_eq!(
            set.kind(),
            NativeContinuationKind::PropertySet(crate::PropertyWriteMode::Assignment)
        );
        assert_eq!(set.first(), receiver);
        assert_eq!(set.second(), assigned);
        let call_root = NativeContinuation::conversion_call_root(site, receiver, assigned);
        assert_eq!(call_root.kind(), NativeContinuationKind::ConversionCallRoot);
        assert_eq!(call_root.first(), receiver);
        assert_eq!(call_root.second(), assigned);
        let conversion = native_continuation().as_conversion().unwrap();
        assert_eq!(conversion.site.call_site, WordOffset::new(0));
        assert_eq!(conversion.consumer, ConversionConsumer::ToNumber);
    }

    #[test]
    /// Proves restoration never consumes another frame or a callback trampoline by accident.
    fn restore_respects_frame_base_and_native_top_entries() {
        let mut stack = CompletionStack::new(3);
        stack.push_record(CompletionRecord::normal(None)).unwrap();
        stack
            .push_record(CompletionRecord::throw(Value::from_i32(9)))
            .unwrap();
        assert_eq!(stack.restore_record(2), None);
        assert_eq!(
            stack.restore_record(1).map(CompletionRecord::kind),
            Some(CompletionKind::Throw)
        );
        stack.push_native(native_continuation()).unwrap();
        assert_eq!(stack.restore_record(0), None);
        assert!(stack.pop_native().is_some());
        assert_eq!(stack.len(), 1);
    }
}
