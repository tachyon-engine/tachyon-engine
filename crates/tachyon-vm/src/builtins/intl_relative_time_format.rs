//! Provider-backed `Intl.RelativeTimeFormat` construction and formatting.

use super::super::*;
use crate::runtime::fiber::IntlRelativeTimeFormatStage;

mod state;

pub(crate) use state::PendingIntlRelativeTimeFormat;

const UNDEFINED: Value = Value::from_immediate(Immediate::Undefined);

impl Isolate {
    /// Starts construction after enforcing the new-only RelativeTimeFormat contract.
    pub(crate) fn begin_intl_relative_time_format_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(site.new_target) {
            return Err(ExecutionError::NonConstructor(site.callee));
        }
        self.begin_intl_relative_time_format_options(site, site.new_target, false)
    }

    /// Starts locale filtering while observing only the localeMatcher option.
    pub(crate) fn begin_intl_relative_time_format_supported_locales_of(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_intl_relative_time_format_options(site, UNDEFINED, true)
    }

    /// Allocates traced state and nests CanonicalizeLocaleList below its typed continuation.
    fn begin_intl_relative_time_format_options(
        &mut self,
        site: &CallSite,
        new_target: Value,
        supported_locales: bool,
    ) -> Result<(), ExecutionError> {
        let locales = self.call_argument(site, 0)?.unwrap_or(UNDEFINED);
        let options = self.call_argument(site, 1)?.unwrap_or(UNDEFINED);
        let state = self.allocate_pending_intl_relative_time_format(
            PendingIntlRelativeTimeFormat::new(new_target, options, supported_locales),
        )?;
        self.dispatch_intl_relative_time_format_nested(
            NativeContinuation::intl_relative_time_format(
                Self::native_site(site),
                IntlRelativeTimeFormatStage::Locales,
                Value::from_heap_ref(state.raw()),
                locales,
            ),
            |isolate| isolate.begin_intl_get_canonical_locales(site),
        )
    }

    /// Resumes locale canonicalization or one observable option Get/ToString boundary.
    pub(crate) fn resume_intl_relative_time_format(
        &mut self,
        continuation: NativeContinuation,
        stage: IntlRelativeTimeFormatStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.pending_intl_relative_time_format_reference(continuation.first())?;
        if stage == IntlRelativeTimeFormatStage::Locales {
            return self.resume_intl_relative_time_format_locales(
                continuation.site(),
                state,
                value,
            );
        }
        if value == UNDEFINED {
            return self.advance_intl_relative_time_format_option(
                continuation.site(),
                state,
                stage,
            );
        }
        self.update_pending_intl_relative_time_format(state, |pending| pending.stage = stage)?;
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlRelativeTimeFormatStringOption,
                continuation.site().caller_base,
                continuation.site().destination,
                Value::from_heap_ref(state.raw()),
                value,
                continuation.site().call_site,
            );
        }
        self.resume_intl_relative_time_format_option_primitive(continuation.site(), state, value)
    }

    /// Stores canonical locales and enters the exact RelativeTimeFormat option order.
    fn resume_intl_relative_time_format_locales(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlRelativeTimeFormat>,
        locales: Value,
    ) -> Result<(), ExecutionError> {
        self.set_pending_intl_relative_time_format_value(
            state,
            |pending| &mut pending.locales,
            locales,
        )?;
        let snapshot = self.pending_intl_relative_time_format_snapshot(state)?;
        if snapshot.options == UNDEFINED {
            return if snapshot.supported_locales {
                self.finish_intl_relative_time_format_supported_locales(site, state)
            } else {
                self.finish_intl_relative_time_format_construction(site, state)
            };
        }
        let options = self.coerce_to_object(snapshot.options)?;
        self.set_pending_intl_relative_time_format_value(
            state,
            |pending| &mut pending.options,
            options,
        )?;
        self.dispatch_intl_relative_time_format_option_get(
            site,
            state,
            IntlRelativeTimeFormatStage::LocaleMatcher,
        )
    }

    /// Converts and parses one string-valued option before advancing observable Gets.
    pub(crate) fn resume_intl_relative_time_format_option_primitive(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlRelativeTimeFormat>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let string = self.primitive_to_string_value(primitive)?;
        let text = self
            .string_value_to_ascii(string)
            .map_err(|_| ExecutionError::InvalidIntlRelativeTimeFormatOption)?;
        let stage = self.pending_intl_relative_time_format_stage(state)?;
        match stage {
            IntlRelativeTimeFormatStage::LocaleMatcher => {
                let matcher = match text.as_str() {
                    "lookup" => IntlLocaleMatcher::Lookup,
                    "best fit" => IntlLocaleMatcher::BestFit,
                    _ => return Err(ExecutionError::InvalidIntlRelativeTimeFormatOption),
                };
                self.update_pending_intl_relative_time_format(state, |pending| {
                    pending.locale_matcher = matcher;
                })?;
            }
            IntlRelativeTimeFormatStage::NumberingSystem => {
                if !is_unicode_locale_type(&text) {
                    return Err(ExecutionError::InvalidIntlRelativeTimeFormatOption);
                }
                self.set_pending_intl_relative_time_format_value(
                    state,
                    |pending| &mut pending.numbering_system,
                    string,
                )?;
            }
            IntlRelativeTimeFormatStage::Style => {
                let style = match text.as_str() {
                    "long" => IntlRelativeTimeFormatStyle::Long,
                    "short" => IntlRelativeTimeFormatStyle::Short,
                    "narrow" => IntlRelativeTimeFormatStyle::Narrow,
                    _ => return Err(ExecutionError::InvalidIntlRelativeTimeFormatOption),
                };
                self.update_pending_intl_relative_time_format(state, |pending| {
                    pending.style = style;
                })?;
            }
            IntlRelativeTimeFormatStage::Numeric => {
                let numeric = match text.as_str() {
                    "always" => IntlRelativeTimeFormatNumeric::Always,
                    "auto" => IntlRelativeTimeFormatNumeric::Auto,
                    _ => return Err(ExecutionError::InvalidIntlRelativeTimeFormatOption),
                };
                self.update_pending_intl_relative_time_format(state, |pending| {
                    pending.numeric = numeric;
                })?;
            }
            _ => return Err(ExecutionError::MissingNativeContinuation),
        }
        self.advance_intl_relative_time_format_option(site, state, stage)
    }

    /// Advances localeMatcher/numberingSystem/style/numeric and closes static filtering early.
    fn advance_intl_relative_time_format_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlRelativeTimeFormat>,
        stage: IntlRelativeTimeFormatStage,
    ) -> Result<(), ExecutionError> {
        let supported = self
            .pending_intl_relative_time_format_snapshot(state)?
            .supported_locales;
        let next = match stage {
            IntlRelativeTimeFormatStage::LocaleMatcher if supported => None,
            IntlRelativeTimeFormatStage::LocaleMatcher => {
                Some(IntlRelativeTimeFormatStage::NumberingSystem)
            }
            IntlRelativeTimeFormatStage::NumberingSystem => {
                Some(IntlRelativeTimeFormatStage::Style)
            }
            IntlRelativeTimeFormatStage::Style => Some(IntlRelativeTimeFormatStage::Numeric),
            IntlRelativeTimeFormatStage::Numeric => None,
            _ => return Err(ExecutionError::MissingNativeContinuation),
        };
        let Some(next) = next else {
            return if supported {
                self.finish_intl_relative_time_format_supported_locales(site, state)
            } else {
                self.finish_intl_relative_time_format_construction(site, state)
            };
        };
        self.dispatch_intl_relative_time_format_option_get(site, state, next)
    }

    /// Performs one Proxy/accessor-aware option Get under a typed continuation.
    fn dispatch_intl_relative_time_format_option_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlRelativeTimeFormat>,
        stage: IntlRelativeTimeFormatStage,
    ) -> Result<(), ExecutionError> {
        self.update_pending_intl_relative_time_format(state, |pending| pending.stage = stage)?;
        let snapshot = self.pending_intl_relative_time_format_snapshot(state)?;
        let key = self
            .intern_intrinsic_name(relative_time_format_option_name(stage)?)?
            .into();
        let continuation = NativeContinuation::intl_relative_time_format(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            snapshot.options,
        );
        match self.resolve_property_read_until_proxy(snapshot.options, key)? {
            PropertyReadResolution::Read(PropertyRead::Missing) => {
                self.resume_intl_relative_time_format(continuation, stage, UNDEFINED)
            }
            PropertyReadResolution::Read(PropertyRead::Data(value)) => {
                self.resume_intl_relative_time_format(continuation, stage, value)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) if getter == UNDEFINED => {
                self.resume_intl_relative_time_format(continuation, stage, UNDEFINED)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => self
                .dispatch_property_callback(
                    NativeContinuation::intl_relative_time_format_property_get(
                        site,
                        Value::from_heap_ref(state.raw()),
                        snapshot.options,
                    ),
                    getter,
                )
                .map(|_| ()),
            PropertyReadResolution::Proxy(_) => {
                self.dispatch_intl_relative_time_format_nested(continuation, |isolate| {
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

    /// Creates the provider backend and branded object with newTarget-Realm fallback semantics.
    fn finish_intl_relative_time_format_construction(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlRelativeTimeFormat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_relative_time_format_snapshot(state)?;
        let numbering_system = if snapshot.numbering_system == UNDEFINED {
            None
        } else {
            Some(
                self.string_value_to_ascii(snapshot.numbering_system)?
                    .into_boxed_str(),
            )
        };
        let request = IntlRelativeTimeFormatRequest {
            locales: self.intl_relative_time_format_locale_strings(snapshot.locales)?,
            locale_matcher: snapshot.locale_matcher,
            numbering_system,
            style: snapshot.style,
            numeric: snapshot.numeric,
        };
        let creation = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .create_relative_time_format(request)
            .map_err(ExecutionError::IntlProvider)?;
        let prototype_atom = self.prototype_atom()?;
        let default_prototype = self
            .realm
            .intl_relative_time_format_prototype
            .expect("Intl.RelativeTimeFormat prototype initializes before construction");
        let prototype = self
            .constructor_prototype_value(snapshot.new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .or_else(|| {
                self.realm_for_callable(snapshot.new_target)
                    .ok()
                    .and_then(|realm| {
                        self.realm_intrinsic_prototype(
                            realm,
                            IntrinsicPrototypeKind::IntlRelativeTimeFormat,
                        )
                    })
            })
            .unwrap_or(default_prototype);
        let object = self.allocate_intl_relative_time_format_object(
            creation,
            prototype,
            AllocationSpace::Young,
        )?;
        self.write(site.caller_base, site.destination, object)
    }

    /// Filters canonical requested locales and publishes a fresh intrinsic Array.
    fn finish_intl_relative_time_format_supported_locales(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlRelativeTimeFormat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_relative_time_format_snapshot(state)?;
        let locales = self.intl_relative_time_format_locale_strings(snapshot.locales)?;
        let supported = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .relative_time_format_supported_locales(&locales, snapshot.locale_matcher)
            .map_err(ExecutionError::IntlProvider)?;
        self.materialize_intl_relative_time_format_locales(site, supported)
    }

    /// Brands the receiver and starts ToNumber(value), followed by ToString(unit).
    pub(crate) fn begin_intl_relative_time_format_format(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_intl_relative_time_format_output(site, false)
    }

    /// Brands the receiver and starts the structured two-argument formatting pipeline.
    pub(crate) fn begin_intl_relative_time_format_format_to_parts(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_intl_relative_time_format_output(site, true)
    }

    /// Allocates one state object before any argument conversion can execute JavaScript.
    fn begin_intl_relative_time_format_output(
        &mut self,
        site: &CallSite,
        to_parts: bool,
    ) -> Result<(), ExecutionError> {
        self.intl_relative_time_format_reference(site.this_value)?;
        let value = self.call_argument(site, 0)?.unwrap_or(UNDEFINED);
        let unit = self.call_argument(site, 1)?.unwrap_or(UNDEFINED);
        let mut pending = PendingIntlRelativeTimeFormat::new(UNDEFINED, UNDEFINED, false);
        pending.receiver = site.this_value;
        pending.value = value;
        pending.unit = unit;
        pending.stage = if to_parts {
            IntlRelativeTimeFormatStage::FormatToPartsValue
        } else {
            IntlRelativeTimeFormatStage::FormatValue
        };
        let state = self.allocate_pending_intl_relative_time_format(pending)?;
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlRelativeTimeFormatValue,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.resume_intl_relative_time_format_value_conversion(
            Self::native_site(site),
            state,
            value,
        )
    }

    /// Finishes ToNumber(value), stores its scalar, and starts ToString(unit).
    pub(crate) fn resume_intl_relative_time_format_value_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlRelativeTimeFormat>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let number = self.convert_to_number(primitive)?;
        let numeric =
            numeric_value(number).ok_or(ExecutionError::UnsupportedNumberConversion(number))?;
        if !numeric.is_finite() {
            return Err(ExecutionError::InvalidIntlRelativeTimeFormatOption);
        }
        self.set_pending_intl_relative_time_format_value(
            state,
            |pending| &mut pending.value,
            number,
        )?;
        let snapshot = self.pending_intl_relative_time_format_snapshot(state)?;
        self.update_pending_intl_relative_time_format(state, |pending| {
            pending.stage = match pending.stage {
                IntlRelativeTimeFormatStage::FormatValue => IntlRelativeTimeFormatStage::FormatUnit,
                IntlRelativeTimeFormatStage::FormatToPartsValue => {
                    IntlRelativeTimeFormatStage::FormatToPartsUnit
                }
                other => other,
            };
        })?;
        if self.is_object_value(snapshot.unit) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlRelativeTimeFormatUnit,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                snapshot.unit,
                site.call_site,
            );
        }
        self.resume_intl_relative_time_format_unit_conversion(site, state, snapshot.unit)
    }

    /// Finishes ToString(unit), validates singular/plural aliases, and calls the backend.
    pub(crate) fn resume_intl_relative_time_format_unit_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlRelativeTimeFormat>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let string = self.primitive_to_string_value(primitive)?;
        self.set_pending_intl_relative_time_format_value(
            state,
            |pending| &mut pending.unit,
            string,
        )?;
        let snapshot = self.pending_intl_relative_time_format_snapshot(state)?;
        let unit_text = self
            .string_value_to_ascii(string)
            .map_err(|_| ExecutionError::InvalidIntlRelativeTimeFormatOption)?;
        let unit = parse_relative_time_unit(&unit_text)
            .ok_or(ExecutionError::InvalidIntlRelativeTimeFormatOption)?;
        let mathematical = self.intl_relative_time_format_mathematical_value(snapshot.value)?;
        let to_parts = snapshot.stage == IntlRelativeTimeFormatStage::FormatToPartsUnit;
        self.finish_intl_relative_time_format_output(
            site,
            snapshot.receiver,
            mathematical,
            unit,
            to_parts,
        )
    }

    /// Calls the immutable provider payload without retaining a borrow across allocation.
    fn finish_intl_relative_time_format_output(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        value: IntlMathematicalValue,
        unit: IntlRelativeTimeUnit,
        to_parts: bool,
    ) -> Result<(), ExecutionError> {
        let object = self.intl_relative_time_format_reference(receiver)?;
        let payload = self.intl_relative_time_format_snapshot(object)?.payload;
        if to_parts {
            let parts = self.heap.with_running_scope(|scope| {
                let payload = scope.root(payload).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(payload, self.types.intl_relative_time_format_payload)
                        .map_err(ExecutionError::NoGcBorrow)?
                        .backend
                        .format_to_parts(&value, unit)
                        .map_err(ExecutionError::IntlProvider)
                })
            })?;
            return self.materialize_intl_relative_time_format_parts(site, parts, unit);
        }
        let formatted = self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(payload, self.types.intl_relative_time_format_payload)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .backend
                    .format(&value, unit)
                    .map_err(ExecutionError::IntlProvider)
            })
        })?;
        let result = self.allocate_runtime_string(
            JsString::try_from_utf16(&formatted).map_err(ExecutionError::PropertyKeyString)?,
        )?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Publishes a fresh parts Array with `type`, `value`, and optional canonical `unit`.
    fn materialize_intl_relative_time_format_parts(
        &mut self,
        site: NativeContinuationSite,
        parts: IntlFormattedRelativeTimeParts,
        unit: IntlRelativeTimeUnit,
    ) -> Result<(), ExecutionError> {
        validate_intl_relative_time_format_parts(&parts)?;
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.RelativeTimeFormat"),
        )?;
        self.write(site.caller_base, site.destination, result)?;
        let type_key = self.intern_intrinsic_name(b"type")?;
        let value_key = self.intern_intrinsic_name(b"value")?;
        let unit_key = self.intern_intrinsic_name(b"unit")?;
        for (index, span) in parts.spans.iter().copied().enumerate() {
            let result = self.read(site.caller_base, site.destination)?;
            let part = self.create_ordinary_object()?;
            let property = self.property_key_atom(safe_integer_value(
                u64::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?,
            ))?;
            self.set_own_data_property(result, property, part)?;
            let (kind, part) = self.allocate_runtime_string_retaining(
                JsString::try_from_latin1(relative_time_part_name(span.kind))
                    .map_err(ExecutionError::PropertyKeyString)?,
                part,
            )?;
            self.set_own_data_property(part, type_key, kind)?;
            let start = usize::try_from(span.start)
                .map_err(|_| ExecutionError::IntlProvider(HostProviderError::Failure(6)))?;
            let end = usize::try_from(span.end)
                .map_err(|_| ExecutionError::IntlProvider(HostProviderError::Failure(6)))?;
            let units = parts
                .formatted
                .get(start..end)
                .ok_or(ExecutionError::IntlProvider(HostProviderError::Failure(6)))?;
            let (value, part) = self.allocate_runtime_string_retaining(
                JsString::try_from_utf16(units).map_err(ExecutionError::PropertyKeyString)?,
                part,
            )?;
            self.set_own_data_property(part, value_key, value)?;
            if span.has_unit {
                let (unit, part) = self.allocate_runtime_string_retaining(
                    JsString::try_from_latin1(relative_time_unit_name(unit))
                        .map_err(ExecutionError::PropertyKeyString)?,
                    part,
                )?;
                self.set_own_data_property(part, unit_key, unit)?;
            }
        }
        Ok(())
    }

    /// Returns locale/style/numeric/numberingSystem in specification property order.
    pub(crate) fn call_intl_relative_time_format_resolved_options(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let object = self.intl_relative_time_format_reference(site.this_value)?;
        let payload = self.intl_relative_time_format_snapshot(object)?.payload;
        let resolved = self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(payload, self.types.intl_relative_time_format_payload)
                    .map(|payload| payload.resolved.clone())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let result = self.create_ordinary_object()?;
        self.write(site.caller_base, site.destination, result)?;
        self.set_intl_relative_time_format_resolved_string(
            result,
            b"locale",
            resolved.locale.as_bytes(),
        )?;
        self.set_intl_relative_time_format_resolved_string(
            result,
            b"style",
            match resolved.style {
                IntlRelativeTimeFormatStyle::Long => b"long",
                IntlRelativeTimeFormatStyle::Short => b"short",
                IntlRelativeTimeFormatStyle::Narrow => b"narrow",
            },
        )?;
        self.set_intl_relative_time_format_resolved_string(
            result,
            b"numeric",
            match resolved.numeric {
                IntlRelativeTimeFormatNumeric::Always => b"always",
                IntlRelativeTimeFormatNumeric::Auto => b"auto",
            },
        )?;
        self.set_intl_relative_time_format_resolved_string(
            result,
            b"numberingSystem",
            resolved.numbering_system.as_bytes(),
        )
    }

    fn intl_relative_time_format_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<IntlRelativeTimeFormatObject>, ExecutionError> {
        let raw = value.as_heap_ref().ok_or(
            ExecutionError::IncompatibleIntlRelativeTimeFormatReceiver(value),
        )?;
        self.heap
            .checked_reference(raw, self.types.intl_relative_time_format_object)
            .map_err(|_| ExecutionError::IncompatibleIntlRelativeTimeFormatReceiver(value))
    }

    fn intl_relative_time_format_snapshot(
        &mut self,
        object: GcRef<IntlRelativeTimeFormatObject>,
    ) -> Result<IntlRelativeTimeFormatObject, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(object, self.types.intl_relative_time_format_object)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn intl_relative_time_format_locale_strings(
        &mut self,
        locales: Value,
    ) -> Result<Box<[Box<str>]>, ExecutionError> {
        let values = self.copy_packed_intl_array(locales)?;
        let mut strings = Vec::new();
        strings
            .try_reserve_exact(values.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for value in values {
            strings.push(self.string_value_to_ascii(value)?.into_boxed_str());
        }
        Ok(strings.into_boxed_slice())
    }

    /// Converts a finite Number into the provider-neutral exact decimal boundary.
    fn intl_relative_time_format_mathematical_value(
        &mut self,
        value: Value,
    ) -> Result<IntlMathematicalValue, ExecutionError> {
        let number =
            numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        if !number.is_finite() {
            return Err(ExecutionError::InvalidIntlRelativeTimeFormatOption);
        }
        if number == 0.0 && number.is_sign_negative() {
            return Ok(IntlMathematicalValue::NegativeZero);
        }
        let string = self.number_to_string(Value::from_f64(number), None)?;
        let units = self.string_value_to_utf16(string)?;
        let value = String::from_utf16(&units)
            .map(String::into_boxed_str)
            .map_err(|_| ExecutionError::NumberFormatInvalidDigit)?;
        Ok(IntlMathematicalValue::Finite(value))
    }

    /// Publishes provider-filtered locales without observing Array prototype methods.
    fn materialize_intl_relative_time_format_locales(
        &mut self,
        site: NativeContinuationSite,
        locales: Box<[Box<str>]>,
    ) -> Result<(), ExecutionError> {
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.RelativeTimeFormat"),
        )?;
        self.write(site.caller_base, site.destination, result)?;
        for (index, locale) in locales.into_vec().into_iter().enumerate() {
            let result = self.read(site.caller_base, site.destination)?;
            let (locale, result) = self.allocate_runtime_string_retaining(
                JsString::try_from_str(&locale).map_err(ExecutionError::PropertyKeyString)?,
                result,
            )?;
            self.write(site.caller_base, site.destination, result)?;
            let key = self.property_key_atom(safe_integer_value(
                u64::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?,
            ))?;
            self.set_own_data_property(result, key, locale)?;
        }
        Ok(())
    }

    fn set_intl_relative_time_format_resolved_string(
        &mut self,
        result: Value,
        name: &[u8],
        value: &[u8],
    ) -> Result<(), ExecutionError> {
        let (value, result) = self.allocate_runtime_string_retaining(
            JsString::try_from_latin1(value).map_err(ExecutionError::PropertyKeyString)?,
            result,
        )?;
        let key = self.intern_intrinsic_name(name)?;
        self.set_own_data_property(result, key, value)
    }

    /// Drains synchronous nested work while preserving the typed parent continuation.
    fn dispatch_intl_relative_time_format_nested(
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
        let NativeContinuationKind::IntlRelativeTimeFormat(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_intl_relative_time_format(continuation, stage, value)
    }
}

