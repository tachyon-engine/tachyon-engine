//! Resumable `String.fromCharCode` and `String.fromCodePoint` construction.

use core::mem::size_of;

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StringCodeKind {
    CharCode,
    CodePoint,
}

/// GC-owned input and fixed-capacity UTF-16 builder retained across observable conversions.
#[derive(Debug)]
pub(crate) struct PendingStringFromCodes {
    arguments: Vec<Value>,
    output: Vec<u16>,
    cursor: usize,
    kind: StringCodeKind,
}

impl Trace for PendingStringFromCodes {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.arguments.trace(tracer);
    }
}

impl GcExternalMemory for PendingStringFromCodes {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.arguments
            .capacity()
            .saturating_mul(size_of::<Value>())
            .saturating_add(self.output.capacity().saturating_mul(size_of::<u16>()))
    }
}

impl Isolate {
    /// Starts a left-to-right `String.fromCharCode` conversion with a numeric fast path.
    pub(crate) fn begin_string_from_char_code(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_string_from_codes(site, StringCodeKind::CharCode)
    }

    /// Starts a left-to-right `String.fromCodePoint` conversion with a numeric fast path.
    pub(crate) fn begin_string_from_code_point(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_string_from_codes(site, StringCodeKind::CodePoint)
    }

    /// Restores the pending builder before converting the primitive callback result.
    pub(crate) fn resume_string_from_codes_conversion(
        &mut self,
        site: NativeContinuationSite,
        state_value: Value,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        self.write(site.caller_base, site.destination, state_value)?;
        let number = self.string_code_to_number(primitive)?;
        let state_value = self.read(site.caller_base, site.destination)?;
        let state = self.pending_string_from_codes_reference(state_value)?;
        self.append_string_code(state, number)?;
        self.advance_string_from_codes(site, state)
    }

    /// Uses the mature-engine shape: one-number fast path, otherwise one fixed builder.
    fn begin_string_from_codes(
        &mut self,
        site: &CallSite,
        kind: StringCodeKind,
    ) -> Result<(), ExecutionError> {
        if site.argument_count == 1 {
            let argument = self
                .call_argument(site, 0)?
                .expect("the single argument is present");
            if numeric_value(argument).is_some() {
                let number = self.string_code_to_number(argument)?;
                let mut output = Vec::new();
                output
                    .try_reserve_exact(if kind == StringCodeKind::CharCode {
                        1
                    } else {
                        2
                    })
                    .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                append_string_code_units(kind, number, &mut output)?;
                let result = self.allocate_runtime_string(
                    JsString::try_from_owned_code_units(output)
                        .map_err(ExecutionError::PropertyKeyString)?,
                )?;
                return self.write(site.caller_base, site.destination, result);
            }
        }

        let count = usize::try_from(site.argument_count)
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(site.argument_count))?;
        let output_capacity = match kind {
            StringCodeKind::CharCode => count,
            StringCodeKind::CodePoint => count
                .checked_mul(2)
                .filter(|length| *length <= u32::MAX as usize)
                .ok_or(ExecutionError::InvalidStringLength)?,
        };
        let mut arguments = Vec::new();
        arguments
            .try_reserve_exact(count)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for index in 0..site.argument_count {
            arguments.push(
                self.call_argument(site, index)?
                    .expect("argument count bounds the String constructor window"),
            );
        }
        let mut output = Vec::new();
        output
            .try_reserve_exact(output_capacity)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        let state = self.allocate_string_from_codes_state(PendingStringFromCodes {
            arguments,
            output,
            cursor: 0,
            kind,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.advance_string_from_codes(continuation_site, state)
    }

    /// Advances without growing the Rust stack and suspends only for object ToPrimitive.
    fn advance_string_from_codes(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingStringFromCodes>,
    ) -> Result<(), ExecutionError> {
        loop {
            let Some(value) = self.string_from_codes_cursor_value(state)? else {
                let output = self.take_string_from_codes_output(state)?;
                let result = self.allocate_runtime_string(
                    JsString::try_from_owned_code_units(output)
                        .map_err(ExecutionError::PropertyKeyString)?,
                )?;
                return self.write(site.caller_base, site.destination, result);
            };
            if self.is_object_value(value) {
                return self.dispatch_object_primitive_conversion(
                    ConversionConsumer::StringFromCodesElement,
                    site.caller_base,
                    site.destination,
                    Value::from_heap_ref(state.raw()),
                    value,
                    site.call_site,
                );
            }
            let number = self.string_code_to_number(value)?;
            self.append_string_code(state, number)?;
        }
    }

    #[inline(always)]
    fn string_code_to_number(&mut self, value: Value) -> Result<f64, ExecutionError> {
        if self.is_bigint_value(value) {
            return Err(ExecutionError::NotObject(value));
        }
        numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))
    }

    /// Appends one converted argument within the preallocated output bound.
    fn append_string_code(
        &mut self,
        state: GcRef<PendingStringFromCodes>,
        number: f64,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_string_from_codes)
                    .map_err(ExecutionError::NoGcBorrow)?;
                append_string_code_units(pending.kind, number, &mut pending.output)?;
                pending.cursor += 1;
                Ok(())
            })
        })
    }

    fn string_from_codes_cursor_value(
        &mut self,
        state: GcRef<PendingStringFromCodes>,
    ) -> Result<Option<Value>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_string_from_codes)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(pending.arguments.get(pending.cursor).copied())
            })
        })
    }

    fn take_string_from_codes_output(
        &mut self,
        state: GcRef<PendingStringFromCodes>,
    ) -> Result<Vec<u16>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_string_from_codes)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(core::mem::take(&mut pending.output))
            })
        })
    }

    fn allocate_string_from_codes_state(
        &mut self,
        pending: PendingStringFromCodes,
    ) -> Result<GcRef<PendingStringFromCodes>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            inactive_realms: &mut self.inactive_realms,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_string_from_codes,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    fn pending_string_from_codes_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingStringFromCodes>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_string_from_codes)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }
}

#[inline(always)]
/// Encodes one already-converted Number without permitting builder growth past its bound.
fn append_string_code_units(
    kind: StringCodeKind,
    number: f64,
    output: &mut Vec<u16>,
) -> Result<(), ExecutionError> {
    match kind {
        StringCodeKind::CharCode => {
            debug_assert!(output.len() < output.capacity());
            let unit = if !number.is_finite() || number == 0.0 {
                0
            } else {
                number.trunc().rem_euclid(65_536.0) as u16
            };
            output.push(unit);
        }
        StringCodeKind::CodePoint => {
            if !number.is_finite()
                || number.fract() != 0.0
                || !(0.0..=0x10_ffff as f64).contains(&number)
            {
                return Err(ExecutionError::InvalidStringLength);
            }
            let code_point = number as u32;
            if code_point <= 0xffff {
                debug_assert!(output.len() < output.capacity());
                output.push(code_point as u16);
            } else {
                debug_assert!(output.len().saturating_add(2) <= output.capacity());
                let astral = code_point - 0x1_0000;
                output.push(0xd800 | ((astral >> 10) as u16));
                output.push(0xdc00 | ((astral & 0x3ff) as u16));
            }
        }
    }
    Ok(())
}
