//! Resumable ECMA-402 option processing for `Intl.DateTimeFormat` construction.

use super::*;
use crate::runtime::fiber::IntlDateTimeFormatStage;

const UNDEFINED: Value = Value::from_immediate(Immediate::Undefined);

/// GC-managed constructor state retained across option getters and primitive conversions.
#[derive(Clone, Debug)]
pub(crate) struct PendingIntlDateTimeFormat {
    new_target: Value,
    legacy_receiver: Value,
    options: Value,
    locales: Value,
    calendar: Value,
    numbering_system: Value,
    time_zone: Value,
    locale_matcher: IntlLocaleMatcher,
    hour_cycle: Option<IntlDateTimeHourCycle>,
    hour12: Option<bool>,
    fields: IntlDateTimeFormatOptions,
    stage: IntlDateTimeFormatStage,
}

impl Trace for PendingIntlDateTimeFormat {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.new_target.trace(tracer);
        self.legacy_receiver.trace(tracer);
        self.options.trace(tracer);
        self.locales.trace(tracer);
        self.calendar.trace(tracer);
        self.numbering_system.trace(tracer);
        self.time_zone.trace(tracer);
    }
}

struct PendingIntlDateTimeFormatRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingIntlDateTimeFormat,
}

impl Trace for PendingIntlDateTimeFormatRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl PendingIntlDateTimeFormat {
    fn new(new_target: Value, options: Value, locales: Value) -> Self {
        Self {
            new_target,
            legacy_receiver: UNDEFINED,
            options,
            locales,
            calendar: UNDEFINED,
            numbering_system: UNDEFINED,
            time_zone: UNDEFINED,
            locale_matcher: IntlLocaleMatcher::BestFit,
            hour_cycle: None,
            hour12: None,
            fields: IntlDateTimeFormatOptions::default(),
            stage: IntlDateTimeFormatStage::LocaleMatcher,
        }
    }
}