fn relative_time_format_option_name(
    stage: IntlRelativeTimeFormatStage,
) -> Result<&'static [u8], ExecutionError> {
    match stage {
        IntlRelativeTimeFormatStage::LocaleMatcher => Ok(b"localeMatcher"),
        IntlRelativeTimeFormatStage::NumberingSystem => Ok(b"numberingSystem"),
        IntlRelativeTimeFormatStage::Style => Ok(b"style"),
        IntlRelativeTimeFormatStage::Numeric => Ok(b"numeric"),
        _ => Err(ExecutionError::MissingNativeContinuation),
    }
}

#[inline(always)]
fn is_unicode_locale_type(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|subtag| {
            (3..=8).contains(&subtag.len())
                && subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

#[inline(always)]
fn parse_relative_time_unit(value: &str) -> Option<IntlRelativeTimeUnit> {
    Some(match value {
        "second" | "seconds" => IntlRelativeTimeUnit::Second,
        "minute" | "minutes" => IntlRelativeTimeUnit::Minute,
        "hour" | "hours" => IntlRelativeTimeUnit::Hour,
        "day" | "days" => IntlRelativeTimeUnit::Day,
        "week" | "weeks" => IntlRelativeTimeUnit::Week,
        "month" | "months" => IntlRelativeTimeUnit::Month,
        "quarter" | "quarters" => IntlRelativeTimeUnit::Quarter,
        "year" | "years" => IntlRelativeTimeUnit::Year,
        _ => return None,
    })
}

#[inline(always)]
const fn relative_time_unit_name(unit: IntlRelativeTimeUnit) -> &'static [u8] {
    match unit {
        IntlRelativeTimeUnit::Second => b"second",
        IntlRelativeTimeUnit::Minute => b"minute",
        IntlRelativeTimeUnit::Hour => b"hour",
        IntlRelativeTimeUnit::Day => b"day",
        IntlRelativeTimeUnit::Week => b"week",
        IntlRelativeTimeUnit::Month => b"month",
        IntlRelativeTimeUnit::Quarter => b"quarter",
        IntlRelativeTimeUnit::Year => b"year",
    }
}

#[inline(always)]
const fn relative_time_part_name(kind: IntlNumberFormatPartType) -> &'static [u8] {
    match kind {
        IntlNumberFormatPartType::Literal => b"literal",
        IntlNumberFormatPartType::Nan => b"nan",
        IntlNumberFormatPartType::Infinity => b"infinity",
        IntlNumberFormatPartType::Integer => b"integer",
        IntlNumberFormatPartType::Group => b"group",
        IntlNumberFormatPartType::Decimal => b"decimal",
        IntlNumberFormatPartType::Fraction => b"fraction",
        IntlNumberFormatPartType::PlusSign => b"plusSign",
        IntlNumberFormatPartType::MinusSign => b"minusSign",
        IntlNumberFormatPartType::PercentSign => b"percentSign",
        IntlNumberFormatPartType::Currency => b"currency",
        IntlNumberFormatPartType::Unit => b"unit",
        IntlNumberFormatPartType::ExponentSeparator => b"exponentSeparator",
        IntlNumberFormatPartType::ExponentMinusSign => b"exponentMinusSign",
        IntlNumberFormatPartType::ExponentInteger => b"exponentInteger",
        IntlNumberFormatPartType::Compact => b"compact",
        IntlNumberFormatPartType::ApproximatelySign => b"approximatelySign",
    }
}

/// Rejects malformed provider spans before publishing a partially populated result.
fn validate_intl_relative_time_format_parts(
    parts: &IntlFormattedRelativeTimeParts,
) -> Result<(), ExecutionError> {
    let mut cursor = 0_u32;
    for span in &parts.spans {
        if span.start != cursor || span.end < span.start {
            return Err(ExecutionError::IntlProvider(HostProviderError::Failure(6)));
        }
        cursor = span.end;
    }
    let length = u32::try_from(parts.formatted.len())
        .map_err(|_| ExecutionError::IntlProvider(HostProviderError::Failure(6)))?;
    if cursor != length || (length != 0 && parts.spans.is_empty()) {
        return Err(ExecutionError::IntlProvider(HostProviderError::Failure(6)));
    }
    Ok(())
}
