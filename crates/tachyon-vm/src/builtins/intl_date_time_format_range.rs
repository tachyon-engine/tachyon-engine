//! Provider-backed `Intl.DateTimeFormat` interval formatting surface.

use super::super::*;

const MAX_TIME_VALUE: f64 = 8.64e15;

struct IntlDateTimeRangeRoots<'a> {
    vm: VmRoots<'a>,
    state: NativeCallState,
}

impl Trace for IntlDateTimeRangeRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.state.trace(tracer);
    }
}

#[derive(Clone, Copy)]
enum IntlDateTimeRangeOutput {
    String,
    Parts,
}

impl Isolate {
    /// Formats two required TimeClip-compatible arguments through the provider interval backend.
    pub(crate) fn call_intl_date_time_format_range(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.call_intl_date_time_format_range_value(site, IntlDateTimeRangeOutput::String)
    }

    /// Materializes the provider's typed interval fields as fresh ECMA-402 part records.
    pub(crate) fn call_intl_date_time_format_range_to_parts(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.call_intl_date_time_format_range_value(site, IntlDateTimeRangeOutput::Parts)
    }

    /// Separates observable input conversion, host offset lookup, provider calls, and GC allocation.
    fn call_intl_date_time_format_range_value(
        &mut self,
        site: &CallSite,
        output: IntlDateTimeRangeOutput,
    ) -> Result<(), ExecutionError> {
        self.intl_date_time_range_reference(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let start = self.call_argument(site, 0)?.unwrap_or(undefined);
        let end = self.call_argument(site, 1)?.unwrap_or(undefined);
        if start == undefined || end == undefined {
            return Err(ExecutionError::UnsupportedNumberConversion(undefined));
        }
        let continuation_site = Self::native_site(site);
        if self.is_object_value(start) {
            let state = self.allocate_intl_date_time_range_state(NativeCallState {
                values: [
                    site.this_value,
                    end,
                    undefined,
                    boolean_value(matches!(output, IntlDateTimeRangeOutput::Parts)),
                    undefined,
                ],
                count: 0,
            })?;
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlDateTimeFormatRangeStart,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                start,
                site.call_site,
            );
        }
        let start = self.intl_date_time_range_milliseconds(start)?;
        self.continue_intl_date_time_format_range(
            continuation_site,
            site.this_value,
            end,
            start,
            output,
        )
    }

    /// Continues after the start argument's observable ToPrimitive conversion.
    pub(crate) fn resume_intl_date_time_format_range_start(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let start = self.intl_date_time_range_milliseconds(primitive)?;
        self.continue_intl_date_time_format_range(
            site,
            snapshot.values[0],
            snapshot.values[1],
            start,
            date_time_range_output(snapshot.values[3]),
        )
    }

    /// Completes the operation after the end argument's observable ToPrimitive conversion.
    pub(crate) fn resume_intl_date_time_format_range_end(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let start =
            numeric_value(snapshot.values[2]).ok_or(ExecutionError::MissingNativeContinuation)?;
        let end = self.intl_date_time_range_milliseconds(primitive)?;
        self.finish_intl_date_time_format_range(
            site,
            snapshot.values[0],
            start as i64,
            end,
            date_time_range_output(snapshot.values[3]),
        )
    }