impl Isolate {
    /// Canonicalizes locales once, roots them in an internal array, then begins ordered option Get.
    pub(super) fn start_intl_date_time_format_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let locales = self.call_argument(site, 0)?.unwrap_or(UNDEFINED);
        let options = self.call_argument(site, 1)?.unwrap_or(UNDEFINED);
        let called_without_new = !self.is_object_value(site.new_target);
        let new_target = if called_without_new {
            site.callee
        } else {
            site.new_target
        };
        let locales = self.intl_date_time_locale_list(locales)?;
        let locales = self.materialize_intl_date_time_locale_list(locales)?;
        let options = if options == UNDEFINED {
            UNDEFINED
        } else {
            self.coerce_to_object(options)?
        };
        let mut pending = PendingIntlDateTimeFormat::new(new_target, options, locales);
        if called_without_new {
            pending.legacy_receiver = site.this_value;
        }
        let state = self.allocate_pending_intl_date_time_format(pending)?;
        if options == UNDEFINED {
            return self.finish_pending_intl_date_time_format(Self::native_site(site), state);
        }
        self.dispatch_intl_date_time_format_option_get(
            Self::native_site(site),
            state,
            IntlDateTimeFormatStage::LocaleMatcher,
        )
    }

    /// Resumes one option Get, preserving undefined and selecting the exact coercion hint.
    pub(crate) fn resume_pending_intl_date_time_format(
        &mut self,
        continuation: NativeContinuation,
        stage: IntlDateTimeFormatStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.pending_intl_date_time_format_reference(continuation.first())?;
        if value == UNDEFINED {
            return self.advance_intl_date_time_format_option(continuation.site(), state, stage);
        }
        if stage == IntlDateTimeFormatStage::Hour12 {
            let hour12 = self.is_truthy_value(value)?;
            self.update_pending_intl_date_time_format(state, |pending| {
                pending.hour12 = Some(hour12);
            })?;
            return self.advance_intl_date_time_format_option(continuation.site(), state, stage);
        }
        if self.is_object_value(value) {
            let consumer = if stage == IntlDateTimeFormatStage::FractionalSecondDigits {
                ConversionConsumer::IntlDateTimeFormatNumberOption
            } else {
                ConversionConsumer::IntlDateTimeFormatStringOption
            };
            return self.dispatch_object_primitive_conversion(
                consumer,
                continuation.site().caller_base,
                continuation.site().destination,
                Value::from_heap_ref(state.raw()),
                value,
                continuation.site().call_site,
            );
        }
        self.resume_intl_date_time_format_option_primitive(continuation.site(), state, value)
    }

    /// Continues after an option object's number- or string-hint ToPrimitive callback.
    pub(crate) fn resume_intl_date_time_format_option_primitive(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlDateTimeFormat>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let stage = self.pending_intl_date_time_format_stage(state)?;
        if stage == IntlDateTimeFormatStage::FractionalSecondDigits {
            let number = numeric_value(self.convert_to_number(primitive)?)
                .ok_or(ExecutionError::InvalidIntlDateTimeFormatOption)?;
            if !number.is_finite() || !(1.0..=3.0).contains(&number) {
                return Err(ExecutionError::InvalidIntlDateTimeFormatOption);
            }
            self.update_pending_intl_date_time_format(state, |pending| {
                pending.fields.fractional_second_digits = Some(number.floor() as u8);
            })?;
            return self.advance_intl_date_time_format_option(site, state, stage);
        }
        let string = self.primitive_to_string_value(primitive)?;
        self.store_intl_date_time_format_string_option(site, state, stage, string)
    }

    /// Parses one already-stringified option and stores only provider-neutral scalar state.
    fn store_intl_date_time_format_string_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlDateTimeFormat>,
        stage: IntlDateTimeFormatStage,
        string: Value,
    ) -> Result<(), ExecutionError> {
        let text = self
            .intl_ascii_string(string)
            .map_err(|_| ExecutionError::InvalidIntlDateTimeFormatOption)?;
        match stage {
            IntlDateTimeFormatStage::LocaleMatcher => {
                let matcher = match text.as_ref() {
                    "best fit" => IntlLocaleMatcher::BestFit,
                    "lookup" => IntlLocaleMatcher::Lookup,
                    _ => return Err(ExecutionError::InvalidIntlDateTimeFormatOption),
                };
                self.update_pending_intl_date_time_format(state, |pending| {
                    pending.locale_matcher = matcher;
                })?;
            }
            IntlDateTimeFormatStage::Calendar | IntlDateTimeFormatStage::NumberingSystem => {
                if !is_date_time_unicode_locale_type(&text) {
                    return Err(ExecutionError::InvalidIntlDateTimeFormatOption);
                }
                self.set_pending_intl_date_time_format_value(state, stage, string)?;
            }
            IntlDateTimeFormatStage::HourCycle => {
                let hour_cycle = parse_date_time_hour_cycle(&text)?;
                self.update_pending_intl_date_time_format(state, |pending| {
                    pending.hour_cycle = Some(hour_cycle);
                })?;
            }
            IntlDateTimeFormatStage::TimeZone => {
                self.set_pending_intl_date_time_format_value(state, stage, string)?;
            }
            IntlDateTimeFormatStage::Weekday | IntlDateTimeFormatStage::Era => {
                let value = parse_date_time_text_style(&text)?;
                self.update_pending_intl_date_time_format(state, |pending| match stage {
                    IntlDateTimeFormatStage::Weekday => pending.fields.weekday = Some(value),
                    IntlDateTimeFormatStage::Era => pending.fields.era = Some(value),
                    _ => unreachable!(),
                })?;
            }
            IntlDateTimeFormatStage::Year
            | IntlDateTimeFormatStage::Day
            | IntlDateTimeFormatStage::Hour
            | IntlDateTimeFormatStage::Minute
            | IntlDateTimeFormatStage::Second => {
                let value = parse_date_time_numeric_style(&text)?;
                self.update_pending_intl_date_time_format(state, |pending| match stage {
                    IntlDateTimeFormatStage::Year => pending.fields.year = Some(value),
                    IntlDateTimeFormatStage::Day => pending.fields.day = Some(value),
                    IntlDateTimeFormatStage::Hour => pending.fields.hour = Some(value),
                    IntlDateTimeFormatStage::Minute => pending.fields.minute = Some(value),
                    IntlDateTimeFormatStage::Second => pending.fields.second = Some(value),
                    _ => unreachable!(),
                })?;
            }
            IntlDateTimeFormatStage::Month => {
                let value = parse_date_time_month_style(&text)?;
                self.update_pending_intl_date_time_format(state, |pending| {
                    pending.fields.month = Some(value);
                })?;
            }
            IntlDateTimeFormatStage::DayPeriod => {
                let value = parse_date_time_text_style(&text)?;
                self.update_pending_intl_date_time_format(state, |pending| {
                    pending.fields.day_period = Some(value);
                })?;
            }
            IntlDateTimeFormatStage::TimeZoneName => {
                let value = parse_date_time_zone_name_style(&text)?;
                self.update_pending_intl_date_time_format(state, |pending| {
                    pending.fields.time_zone_name = Some(value);
                })?;
            }
            IntlDateTimeFormatStage::FormatMatcher => match text.as_ref() {
                "basic" | "best fit" => {}
                _ => return Err(ExecutionError::InvalidIntlDateTimeFormatOption),
            },
            IntlDateTimeFormatStage::DateStyle | IntlDateTimeFormatStage::TimeStyle => {
                let value = parse_date_time_style(&text)?;
                self.update_pending_intl_date_time_format(state, |pending| match stage {
                    IntlDateTimeFormatStage::DateStyle => pending.fields.date_style = Some(value),
                    IntlDateTimeFormatStage::TimeStyle => pending.fields.time_style = Some(value),
                    _ => unreachable!(),
                })?;
            }
            IntlDateTimeFormatStage::Hour12 | IntlDateTimeFormatStage::FractionalSecondDigits => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
        }
        self.advance_intl_date_time_format_option(site, state, stage)
    }

    /// Advances through the specification's observable option order without recursive Rust calls.
    fn advance_intl_date_time_format_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlDateTimeFormat>,
        stage: IntlDateTimeFormatStage,
    ) -> Result<(), ExecutionError> {
        let Some(next) = next_intl_date_time_format_stage(stage) else {
            return self.finish_pending_intl_date_time_format(site, state);
        };
        self.dispatch_intl_date_time_format_option_get(site, state, next)
    }

    /// Performs one Proxy/accessor-aware Get under a DateTimeFormat-specific continuation.
    fn dispatch_intl_date_time_format_option_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlDateTimeFormat>,
        stage: IntlDateTimeFormatStage,
    ) -> Result<(), ExecutionError> {
        self.set_pending_intl_date_time_format_stage(state, stage)?;
        let snapshot = self.pending_intl_date_time_format_snapshot(state)?;
        let key = self
            .intern_intrinsic_name(intl_date_time_format_option_name(stage))?
            .into();
        let continuation = NativeContinuation::intl_date_time_format(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            snapshot.options,
        );
        match self.resolve_property_read_until_proxy(snapshot.options, key)? {
            PropertyReadResolution::Read(PropertyRead::Missing) => {
                self.resume_pending_intl_date_time_format(continuation, stage, UNDEFINED)
            }
            PropertyReadResolution::Read(PropertyRead::Data(value)) => {
                self.resume_pending_intl_date_time_format(continuation, stage, value)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) if getter == UNDEFINED => {
                self.resume_pending_intl_date_time_format(continuation, stage, UNDEFINED)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => self
                .dispatch_property_callback(
                    NativeContinuation::intl_date_time_format_property_get(
                        site,
                        Value::from_heap_ref(state.raw()),
                        snapshot.options,
                    ),
                    getter,
                )
                .map(|_| ()),
            PropertyReadResolution::Proxy(_) => {
                self.dispatch_intl_date_time_format_nested(continuation, |isolate| {
                    isolate
                        .dispatch_proxy_aware_property_read(
                            site,
                            snapshot.options,
                            snapshot.options,
                            key,
                        )
                        .map(|_| ())
                })
            }
        }
    }

    /// Builds the provider request after every observable option boundary has completed.
    fn finish_pending_intl_date_time_format(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlDateTimeFormat>,
    ) -> Result<(), ExecutionError> {
        let mut snapshot = self.pending_intl_date_time_format_snapshot(state)?;
        if (snapshot.fields.date_style.is_some() || snapshot.fields.time_style.is_some())
            && has_date_time_components(&snapshot.fields)
        {
            return Err(ExecutionError::IntlDateTimeFormatStyleConflict);
        }
        if !has_date_time_components(&snapshot.fields)
            && snapshot.fields.date_style.is_none()
            && snapshot.fields.time_style.is_none()
        {
            snapshot.fields.year = Some(IntlDateTimeNumericStyle::Numeric);
            snapshot.fields.month = Some(IntlDateTimeMonthStyle::Numeric);
            snapshot.fields.day = Some(IntlDateTimeNumericStyle::Numeric);
        }
        let request = IntlDateTimeFormatRequest {
            locales: self.intl_date_time_format_locale_strings(snapshot.locales)?,
            locale_matcher: snapshot.locale_matcher,
            calendar: self.optional_intl_date_time_format_string(snapshot.calendar)?,
            numbering_system: self
                .optional_intl_date_time_format_string(snapshot.numbering_system)?,
            hour_cycle: snapshot.hour_cycle,
            hour12: snapshot.hour12,
            time_zone: self
                .optional_intl_date_time_format_string(snapshot.time_zone)?
                .unwrap_or_default(),
            options: snapshot.fields,
        };
        self.finish_intl_date_time_format_constructor(
            site,
            snapshot.new_target,
            snapshot.legacy_receiver,
            request,
        )
    }

    /// Stores canonical locale strings in a private intrinsic Array rooted across option callbacks.
    fn materialize_intl_date_time_locale_list(
        &mut self,
        locales: Box<[Box<str>]>,
    ) -> Result<Value, ExecutionError> {
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.DateTimeFormat"),
        )?;
        for (index, locale) in locales.into_vec().into_iter().enumerate() {
            let (locale, retained) = self.allocate_runtime_string_retaining(
                JsString::try_from_str(&locale).map_err(ExecutionError::PropertyKeyString)?,
                result,
            )?;
            debug_assert_eq!(retained, result);
            let index = u32::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
            let key = self.property_key_atom(safe_integer_value(u64::from(index)))?;
            self.set_own_data_property(result, key, locale)?;
        }
        Ok(result)
    }

    fn intl_date_time_format_locale_strings(
        &mut self,
        locales: Value,
    ) -> Result<Box<[Box<str>]>, ExecutionError> {
        let values = self.copy_packed_intl_array(locales)?;
        let mut strings = Vec::new();
        strings
            .try_reserve_exact(values.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for value in values {
            strings.push(
                self.intl_ascii_string(value)
                    .map_err(|_| ExecutionError::InvalidLanguageTag)?,
            );
        }
        Ok(strings.into_boxed_slice())
    }

    fn optional_intl_date_time_format_string(
        &mut self,
        value: Value,
    ) -> Result<Option<Box<str>>, ExecutionError> {
        (value != UNDEFINED)
            .then(|| {
                self.intl_ascii_string(value)
                    .map_err(|_| ExecutionError::InvalidIntlDateTimeFormatOption)
            })
            .transpose()
    }

    /// Allocates the traced pending record while retaining every managed constructor input.
    fn allocate_pending_intl_date_time_format(
        &mut self,
        pending: PendingIntlDateTimeFormat,
    ) -> Result<GcRef<PendingIntlDateTimeFormat>, ExecutionError> {
        let mut roots = PendingIntlDateTimeFormatRoots {
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
            pending,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_intl_date_time_format,
                0,
                0,
                roots.pending.clone(),
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_intl_date_time_format_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingIntlDateTimeFormat>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_intl_date_time_format)
            .map_err(ExecutionError::HeapReference)
    }

    fn pending_intl_date_time_format_snapshot(
        &mut self,
        state: GcRef<PendingIntlDateTimeFormat>,
    ) -> Result<PendingIntlDateTimeFormat, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_intl_date_time_format)
                    .cloned()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(crate) fn pending_intl_date_time_format_stage(
        &mut self,
        state: GcRef<PendingIntlDateTimeFormat>,
    ) -> Result<IntlDateTimeFormatStage, ExecutionError> {
        self.pending_intl_date_time_format_snapshot(state)
            .map(|pending| pending.stage)
    }

    fn update_pending_intl_date_time_format(
        &mut self,
        state: GcRef<PendingIntlDateTimeFormat>,
        update: impl FnOnce(&mut PendingIntlDateTimeFormat),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                update(
                    no_gc
                        .borrow_mut(state, self.types.pending_intl_date_time_format)
                        .map_err(ExecutionError::NoGcBorrow)?,
                );
                Ok(())
            })
        })
    }

    fn set_pending_intl_date_time_format_value(
        &mut self,
        state: GcRef<PendingIntlDateTimeFormat>,
        stage: IntlDateTimeFormatStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_intl_date_time_format)
                    .map_err(ExecutionError::NoGcBorrow)?;
                match stage {
                    IntlDateTimeFormatStage::Calendar => pending.calendar = value,
                    IntlDateTimeFormatStage::NumberingSystem => pending.numbering_system = value,
                    IntlDateTimeFormatStage::TimeZone => pending.time_zone = value,
                    _ => return Err(ExecutionError::MissingNativeContinuation),
                }
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    fn set_pending_intl_date_time_format_stage(
        &mut self,
        state: GcRef<PendingIntlDateTimeFormat>,
        stage: IntlDateTimeFormatStage,
    ) -> Result<(), ExecutionError> {
        self.update_pending_intl_date_time_format(state, |pending| pending.stage = stage)
    }

    /// Drains synchronous nested Proxy reads without allowing recursive Rust continuation growth.
    fn dispatch_intl_date_time_format_nested(
        &mut self,
        continuation: NativeContinuation,
        operation: impl FnOnce(&mut Self) -> Result<(), ExecutionError>,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = operation(self) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(
            continuation.site().caller_base,
            continuation.site().destination,
        )?;
        let NativeContinuationKind::IntlDateTimeFormat(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_pending_intl_date_time_format(continuation, stage, value)
    }
}

