//! Provider-backed `Intl.DateTimeFormat` branded surface and initial option pipeline.

use super::super::*;
use crate::runtime::fiber::IntlDateTimeFormatLegacyStage;

mod options;
pub(crate) use options::PendingIntlDateTimeFormat;
mod legacy;

const MAX_TIME_VALUE: f64 = 8.64e15;

struct IntlDateTimeFormatBoundRoots<'a> {
    vm: VmRoots<'a>,
    target: Value,
    date_time_format: Value,
    name: Value,
    data: Option<GcRef<BoundFunctionData>>,
}

struct IntlDateTimeFormatValueRoots<'a> {
    vm: VmRoots<'a>,
    state: NativeCallState,
}

impl Trace for IntlDateTimeFormatBoundRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.target.trace(tracer);
        self.date_time_format.trace(tracer);
        self.name.trace(tracer);
        self.data.trace(tracer);
    }
}

impl Trace for IntlDateTimeFormatValueRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.state.trace(tracer);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlDateTimeFormatOutput {
    String,
    Parts,
}

impl Isolate {
    /// Constructs a real provider-backed formatter from synchronously observable scalar options.
    pub(crate) fn begin_intl_date_time_format_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.start_intl_date_time_format_constructor(site)
    }

    /// Allocates the branded formatter after all observable option processing has completed.
    fn finish_intl_date_time_format_constructor(
        &mut self,
        site: NativeContinuationSite,
        new_target: Value,
        legacy_receiver: Value,
        mut request: IntlDateTimeFormatRequest,
    ) -> Result<(), ExecutionError> {
        let time_zone = match request.time_zone.take() {
            Some(time_zone) => time_zone,
            None => self
                .host_providers
                .time_zone_mut()
                .ok_or(ExecutionError::MissingTimeZoneProvider)?
                .default_time_zone_identifier()
                .map_err(ExecutionError::TimeZoneProvider)?,
        };
        request.time_zone = Some(self.canonicalize_intl_time_zone(&time_zone)?);
        let creation = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .create_date_time_format(request)
            .map_err(ExecutionError::IntlProvider)?;
        let prototype_atom = self.prototype_atom()?;
        let default_prototype = self
            .realm
            .intl_date_time_format_prototype
            .expect("Intl.DateTimeFormat prototype initializes before construction");
        let prototype = self
            .constructor_prototype_value(new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .or_else(|| {
                self.realm_for_callable(new_target).ok().and_then(|realm| {
                    self.realm_intrinsic_prototype(
                        realm,
                        IntrinsicPrototypeKind::IntlDateTimeFormat,
                    )
                })
            })
            .unwrap_or(default_prototype);
        let formatter = self.allocate_intl_date_time_format_object(
            creation,
            prototype,
            AllocationSpace::Young,
        )?;
        if legacy_receiver.as_immediate() == Some(Immediate::Undefined)
            || !self.is_object_value(legacy_receiver)
        {
            return self.write(site.caller_base, site.destination, formatter);
        }
        self.begin_intl_date_time_format_chain(site, legacy_receiver, formatter)
    }

    /// Filters the synchronously canonicalized locale list through provider capability data.
    pub(crate) fn call_intl_date_time_format_supported_locales_of(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let locales_argument = self.call_argument(site, 0)?.unwrap_or(undefined);
        let options_argument = self.call_argument(site, 1)?.unwrap_or(undefined);
        let locales = self.intl_date_time_locale_list(locales_argument)?;
        let matcher = self.intl_date_time_locale_matcher(options_argument)?;
        let supported = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .date_time_format_supported_locales(&locales, matcher)
            .map_err(ExecutionError::IntlProvider)?;
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.DateTimeFormat"),
        )?;
        self.write(site.caller_base, site.destination, result)?;
        for (index, locale) in supported.into_vec().into_iter().enumerate() {
            let result = self.read(site.caller_base, site.destination)?;
            let (locale, result) = self.allocate_runtime_string_retaining(
                JsString::try_from_str(&locale).map_err(ExecutionError::PropertyKeyString)?,
                result,
            )?;
            self.write(site.caller_base, site.destination, result)?;
            let index = u32::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
            let key = self.property_key_atom(safe_integer_value(u64::from(index)))?;
            self.set_own_data_property(result, key, locale)?;
        }
        Ok(())
    }

    /// Returns one cached anonymous bound formatter after enforcing the receiver brand.
    pub(crate) fn call_intl_date_time_format_format_getter(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        if self
            .intl_date_time_format_reference_if_branded(site.this_value)
            .is_none()
        {
            return self.begin_intl_date_time_format_unwrap(
                Self::native_site(site),
                IntlDateTimeFormatLegacyStage::FormatHasInstance,
                site.this_value,
            );
        }
        self.finish_intl_date_time_format_format_getter(Self::native_site(site), site.this_value)
    }

    /// Returns or creates the cached bound formatter for an already-unwrapped receiver.
    fn finish_intl_date_time_format_format_getter(
        &mut self,
        site: NativeContinuationSite,
        formatter_value: Value,
    ) -> Result<(), ExecutionError> {
        let formatter = self.intl_date_time_format_reference(formatter_value)?;
        let snapshot = self.intl_date_time_format_snapshot(formatter)?;
        if snapshot.cached_bound_format.as_immediate() != Some(Immediate::Undefined) {
            return self.write(
                site.caller_base,
                site.destination,
                snapshot.cached_bound_format,
            );
        }
        let format = self.allocate_intl_date_time_format_bound(formatter_value)?;
        self.set_intl_date_time_format_bound(formatter, format)?;
        self.write(site.caller_base, site.destination, format)
    }

    /// Formats one TimeClip-compatible argument through the cached provider backend.
    pub(crate) fn call_intl_date_time_format_format(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.call_intl_date_time_format_value(site, IntlDateTimeFormatOutput::String)
    }

    /// Formats one argument and materializes typed provider spans as fresh part records.
    pub(crate) fn call_intl_date_time_format_format_to_parts(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.call_intl_date_time_format_value(site, IntlDateTimeFormatOutput::Parts)
    }

    /// Converts a primitive date argument and keeps provider/GC borrows in disjoint phases.
    fn call_intl_date_time_format_value(
        &mut self,
        site: &CallSite,
        output: IntlDateTimeFormatOutput,
    ) -> Result<(), ExecutionError> {
        self.intl_date_time_format_reference(site.this_value)?;
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let milliseconds = if value.as_immediate() == Some(Immediate::Undefined) {
            self.host_providers
                .wall_clock_mut()
                .ok_or(ExecutionError::MissingWallClockProvider)?
                .unix_time_milliseconds()
                .map_err(ExecutionError::WallClockProvider)?
        } else {
            if self.is_object_value(value) {
                let state = self.allocate_intl_date_time_format_value_state(NativeCallState {
                    values: [
                        site.this_value,
                        boolean_value(output == IntlDateTimeFormatOutput::Parts),
                        Value::from_immediate(Immediate::Undefined),
                        Value::from_immediate(Immediate::Undefined),
                        Value::from_immediate(Immediate::Undefined),
                    ],
                    count: 0,
                })?;
                return self.dispatch_object_primitive_conversion(
                    ConversionConsumer::IntlDateTimeFormatValue,
                    site.caller_base,
                    site.destination,
                    Value::from_heap_ref(state.raw()),
                    value,
                    site.call_site,
                );
            }
            return self.finish_intl_date_time_format_primitive(
                Self::native_site(site),
                site.this_value,
                value,
                output,
            );
        };
        self.finish_intl_date_time_format_milliseconds(
            Self::native_site(site),
            site.this_value,
            milliseconds,
            output,
        )
    }

    /// Continues single-value formatting after observable number-hint ToPrimitive.
    pub(crate) fn resume_intl_date_time_format_value_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let output = if snapshot.values[1].as_immediate() == Some(Immediate::True) {
            IntlDateTimeFormatOutput::Parts
        } else {
            IntlDateTimeFormatOutput::String
        };
        self.finish_intl_date_time_format_primitive(site, snapshot.values[0], primitive, output)
    }

    /// Converts one primitive to a clipped finite millisecond value before provider access.
    fn finish_intl_date_time_format_primitive(
        &mut self,
        site: NativeContinuationSite,
        formatter_value: Value,
        primitive: Value,
        output: IntlDateTimeFormatOutput,
    ) -> Result<(), ExecutionError> {
        let number = numeric_value(self.convert_to_number(primitive)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(primitive))?;
        let milliseconds = time_clip_to_i64(number)?;
        self.finish_intl_date_time_format_milliseconds(site, formatter_value, milliseconds, output)
    }

    /// Performs host offset lookup and formatting after all JavaScript conversion has finished.
    fn finish_intl_date_time_format_milliseconds(
        &mut self,
        site: NativeContinuationSite,
        formatter_value: Value,
        milliseconds: i64,
        output: IntlDateTimeFormatOutput,
    ) -> Result<(), ExecutionError> {
        let formatter = self.intl_date_time_format_reference(formatter_value)?;
        let payload = self.intl_date_time_format_snapshot(formatter)?.payload;
        let time_zone = self.intl_date_time_format_resolved(formatter)?.time_zone;
        let offset = self.intl_time_zone_offset_milliseconds(&time_zone, milliseconds)?;
        let input = IntlDateTimeInput {
            utc_milliseconds: milliseconds,
            offset_milliseconds: offset,
        };
        match output {
            IntlDateTimeFormatOutput::String => {
                let formatted = self.format_intl_date_time_payload(payload, input)?;
                let value = self.allocate_runtime_string(
                    JsString::try_from_utf16(&formatted)
                        .map_err(ExecutionError::PropertyKeyString)?,
                )?;
                self.write(site.caller_base, site.destination, value)
            }
            IntlDateTimeFormatOutput::Parts => {
                let parts = self.format_intl_date_time_parts_payload(payload, input)?;
                self.materialize_intl_date_time_parts(site, parts)
            }
        }
    }

    /// Resolves fixed offset identifiers in-engine and delegates named zones to the embedder.
    pub(super) fn intl_time_zone_offset_milliseconds(
        &mut self,
        time_zone: &str,
        utc_milliseconds: i64,
    ) -> Result<i64, ExecutionError> {
        if let Some(minutes) = crate::parse_offset_time_zone_minutes(time_zone) {
            return Ok(i64::from(minutes) * 60_000);
        }
        self.host_providers
            .time_zone_mut()
            .ok_or(ExecutionError::MissingTimeZoneProvider)?
            .offset_milliseconds_for_utc_in_zone(time_zone, utc_milliseconds)
            .map_err(ExecutionError::TimeZoneProvider)
    }

    /// Allocates fixed traced state before a single formatting argument invokes JavaScript.
    fn allocate_intl_date_time_format_value_state(
        &mut self,
        state: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = IntlDateTimeFormatValueRoots {
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

    /// Publishes a fresh resolved-options record in the ECMA-402 property order.
    pub(crate) fn call_intl_date_time_format_resolved_options(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        if self
            .intl_date_time_format_reference_if_branded(site.this_value)
            .is_none()
        {
            return self.begin_intl_date_time_format_unwrap(
                Self::native_site(site),
                IntlDateTimeFormatLegacyStage::ResolvedOptionsHasInstance,
                site.this_value,
            );
        }
        self.finish_intl_date_time_format_resolved_options(Self::native_site(site), site.this_value)
    }

    /// Publishes resolved options for an already-unwrapped DateTimeFormat receiver.
    fn finish_intl_date_time_format_resolved_options(
        &mut self,
        site: NativeContinuationSite,
        formatter_value: Value,
    ) -> Result<(), ExecutionError> {
        let formatter = self.intl_date_time_format_reference(formatter_value)?;
        let resolved = self.intl_date_time_format_resolved(formatter)?;
        let result = self.create_ordinary_object()?;
        self.write(site.caller_base, site.destination, result)?;
        self.set_intl_date_time_string(result, b"locale", &resolved.locale)?;
        self.set_intl_date_time_string(result, b"calendar", &resolved.calendar)?;
        self.set_intl_date_time_string(result, b"numberingSystem", &resolved.numbering_system)?;
        self.set_intl_date_time_string(result, b"timeZone", &resolved.time_zone)?;
        if let Some(hour_cycle) = resolved.hour_cycle {
            self.set_intl_date_time_string(
                result,
                b"hourCycle",
                date_time_hour_cycle_name(hour_cycle),
            )?;
            let key = self.intern_intrinsic_name(b"hour12")?;
            self.set_own_data_property(
                result,
                key,
                boolean_value(matches!(
                    hour_cycle,
                    IntlDateTimeHourCycle::H11 | IntlDateTimeHourCycle::H12
                )),
            )?;
        }
        self.append_intl_date_time_resolved_components(result, &resolved.options)
    }

    /// Materializes one provider-owned parts buffer without observable Array mutation methods.
    fn materialize_intl_date_time_parts(
        &mut self,
        site: NativeContinuationSite,
        parts: IntlFormattedDateTimeParts,
    ) -> Result<(), ExecutionError> {
        validate_intl_date_time_parts(&parts)?;
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.DateTimeFormat"),
        )?;
        self.write(site.caller_base, site.destination, result)?;
        let type_key = self.intern_intrinsic_name(b"type")?;
        let value_key = self.intern_intrinsic_name(b"value")?;
        for (index, span) in parts.spans.iter().copied().enumerate() {
            let result = self.read(site.caller_base, site.destination)?;
            let part = self.create_ordinary_object()?;
            let index = u32::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
            let property = self.property_key_atom(safe_integer_value(u64::from(index)))?;
            self.set_own_data_property(result, property, part)?;
            let (kind, part) = self.allocate_runtime_string_retaining(
                JsString::try_from_latin1(date_time_part_name(span.kind))
                    .map_err(ExecutionError::PropertyKeyString)?,
                part,
            )?;
            self.set_own_data_property(part, type_key, kind)?;
            let result = self.read(site.caller_base, site.destination)?;
            let part = self
                .get_data_property(result, property)?
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            let start = usize::try_from(span.start)
                .map_err(|_| ExecutionError::IntlProvider(HostProviderError::Failure(3)))?;
            let end = usize::try_from(span.end)
                .map_err(|_| ExecutionError::IntlProvider(HostProviderError::Failure(3)))?;
            let units = parts
                .formatted
                .get(start..end)
                .ok_or(ExecutionError::IntlProvider(HostProviderError::Failure(3)))?;
            let (value, part) = self.allocate_runtime_string_retaining(
                JsString::try_from_utf16(units).map_err(ExecutionError::PropertyKeyString)?,
                part,
            )?;
            self.set_own_data_property(part, value_key, value)?;
        }
        Ok(())
    }

    /// Converts the currently supported locale-list shapes without retaining managed handles.
    fn intl_date_time_locale_list(
        &mut self,
        locales: Value,
    ) -> Result<Box<[Box<str>]>, ExecutionError> {
        if locales.as_immediate() == Some(Immediate::Undefined) {
            return Ok(Box::new([]));
        }
        if self.is_string_value(locales) {
            return Ok(vec![self.canonicalize_intl_locale_text(locales)?].into_boxed_slice());
        }
        let values = self.copy_packed_intl_array(locales)?;
        let mut canonical = Vec::new();
        canonical
            .try_reserve_exact(values.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for value in values {
            if !self.is_string_value(value) {
                return Err(ExecutionError::InvalidLocaleListElement(value));
            }
            let locale = self.canonicalize_intl_locale_text(value)?;
            if !canonical.iter().any(|existing| existing == &locale) {
                canonical.push(locale);
            }
        }
        Ok(canonical.into_boxed_slice())
    }

    fn intl_date_time_locale_matcher(
        &mut self,
        options: Value,
    ) -> Result<IntlLocaleMatcher, ExecutionError> {
        let options = if options.as_immediate() == Some(Immediate::Undefined) {
            None
        } else {
            Some(self.coerce_to_object(options)?)
        };
        match self
            .intl_date_time_optional_string(options, b"localeMatcher")?
            .as_deref()
        {
            None | Some("best fit") => Ok(IntlLocaleMatcher::BestFit),
            Some("lookup") => Ok(IntlLocaleMatcher::Lookup),
            Some(_) => Err(ExecutionError::InvalidIntlDateTimeFormatOption),
        }
    }

    fn intl_date_time_optional_string(
        &mut self,
        options: Option<Value>,
        name: &[u8],
    ) -> Result<Option<Box<str>>, ExecutionError> {
        let Some(options) = options else {
            return Ok(None);
        };
        let key = self.intern_intrinsic_name(name)?;
        let value = self
            .get_data_property(options, key)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if value.as_immediate() == Some(Immediate::Undefined) {
            return Ok(None);
        }
        if self.is_object_value(value) {
            return Err(ExecutionError::InvalidIntlDateTimeFormatOption);
        }
        let value = self.primitive_to_string_value(value)?;
        self.intl_ascii_string(value)
            .map(Some)
            .map_err(|_| ExecutionError::InvalidIntlDateTimeFormatOption)
    }

    fn canonicalize_intl_time_zone(&mut self, time_zone: &str) -> Result<Box<str>, ExecutionError> {
        self.host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .canonicalize_time_zone(time_zone)
            .map_err(ExecutionError::IntlProvider)?
            .ok_or(ExecutionError::InvalidIntlDateTimeFormatOption)
    }

    fn intl_date_time_format_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<IntlDateTimeFormatObject>, ExecutionError> {
        self.intl_date_time_format_reference_if_branded(value)
            .ok_or(ExecutionError::IncompatibleIntlDateTimeFormatReceiver(
                value,
            ))
    }

    /// Returns the internal brand only for a direct DateTimeFormat object.
    fn intl_date_time_format_reference_if_branded(
        &self,
        value: Value,
    ) -> Option<GcRef<IntlDateTimeFormatObject>> {
        let raw = value.as_heap_ref()?;
        self.heap
            .checked_reference(raw, self.types.intl_date_time_format_object)
            .ok()
    }

    fn intl_date_time_format_snapshot(
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

    fn intl_date_time_format_resolved(
        &mut self,
        formatter: GcRef<IntlDateTimeFormatObject>,
    ) -> Result<IntlDateTimeFormatResolved, ExecutionError> {
        let payload = self.intl_date_time_format_snapshot(formatter)?.payload;
        self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(payload, self.types.intl_date_time_format_payload)
                    .map(|payload| payload.resolved.clone())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn format_intl_date_time_payload(
        &mut self,
        payload: GcRef<IntlDateTimeFormatPayload>,
        input: IntlDateTimeInput,
    ) -> Result<Box<[u16]>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(payload, self.types.intl_date_time_format_payload)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .backend
                    .format(input)
                    .map_err(ExecutionError::IntlProvider)
            })
        })
    }

    fn format_intl_date_time_parts_payload(
        &mut self,
        payload: GcRef<IntlDateTimeFormatPayload>,
        input: IntlDateTimeInput,
    ) -> Result<IntlFormattedDateTimeParts, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(payload, self.types.intl_date_time_format_payload)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .backend
                    .format_to_parts(input)
                    .map_err(ExecutionError::IntlProvider)
            })
        })
    }

    fn set_intl_date_time_format_bound(
        &mut self,
        formatter: GcRef<IntlDateTimeFormatObject>,
        format: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let formatter = scope.root(formatter).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(formatter, self.types.intl_date_time_format_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .cached_bound_format = format;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(formatter, format)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Allocates an anonymous bound function with length one and no bound arguments.
    fn allocate_intl_date_time_format_bound(
        &mut self,
        date_time_format: Value,
    ) -> Result<Value, ExecutionError> {
        let target = self
            .realm
            .intl_date_time_format_format
            .expect("Intl.DateTimeFormat format target initializes before access");
        let name = self.allocate_runtime_string(
            JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?,
        )?;
        let realm = self.realm_for_callable(target)?;
        let prototype = self.resolve_function_object(target)?.ordinary.prototype;
        let mut roots = IntlDateTimeFormatBoundRoots {
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
            target,
            date_time_format,
            name,
            data: None,
        };
        let data = self
            .heap
            .try_allocate_external_with_gc(
                self.types.bound_function,
                0,
                BoundFunctionData {
                    bound_target: target,
                    call_target: target,
                    bound_this: date_time_format,
                    arguments: Box::new([]),
                    length: Value::from_i32(1),
                    name,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.data = Some(data);
        self.heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::Bound(data),
                    realm,
                    prototype_or_home_object: FunctionAuxiliaryEdge::NONE,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|function| Value::from_heap_ref(function.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    fn set_intl_date_time_string(
        &mut self,
        result: Value,
        key: &[u8],
        value: &str,
    ) -> Result<(), ExecutionError> {
        let (value, result) = self.allocate_runtime_string_retaining(
            JsString::try_from_str(value).map_err(ExecutionError::PropertyKeyString)?,
            result,
        )?;
        let key = self.intern_intrinsic_name(key)?;
        self.set_own_data_property(result, key, value)
    }

    /// Appends every resolved component in the specification's observable property order.
    fn append_intl_date_time_resolved_components(
        &mut self,
        result: Value,
        options: &IntlDateTimeFormatOptions,
    ) -> Result<(), ExecutionError> {
        if let Some(value) = options.weekday {
            self.set_intl_date_time_string(result, b"weekday", date_time_text_style_name(value))?;
        }
        if let Some(value) = options.era {
            self.set_intl_date_time_string(result, b"era", date_time_text_style_name(value))?;
        }
        if let Some(value) = options.year {
            self.set_intl_date_time_string(result, b"year", date_time_numeric_style_name(value))?;
        }
        if let Some(value) = options.month {
            self.set_intl_date_time_string(result, b"month", date_time_month_style_name(value))?;
        }
        if let Some(value) = options.day {
            self.set_intl_date_time_string(result, b"day", date_time_numeric_style_name(value))?;
        }
        if let Some(value) = options.day_period {
            self.set_intl_date_time_string(result, b"dayPeriod", date_time_text_style_name(value))?;
        }
        if let Some(value) = options.hour {
            self.set_intl_date_time_string(result, b"hour", date_time_numeric_style_name(value))?;
        }
        if let Some(value) = options.minute {
            self.set_intl_date_time_string(result, b"minute", date_time_numeric_style_name(value))?;
        }
        if let Some(value) = options.second {
            self.set_intl_date_time_string(result, b"second", date_time_numeric_style_name(value))?;
        }
        if let Some(value) = options.fractional_second_digits {
            let key = self.intern_intrinsic_name(b"fractionalSecondDigits")?;
            self.set_own_data_property(result, key, Value::from_i32(i32::from(value)))?;
        }
        if let Some(value) = options.time_zone_name {
            self.set_intl_date_time_string(
                result,
                b"timeZoneName",
                date_time_zone_name_style_name(value),
            )?;
        }
        if let Some(value) = options.date_style {
            self.set_intl_date_time_string(result, b"dateStyle", date_time_style_name(value))?;
        }
        if let Some(value) = options.time_style {
            self.set_intl_date_time_string(result, b"timeStyle", date_time_style_name(value))?;
        }
        Ok(())
    }
}

fn validate_intl_date_time_parts(parts: &IntlFormattedDateTimeParts) -> Result<(), ExecutionError> {
    let mut cursor = 0_u32;
    for span in &parts.spans {
        if span.start != cursor || span.end <= span.start {
            return Err(ExecutionError::IntlProvider(HostProviderError::Failure(3)));
        }
        cursor = span.end;
    }
    let length = u32::try_from(parts.formatted.len())
        .map_err(|_| ExecutionError::IntlProvider(HostProviderError::Failure(3)))?;
    if cursor != length || (length != 0 && parts.spans.is_empty()) {
        return Err(ExecutionError::IntlProvider(HostProviderError::Failure(3)));
    }
    Ok(())
}

#[inline(always)]
fn time_clip_to_i64(value: f64) -> Result<i64, ExecutionError> {
    if !value.is_finite() || value.abs() > MAX_TIME_VALUE {
        Err(ExecutionError::InvalidDateValue)
    } else {
        Ok(value.trunc() as i64)
    }
}

#[inline(always)]
fn has_date_time_components(options: &IntlDateTimeFormatOptions) -> bool {
    options.weekday.is_some()
        || options.era.is_some()
        || options.year.is_some()
        || options.month.is_some()
        || options.day.is_some()
        || options.day_period.is_some()
        || options.hour.is_some()
        || options.minute.is_some()
        || options.second.is_some()
        || options.fractional_second_digits.is_some()
        || options.time_zone_name.is_some()
}

fn parse_date_time_text_style(value: &str) -> Result<IntlDateTimeTextStyle, ExecutionError> {
    match value {
        "long" => Ok(IntlDateTimeTextStyle::Long),
        "short" => Ok(IntlDateTimeTextStyle::Short),
        "narrow" => Ok(IntlDateTimeTextStyle::Narrow),
        _ => Err(ExecutionError::InvalidIntlDateTimeFormatOption),
    }
}

/// Validates the Unicode locale type grammar used by calendar and numberingSystem.
fn is_date_time_unicode_locale_type(value: &str) -> bool {
    let mut saw_subtag = false;
    for subtag in value.split('-') {
        if !(3..=8).contains(&subtag.len())
            || !subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return false;
        }
        saw_subtag = true;
    }
    saw_subtag
}

fn parse_date_time_numeric_style(value: &str) -> Result<IntlDateTimeNumericStyle, ExecutionError> {
    match value {
        "numeric" => Ok(IntlDateTimeNumericStyle::Numeric),
        "2-digit" => Ok(IntlDateTimeNumericStyle::TwoDigit),
        _ => Err(ExecutionError::InvalidIntlDateTimeFormatOption),
    }
}

fn parse_date_time_month_style(value: &str) -> Result<IntlDateTimeMonthStyle, ExecutionError> {
    match value {
        "numeric" => Ok(IntlDateTimeMonthStyle::Numeric),
        "2-digit" => Ok(IntlDateTimeMonthStyle::TwoDigit),
        "long" => Ok(IntlDateTimeMonthStyle::Long),
        "short" => Ok(IntlDateTimeMonthStyle::Short),
        "narrow" => Ok(IntlDateTimeMonthStyle::Narrow),
        _ => Err(ExecutionError::InvalidIntlDateTimeFormatOption),
    }
}

fn parse_date_time_style(value: &str) -> Result<IntlDateTimeStyle, ExecutionError> {
    match value {
        "full" => Ok(IntlDateTimeStyle::Full),
        "long" => Ok(IntlDateTimeStyle::Long),
        "medium" => Ok(IntlDateTimeStyle::Medium),
        "short" => Ok(IntlDateTimeStyle::Short),
        _ => Err(ExecutionError::InvalidIntlDateTimeFormatOption),
    }
}

fn parse_date_time_hour_cycle(value: &str) -> Result<IntlDateTimeHourCycle, ExecutionError> {
    match value {
        "h11" => Ok(IntlDateTimeHourCycle::H11),
        "h12" => Ok(IntlDateTimeHourCycle::H12),
        "h23" => Ok(IntlDateTimeHourCycle::H23),
        "h24" => Ok(IntlDateTimeHourCycle::H24),
        _ => Err(ExecutionError::InvalidIntlDateTimeFormatOption),
    }
}

fn parse_date_time_zone_name_style(
    value: &str,
) -> Result<IntlDateTimeZoneNameStyle, ExecutionError> {
    match value {
        "long" => Ok(IntlDateTimeZoneNameStyle::Long),
        "short" => Ok(IntlDateTimeZoneNameStyle::Short),
        "shortOffset" => Ok(IntlDateTimeZoneNameStyle::ShortOffset),
        "longOffset" => Ok(IntlDateTimeZoneNameStyle::LongOffset),
        "shortGeneric" => Ok(IntlDateTimeZoneNameStyle::ShortGeneric),
        "longGeneric" => Ok(IntlDateTimeZoneNameStyle::LongGeneric),
        _ => Err(ExecutionError::InvalidIntlDateTimeFormatOption),
    }
}

#[inline(always)]
const fn date_time_text_style_name(value: IntlDateTimeTextStyle) -> &'static str {
    match value {
        IntlDateTimeTextStyle::Long => "long",
        IntlDateTimeTextStyle::Short => "short",
        IntlDateTimeTextStyle::Narrow => "narrow",
    }
}

#[inline(always)]
const fn date_time_numeric_style_name(value: IntlDateTimeNumericStyle) -> &'static str {
    match value {
        IntlDateTimeNumericStyle::Numeric => "numeric",
        IntlDateTimeNumericStyle::TwoDigit => "2-digit",
    }
}

#[inline(always)]
const fn date_time_month_style_name(value: IntlDateTimeMonthStyle) -> &'static str {
    match value {
        IntlDateTimeMonthStyle::Numeric => "numeric",
        IntlDateTimeMonthStyle::TwoDigit => "2-digit",
        IntlDateTimeMonthStyle::Long => "long",
        IntlDateTimeMonthStyle::Short => "short",
        IntlDateTimeMonthStyle::Narrow => "narrow",
    }
}

#[inline(always)]
const fn date_time_style_name(value: IntlDateTimeStyle) -> &'static str {
    match value {
        IntlDateTimeStyle::Full => "full",
        IntlDateTimeStyle::Long => "long",
        IntlDateTimeStyle::Medium => "medium",
        IntlDateTimeStyle::Short => "short",
    }
}

#[inline(always)]
const fn date_time_hour_cycle_name(value: IntlDateTimeHourCycle) -> &'static str {
    match value {
        IntlDateTimeHourCycle::H11 => "h11",
        IntlDateTimeHourCycle::H12 => "h12",
        IntlDateTimeHourCycle::H23 => "h23",
        IntlDateTimeHourCycle::H24 => "h24",
    }
}

#[inline(always)]
const fn date_time_zone_name_style_name(value: IntlDateTimeZoneNameStyle) -> &'static str {
    match value {
        IntlDateTimeZoneNameStyle::Long => "long",
        IntlDateTimeZoneNameStyle::Short => "short",
        IntlDateTimeZoneNameStyle::ShortOffset => "shortOffset",
        IntlDateTimeZoneNameStyle::LongOffset => "longOffset",
        IntlDateTimeZoneNameStyle::ShortGeneric => "shortGeneric",
        IntlDateTimeZoneNameStyle::LongGeneric => "longGeneric",
    }
}

#[inline(always)]
const fn date_time_part_name(kind: IntlDateTimePartType) -> &'static [u8] {
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