    /// Either dispatches the end conversion or proceeds with two finite clipped millisecond values.
    fn continue_intl_date_time_format_range(
        &mut self,
        site: NativeContinuationSite,
        formatter_value: Value,
        end: Value,
        start: i64,
        output: IntlDateTimeRangeOutput,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(end) {
            let undefined = Value::from_immediate(Immediate::Undefined);
            let state = self.allocate_intl_date_time_range_state(NativeCallState {
                values: [
                    formatter_value,
                    end,
                    Value::from_f64(start as f64),
                    boolean_value(matches!(output, IntlDateTimeRangeOutput::Parts)),
                    undefined,
                ],
                count: 1,
            })?;
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlDateTimeFormatRangeEnd,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                end,
                site.call_site,
            );
        }
        let end = self.intl_date_time_range_milliseconds(end)?;
        self.finish_intl_date_time_format_range(site, formatter_value, start, end, output)
    }

    /// Resolves host offsets and delegates only after both ECMAScript conversions have completed.
    fn finish_intl_date_time_format_range(
        &mut self,
        site: NativeContinuationSite,
        formatter_value: Value,
        start: i64,
        end: i64,
        output: IntlDateTimeRangeOutput,
    ) -> Result<(), ExecutionError> {
        let formatter = self.intl_date_time_range_reference(formatter_value)?;
        let snapshot = self.intl_date_time_range_snapshot(formatter)?;
        let (time_zone, payload) = self.intl_date_time_range_payload(snapshot.payload)?;
        let start = self.intl_date_time_range_input(&time_zone, start)?;
        let end = self.intl_date_time_range_input(&time_zone, end)?;
        match output {
            IntlDateTimeRangeOutput::String => {
                let formatted = self.format_intl_date_time_range_payload(payload, start, end)?;
                let value = self.allocate_runtime_string(
                    JsString::try_from_utf16(&formatted)
                        .map_err(ExecutionError::PropertyKeyString)?,
                )?;
                self.write(site.caller_base, site.destination, value)
            }
            IntlDateTimeRangeOutput::Parts => {
                let parts = self.format_intl_date_time_range_parts_payload(payload, start, end)?;
                self.materialize_intl_date_time_range_parts(site, parts)
            }
        }
    }

    /// Converts supported primitive and branded Date inputs without retaining managed handles.
    fn intl_date_time_range_milliseconds(&mut self, value: Value) -> Result<i64, ExecutionError> {
        debug_assert!(!self.is_object_value(value));
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        if !number.is_finite() || number.abs() > MAX_TIME_VALUE {
            Err(ExecutionError::InvalidDateValue)
        } else {
            Ok(number.trunc() as i64)
        }
    }

    /// Allocates the fixed traced state used across either endpoint's JavaScript callbacks.
    fn allocate_intl_date_time_range_state(
        &mut self,
        state: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = IntlDateTimeRangeRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                inactive_realms: &mut self.inactive_realms,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            state,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                roots.state,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    fn intl_date_time_range_input(
        &mut self,
        time_zone: &str,
        utc_milliseconds: i64,
    ) -> Result<IntlDateTimeInput, ExecutionError> {
        let offset_milliseconds =
            self.intl_time_zone_offset_milliseconds(time_zone, utc_milliseconds)?;
        Ok(IntlDateTimeInput {
            utc_milliseconds,
            offset_milliseconds,
        })
    }

    fn intl_date_time_range_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<IntlDateTimeFormatObject>, ExecutionError> {
        let raw =
            value
                .as_heap_ref()
                .ok_or(ExecutionError::IncompatibleIntlDateTimeFormatReceiver(
                    value,
                ))?;
        self.heap
            .checked_reference(raw, self.types.intl_date_time_format_object)
            .map_err(|_| ExecutionError::IncompatibleIntlDateTimeFormatReceiver(value))
    }

    fn intl_date_time_range_snapshot(
        &mut self,
        formatter: GcRef<IntlDateTimeFormatObject>,
    ) -> Result<IntlDateTimeFormatObject, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let formatter = scope.root(formatter).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(formatter, self.types.intl_date_time_format_object)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn intl_date_time_range_payload(
        &mut self,
        payload: GcRef<IntlDateTimeFormatPayload>,
    ) -> Result<(Box<str>, GcRef<IntlDateTimeFormatPayload>), ExecutionError> {
        let time_zone = self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(payload, self.types.intl_date_time_format_payload)
                    .map(|payload| payload.resolved.time_zone.clone())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        Ok((time_zone, payload))
    }

    fn format_intl_date_time_range_payload(
        &mut self,
        payload: GcRef<IntlDateTimeFormatPayload>,
        start: IntlDateTimeInput,
        end: IntlDateTimeInput,
    ) -> Result<Box<[u16]>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(payload, self.types.intl_date_time_format_payload)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .backend
                    .format_range(start, end)
                    .map_err(ExecutionError::IntlProvider)
            })
        })
    }

    fn format_intl_date_time_range_parts_payload(
        &mut self,
        payload: GcRef<IntlDateTimeFormatPayload>,
        start: IntlDateTimeInput,
        end: IntlDateTimeInput,
    ) -> Result<IntlFormattedDateTimeRangeParts, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(payload, self.types.intl_date_time_format_payload)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .backend
                    .format_range_to_parts(start, end)
                    .map_err(ExecutionError::IntlProvider)
            })
        })
    }

    /// Publishes type/value/source records while rooting the result array through each allocation.
    fn materialize_intl_date_time_range_parts(
        &mut self,
        site: NativeContinuationSite,
        parts: IntlFormattedDateTimeRangeParts,
    ) -> Result<(), ExecutionError> {
        validate_intl_date_time_range_parts(&parts)?;
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.DateTimeFormat"),
        )?;
        self.write(site.caller_base, site.destination, result)?;
        let type_key = self.intern_intrinsic_name(b"type")?;
        let value_key = self.intern_intrinsic_name(b"value")?;
        let source_key = self.intern_intrinsic_name(b"source")?;
        for (index, span) in parts.spans.iter().copied().enumerate() {
            let result = self.read(site.caller_base, site.destination)?;
            let part = self.create_ordinary_object()?;
            let index = u32::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
            let property = self.property_key_atom(safe_integer_value(u64::from(index)))?;
            self.set_own_data_property(result, property, part)?;
            self.set_intl_date_time_range_part_string(
                part,
                type_key,
                date_time_range_part_name(span.kind),
            )?;
            let result = self.read(site.caller_base, site.destination)?;
            let part = self
                .get_data_property(result, property)?
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            let start = usize::try_from(span.start).map_err(|_| range_data_failure())?;
            let end = usize::try_from(span.end).map_err(|_| range_data_failure())?;
            let units = parts
                .formatted
                .get(start..end)
                .ok_or_else(range_data_failure)?;
            let (value, part) = self.allocate_runtime_string_retaining(
                JsString::try_from_utf16(units).map_err(ExecutionError::PropertyKeyString)?,
                part,
            )?;
            self.set_own_data_property(part, value_key, value)?;
            let result = self.read(site.caller_base, site.destination)?;
            let part = self
                .get_data_property(result, property)?
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            self.set_intl_date_time_range_part_string(
                part,
                source_key,
                date_time_range_source_name(span.source),
            )?;
        }
        Ok(())
    }

    fn set_intl_date_time_range_part_string(
        &mut self,
        part: Value,
        key: AtomId,
        text: &[u8],
    ) -> Result<(), ExecutionError> {
        let (value, part) = self.allocate_runtime_string_retaining(
            JsString::try_from_latin1(text).map_err(ExecutionError::PropertyKeyString)?,
            part,
        )?;
        self.set_own_data_property(part, key, value)
    }
}