#[inline(always)]
const fn next_intl_date_time_format_stage(
    stage: IntlDateTimeFormatStage,
) -> Option<IntlDateTimeFormatStage> {
    use IntlDateTimeFormatStage as S;
    Some(match stage {
        S::LocaleMatcher => S::Calendar,
        S::Calendar => S::NumberingSystem,
        S::NumberingSystem => S::Hour12,
        S::Hour12 => S::HourCycle,
        S::HourCycle => S::TimeZone,
        S::TimeZone => S::Weekday,
        S::Weekday => S::Era,
        S::Era => S::Year,
        S::Year => S::Month,
        S::Month => S::Day,
        S::Day => S::DayPeriod,
        S::DayPeriod => S::Hour,
        S::Hour => S::Minute,
        S::Minute => S::Second,
        S::Second => S::FractionalSecondDigits,
        S::FractionalSecondDigits => S::TimeZoneName,
        S::TimeZoneName => S::FormatMatcher,
        S::FormatMatcher => S::DateStyle,
        S::DateStyle => S::TimeStyle,
        S::TimeStyle => return None,
    })
}

#[inline(always)]
const fn intl_date_time_format_option_name(stage: IntlDateTimeFormatStage) -> &'static [u8] {
    use IntlDateTimeFormatStage as S;
    match stage {
        S::LocaleMatcher => b"localeMatcher",
        S::Calendar => b"calendar",
        S::NumberingSystem => b"numberingSystem",
        S::Hour12 => b"hour12",
        S::HourCycle => b"hourCycle",
        S::TimeZone => b"timeZone",
        S::Weekday => b"weekday",
        S::Era => b"era",
        S::Year => b"year",
        S::Month => b"month",
        S::Day => b"day",
        S::DayPeriod => b"dayPeriod",
        S::Hour => b"hour",
        S::Minute => b"minute",
        S::Second => b"second",
        S::FractionalSecondDigits => b"fractionalSecondDigits",
        S::TimeZoneName => b"timeZoneName",
        S::FormatMatcher => b"formatMatcher",
        S::DateStyle => b"dateStyle",
        S::TimeStyle => b"timeStyle",
    }
}