/// Rejects malformed provider spans before any partially materialized array escapes.
fn validate_intl_date_time_range_parts(
    parts: &IntlFormattedDateTimeRangeParts,
) -> Result<(), ExecutionError> {
    let mut cursor = 0_u32;
    for span in &parts.spans {
        if span.start != cursor || span.end <= span.start {
            return Err(range_data_failure());
        }
        cursor = span.end;
    }
    let length = u32::try_from(parts.formatted.len()).map_err(|_| range_data_failure())?;
    if cursor != length || (length != 0 && parts.spans.is_empty()) {
        return Err(range_data_failure());
    }
    Ok(())
}

#[inline(always)]
fn range_data_failure() -> ExecutionError {
    ExecutionError::IntlProvider(HostProviderError::Failure(3))
}

#[inline(always)]
const fn date_time_range_source_name(source: IntlDateTimeRangeSource) -> &'static [u8] {
    match source {
        IntlDateTimeRangeSource::StartRange => b"startRange",
        IntlDateTimeRangeSource::EndRange => b"endRange",
        IntlDateTimeRangeSource::Shared => b"shared",
    }
}

#[inline(always)]
fn date_time_range_output(value: Value) -> IntlDateTimeRangeOutput {
    if value.as_immediate() == Some(Immediate::True) {
        IntlDateTimeRangeOutput::Parts
    } else {
        IntlDateTimeRangeOutput::String
    }
}

#[inline(always)]
const fn date_time_range_part_name(kind: IntlDateTimePartType) -> &'static [u8] {
    match kind {
        IntlDateTimePartType::Literal => b"literal",
        IntlDateTimePartType::Era => b"era",
        IntlDateTimePartType::Year => b"year",
        IntlDateTimePartType::RelatedYear => b"relatedYear",
        IntlDateTimePartType::YearName => b"yearName",
        IntlDateTimePartType::Month => b"month",
        IntlDateTimePartType::Day => b"day",
        IntlDateTimePartType::Weekday => b"weekday",
        IntlDateTimePartType::DayPeriod => b"dayPeriod",
        IntlDateTimePartType::Hour => b"hour",
        IntlDateTimePartType::Minute => b"minute",
        IntlDateTimePartType::Second => b"second",
        IntlDateTimePartType::FractionalSecond => b"fractionalSecond",
        IntlDateTimePartType::TimeZoneName => b"timeZoneName",
        IntlDateTimePartType::Unknown => b"unknown",
    }
}
