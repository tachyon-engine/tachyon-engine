//! Resumable ECMA-402 option processing for `Intl.NumberFormat`.

use super::super::*;

mod parsing;
use parsing::*;

const UNDEFINED: Value = Value::from_immediate(Immediate::Undefined);

mod state;
pub(crate) use state::PendingIntlNumberFormat;
use state::PendingIntlNumberFormatRoots;

impl Isolate {
    /// Starts constructor locale canonicalization with a dedicated traced pending record.
    pub(crate) fn start_intl_number_format_constructor(
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
        let mut pending = PendingIntlNumberFormat::new(new_target, options, false);
        if called_without_new {
            pending.legacy_receiver = site.this_value;
        }
        let state = self.allocate_pending_intl_number_format(pending)?;
        self.dispatch_intl_number_format_nested(
            NativeContinuation::intl_number_format(
                native_site(site),
                IntlNumberFormatStage::Locales,
                Value::from_heap_ref(state.raw()),
                locales,
            ),
            |isolate| isolate.begin_intl_get_canonical_locales(site),
        )
    }

    /// Runs NumberFormat construction for `Number.prototype.toLocaleString` without JS re-entry.
    pub(crate) fn start_number_to_locale_string(
        &mut self,
        site: &CallSite,
        number: Value,
    ) -> Result<(), ExecutionError> {
        let locales = self.call_argument(site, 0)?.unwrap_or(UNDEFINED);
        let options = self.call_argument(site, 1)?.unwrap_or(UNDEFINED);
        let constructor = self
            .realm
            .intl_number_format_constructor
            .expect("Intl.NumberFormat initializes before Number.prototype.toLocaleString");
        let mut pending = PendingIntlNumberFormat::new(constructor, options, false);
        pending.format_value = number;
        let state = self.allocate_pending_intl_number_format(pending)?;
        self.dispatch_intl_number_format_nested(
            NativeContinuation::intl_number_format(
                native_site(site),
                IntlNumberFormatStage::Locales,
                Value::from_heap_ref(state.raw()),
                locales,
            ),
            |isolate| isolate.begin_intl_get_canonical_locales(site),
        )
    }

    /// Starts locale filtering and reads only the supportedLocalesOf localeMatcher option.
    pub(crate) fn start_intl_number_format_supported_locales_of(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let locales = self.call_argument(site, 0)?.unwrap_or(UNDEFINED);
        let options = self.call_argument(site, 1)?.unwrap_or(UNDEFINED);
        let state = self.allocate_pending_intl_number_format(PendingIntlNumberFormat::new(
            UNDEFINED, options, true,
        ))?;
        self.dispatch_intl_number_format_nested(
            NativeContinuation::intl_number_format(
                native_site(site),
                IntlNumberFormatStage::Locales,
                Value::from_heap_ref(state.raw()),
                locales,
            ),
            |isolate| isolate.begin_intl_get_canonical_locales(site),
        )
    }

    /// Resumes locale canonicalization, one option Get, or one delayed numeric conversion.
    pub(crate) fn resume_pending_intl_number_format(
        &mut self,
        continuation: NativeContinuation,
        stage: IntlNumberFormatStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.pending_intl_number_format_reference(continuation.first())?;
        if stage == IntlNumberFormatStage::Locales {
            return self.resume_intl_number_format_locales(continuation.site(), state, value);
        }
        if is_digit_conversion_stage(stage) {
            return self.resume_intl_number_format_option_primitive(
                continuation.site(),
                state,
                value,
            );
        }
        if value.as_immediate() == Some(Immediate::Undefined) {
            return self.store_undefined_number_format_option(continuation.site(), state, stage);
        }
        if is_raw_digit_stage(stage) {
            self.store_raw_digit_value(state, stage, value)?;
            return self.advance_intl_number_format_option(continuation.site(), state, stage);
        }
        self.resume_present_number_format_option(continuation.site(), state, stage, value)
    }

    /// Continues an option after ToPrimitive with the hint selected by its exact option kind.
    pub(crate) fn resume_intl_number_format_option_primitive(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let stage = self.pending_intl_number_format_stage(state)?;
        if is_numeric_conversion_stage(stage) {
            let number = numeric_value(self.convert_to_number(primitive)?)
                .ok_or(ExecutionError::InvalidIntlNumberFormatOption)?;
            return self.store_number_format_numeric_option(site, state, stage, number);
        }
        let string = self.primitive_to_string_value(primitive)?;
        self.store_number_format_string_option(site, state, stage, string)
    }

    /// Stores the canonical locale list and enters the first observable option read.
    fn resume_intl_number_format_locales(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
        locales: Value,
    ) -> Result<(), ExecutionError> {
        self.set_pending_intl_number_format_value(state, |pending| &mut pending.locales, locales)?;
        let snapshot = self.pending_intl_number_format_snapshot(state)?;
        if snapshot.options.as_immediate() == Some(Immediate::Undefined) {
            return if snapshot.supported_locales {
                self.finish_intl_number_format_supported_locales(site, state)
            } else {
                self.update_pending_intl_number_format(state, |pending| {
                    pending.minimum_fraction_digits = Some(0);
                    pending.maximum_fraction_digits = Some(3);
                })?;
                self.finish_intl_number_format_construction(site, state)
            };
        }
        let options = self.coerce_to_object(snapshot.options)?;
        self.set_pending_intl_number_format_value(state, |pending| &mut pending.options, options)?;
        self.dispatch_intl_number_format_option_get(
            site,
            state,
            IntlNumberFormatStage::LocaleMatcher,
        )
    }

    /// Handles a present option without losing its specification-selected coercion hint.
    fn resume_present_number_format_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
        stage: IntlNumberFormatStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.set_pending_intl_number_format_stage(state, stage)?;
        if stage == IntlNumberFormatStage::UseGrouping {
            return self.resume_intl_number_format_grouping(site, state, value);
        }
        if self.is_object_value(value) {
            let consumer = if is_immediate_numeric_stage(stage) {
                ConversionConsumer::IntlNumberFormatNumberOption
            } else {
                ConversionConsumer::IntlNumberFormatStringOption
            };
            return self.dispatch_object_primitive_conversion(
                consumer,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.resume_intl_number_format_option_primitive(site, state, value)
    }

    /// Applies GetBooleanOrStringNumberFormatOption before any possible ToString callback.
    fn resume_intl_number_format_grouping(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if value.as_immediate() == Some(Immediate::True) {
            self.update_pending_intl_number_format(state, |pending| {
                pending.use_grouping = IntlNumberFormatUseGrouping::Always;
            })?;
            return self.advance_intl_number_format_option(
                site,
                state,
                IntlNumberFormatStage::UseGrouping,
            );
        }
        if !self.is_truthy_value(value)? {
            self.update_pending_intl_number_format(state, |pending| {
                pending.use_grouping = IntlNumberFormatUseGrouping::Never;
            })?;
            return self.advance_intl_number_format_option(
                site,
                state,
                IntlNumberFormatStage::UseGrouping,
            );
        }
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlNumberFormatStringOption,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        let string = self.primitive_to_string_value(value)?;
        self.store_number_format_string_option(
            site,
            state,
            IntlNumberFormatStage::UseGrouping,
            string,
        )
    }

    /// Validates one ToString result and stores the corresponding scalar or rooted string slot.
    fn store_number_format_string_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
        stage: IntlNumberFormatStage,
        string: Value,
    ) -> Result<(), ExecutionError> {
        let text = self.intl_number_format_ascii_string(string)?;
        match stage {
            IntlNumberFormatStage::LocaleMatcher => {
                self.set_number_format_locale_matcher(state, &text)?
            }
            IntlNumberFormatStage::NumberingSystem => {
                if !is_unicode_locale_type(&text) {
                    return Err(ExecutionError::InvalidIntlNumberFormatOption);
                }
                self.set_pending_intl_number_format_value(
                    state,
                    |pending| &mut pending.numbering_system,
                    string,
                )?;
            }
            IntlNumberFormatStage::Style => self.set_number_format_style(state, &text)?,
            IntlNumberFormatStage::Currency => {
                let uppercase = normalize_currency(&text)?;
                let value = self.allocate_runtime_string(
                    JsString::try_from_latin1(&uppercase)
                        .map_err(ExecutionError::PropertyKeyString)?,
                )?;
                self.set_pending_intl_number_format_value(
                    state,
                    |pending| &mut pending.currency,
                    value,
                )?;
            }
            IntlNumberFormatStage::CurrencyDisplay => {
                self.set_number_format_currency_display(state, &text)?
            }
            IntlNumberFormatStage::CurrencySign => {
                self.set_number_format_currency_sign(state, &text)?
            }
            IntlNumberFormatStage::Unit => {
                if !is_well_formed_unit(&text) {
                    return Err(ExecutionError::InvalidIntlNumberFormatOption);
                }
                self.set_pending_intl_number_format_value(
                    state,
                    |pending| &mut pending.unit,
                    string,
                )?;
            }
            IntlNumberFormatStage::UnitDisplay => {
                self.set_number_format_unit_display(state, &text)?
            }
            IntlNumberFormatStage::Notation => self.set_number_format_notation(state, &text)?,
            IntlNumberFormatStage::RoundingMode => {
                self.set_number_format_rounding_mode(state, &text)?
            }
            IntlNumberFormatStage::RoundingPriority => {
                self.set_number_format_rounding_priority(state, &text)?
            }
            IntlNumberFormatStage::TrailingZeroDisplay => {
                self.set_number_format_trailing_zero_display(state, &text)?
            }
            IntlNumberFormatStage::CompactDisplay => {
                self.set_number_format_compact_display(state, &text)?
            }
            IntlNumberFormatStage::UseGrouping => self.set_number_format_grouping(state, &text)?,
            IntlNumberFormatStage::SignDisplay => {
                self.set_number_format_sign_display(state, &text)?
            }
            _ => return Err(ExecutionError::MissingNativeContinuation),
        }
        self.advance_intl_number_format_option(site, state, stage)
    }

    /// Floors and range-checks one immediate or delayed numeric option conversion.
    fn store_number_format_numeric_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
        stage: IntlNumberFormatStage,
        number: f64,
    ) -> Result<(), ExecutionError> {
        let (minimum, maximum) = numeric_option_range(stage)?;
        if !number.is_finite() || number < minimum || number > maximum {
            return Err(ExecutionError::InvalidIntlNumberFormatOption);
        }
        let value = number.floor() as u16;
        self.update_pending_intl_number_format(state, |pending| match stage {
            IntlNumberFormatStage::MinimumIntegerDigits => {
                pending.minimum_integer_digits = value as u8
            }
            IntlNumberFormatStage::RoundingIncrement => pending.rounding_increment = value,
            IntlNumberFormatStage::ConvertMinimumSignificantDigits => {
                pending.minimum_significant_digits = Some(value as u8)
            }
            IntlNumberFormatStage::ConvertMaximumSignificantDigits => {
                pending.maximum_significant_digits = Some(value as u8)
            }
            IntlNumberFormatStage::ConvertMinimumFractionDigits => {
                pending.minimum_fraction_digits = Some(value as u8)
            }
            IntlNumberFormatStage::ConvertMaximumFractionDigits => {
                pending.maximum_fraction_digits = Some(value as u8)
            }
            _ => {}
        })?;
        if stage == IntlNumberFormatStage::RoundingIncrement && !is_rounding_increment(value) {
            return Err(ExecutionError::InvalidIntlNumberFormatOption);
        }
        if is_digit_conversion_stage(stage) {
            self.advance_digit_conversion(site, state, stage)
        } else {
            self.advance_intl_number_format_option(site, state, stage)
        }
    }

    /// Preserves the four raw digit Values until SetNumberFormatDigitOptions selects a group.
    fn store_raw_digit_value(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        stage: IntlNumberFormatStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match stage {
            IntlNumberFormatStage::MinimumFractionDigits => self
                .set_pending_intl_number_format_value(
                    state,
                    |pending| &mut pending.minimum_fraction_raw,
                    value,
                ),
            IntlNumberFormatStage::MaximumFractionDigits => self
                .set_pending_intl_number_format_value(
                    state,
                    |pending| &mut pending.maximum_fraction_raw,
                    value,
                ),
            IntlNumberFormatStage::MinimumSignificantDigits => self
                .set_pending_intl_number_format_value(
                    state,
                    |pending| &mut pending.minimum_significant_raw,
                    value,
                ),
            IntlNumberFormatStage::MaximumSignificantDigits => self
                .set_pending_intl_number_format_value(
                    state,
                    |pending| &mut pending.maximum_significant_raw,
                    value,
                ),
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Advances exact option Get order and enters delayed digit interpretation at its boundary.
    fn advance_intl_number_format_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
        stage: IntlNumberFormatStage,
    ) -> Result<(), ExecutionError> {
        if stage == IntlNumberFormatStage::LocaleMatcher
            && self
                .pending_intl_number_format_snapshot(state)?
                .supported_locales
        {
            return self.finish_intl_number_format_supported_locales(site, state);
        }
        if stage == IntlNumberFormatStage::Currency {
            let snapshot = self.pending_intl_number_format_snapshot(state)?;
            if snapshot.style == IntlNumberFormatStyle::Currency && snapshot.currency == UNDEFINED {
                return Err(ExecutionError::MissingIntlNumberFormatCurrency);
            }
        }
        if stage == IntlNumberFormatStage::Unit {
            let snapshot = self.pending_intl_number_format_snapshot(state)?;
            if snapshot.style == IntlNumberFormatStyle::Unit && snapshot.unit == UNDEFINED {
                return Err(ExecutionError::MissingIntlNumberFormatUnit);
            }
        }
        let next = next_number_format_option(stage)?;
        if stage == IntlNumberFormatStage::TrailingZeroDisplay {
            return self.begin_number_format_digit_conversion(site, state);
        }
        if stage == IntlNumberFormatStage::SignDisplay {
            return self.finish_intl_number_format_construction(site, state);
        }
        self.dispatch_intl_number_format_option_get(site, state, next)
    }

    /// Selects significant/fraction groups only after all raw digit properties were read.
    fn begin_number_format_digit_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_number_format_snapshot(state)?;
        let has_significant = snapshot.minimum_significant_raw != UNDEFINED
            || snapshot.maximum_significant_raw != UNDEFINED;
        let has_fraction = snapshot.minimum_fraction_raw != UNDEFINED
            || snapshot.maximum_fraction_raw != UNDEFINED;
        let need_significant =
            snapshot.rounding_priority != IntlNumberFormatRoundingPriority::Auto || has_significant;
        let need_fraction = snapshot.rounding_priority != IntlNumberFormatRoundingPriority::Auto
            || (!has_significant
                && (has_fraction || snapshot.notation != IntlNumberFormatNotation::Compact));
        self.update_pending_intl_number_format(state, |pending| {
            pending.need_significant = need_significant;
            pending.need_fraction = need_fraction;
        })?;
        if need_significant {
            return self.dispatch_digit_conversion(
                site,
                state,
                IntlNumberFormatStage::ConvertMinimumSignificantDigits,
            );
        }
        if need_fraction {
            return self.dispatch_digit_conversion(
                site,
                state,
                IntlNumberFormatStage::ConvertMinimumFractionDigits,
            );
        }
        self.finish_number_format_digit_options(site, state)
    }

    /// Converts only selected raw digit values, preserving the spec's mnsd/mxsd/mnfd/mxfd order.
    fn dispatch_digit_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
        stage: IntlNumberFormatStage,
    ) -> Result<(), ExecutionError> {
        self.set_pending_intl_number_format_stage(state, stage)?;
        let snapshot = self.pending_intl_number_format_snapshot(state)?;
        let value = raw_digit_value(snapshot, stage)?;
        if value == UNDEFINED {
            return self.advance_digit_conversion(site, state, stage);
        }
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlNumberFormatNumberOption,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.resume_intl_number_format_option_primitive(site, state, value)
    }

    /// Moves through selected digit conversions, then normalizes defaults and cross-constraints.
    fn advance_digit_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
        stage: IntlNumberFormatStage,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_number_format_snapshot(state)?;
        match stage {
            IntlNumberFormatStage::ConvertMinimumSignificantDigits => self
                .dispatch_digit_conversion(
                    site,
                    state,
                    IntlNumberFormatStage::ConvertMaximumSignificantDigits,
                ),
            IntlNumberFormatStage::ConvertMaximumSignificantDigits if snapshot.need_fraction => {
                self.dispatch_digit_conversion(
                    site,
                    state,
                    IntlNumberFormatStage::ConvertMinimumFractionDigits,
                )
            }
            IntlNumberFormatStage::ConvertMaximumSignificantDigits => {
                self.finish_number_format_digit_options(site, state)
            }
            IntlNumberFormatStage::ConvertMinimumFractionDigits => self.dispatch_digit_conversion(
                site,
                state,
                IntlNumberFormatStage::ConvertMaximumFractionDigits,
            ),
            IntlNumberFormatStage::ConvertMaximumFractionDigits => {
                self.finish_number_format_digit_options(site, state)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Applies digit defaults, ordering checks, compact semantics, and roundingIncrement constraints.
    fn finish_number_format_digit_options(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_number_format_snapshot(state)?;
        let (mnfd_default, mut mxfd_default) = fraction_defaults(snapshot.style, snapshot.notation);
        if snapshot.rounding_increment != 1 {
            mxfd_default = mnfd_default;
        }
        let significant = normalize_significant_digits(snapshot)?;
        let fraction = normalize_fraction_digits(snapshot, mnfd_default, mxfd_default)?;
        if snapshot.rounding_increment != 1 {
            if snapshot.need_significant
                || snapshot.rounding_priority != IntlNumberFormatRoundingPriority::Auto
            {
                return Err(ExecutionError::InvalidIntlNumberFormatRoundingIncrementCombination);
            }
            if fraction.0 != fraction.1 {
                return Err(ExecutionError::InvalidIntlNumberFormatOption);
            }
        }
        self.update_pending_intl_number_format(state, |pending| {
            pending.minimum_significant_digits =
                if !pending.need_fraction && !pending.need_significant {
                    Some(1)
                } else {
                    significant.0
                };
            pending.maximum_significant_digits =
                if !pending.need_fraction && !pending.need_significant {
                    Some(2)
                } else {
                    significant.1
                };
            pending.minimum_fraction_digits = fraction.0;
            pending.maximum_fraction_digits = fraction.1;
            if !pending.need_fraction && !pending.need_significant {
                pending.rounding_priority = IntlNumberFormatRoundingPriority::MorePrecision;
            }
        })?;
        self.dispatch_intl_number_format_option_get(
            site,
            state,
            IntlNumberFormatStage::CompactDisplay,
        )
    }

    /// Performs one Proxy/accessor-aware Get under a typed NumberFormat continuation.
    fn dispatch_intl_number_format_option_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
        stage: IntlNumberFormatStage,
    ) -> Result<(), ExecutionError> {
        self.set_pending_intl_number_format_stage(state, stage)?;
        let snapshot = self.pending_intl_number_format_snapshot(state)?;
        let key = self
            .intern_intrinsic_name(number_format_option_name(stage)?)?
            .into();
        match self.resolve_property_read_until_proxy(snapshot.options, key)? {
            PropertyReadResolution::Read(PropertyRead::Missing) => self
                .resume_pending_intl_number_format(
                    NativeContinuation::intl_number_format(
                        site,
                        stage,
                        Value::from_heap_ref(state.raw()),
                        snapshot.options,
                    ),
                    stage,
                    UNDEFINED,
                ),
            PropertyReadResolution::Read(PropertyRead::Data(value)) => self
                .resume_pending_intl_number_format(
                    NativeContinuation::intl_number_format(
                        site,
                        stage,
                        Value::from_heap_ref(state.raw()),
                        snapshot.options,
                    ),
                    stage,
                    value,
                ),
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) if getter == UNDEFINED => {
                self.resume_pending_intl_number_format(
                    NativeContinuation::intl_number_format(
                        site,
                        stage,
                        Value::from_heap_ref(state.raw()),
                        snapshot.options,
                    ),
                    stage,
                    UNDEFINED,
                )
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => self
                .dispatch_property_callback(
                    NativeContinuation::intl_number_format_property_get(
                        site,
                        Value::from_heap_ref(state.raw()),
                        snapshot.options,
                    ),
                    getter,
                )
                .map(|_| ()),
            PropertyReadResolution::Proxy(_) => self.dispatch_intl_number_format_nested(
                NativeContinuation::intl_number_format(
                    site,
                    stage,
                    Value::from_heap_ref(state.raw()),
                    snapshot.options,
                ),
                |isolate| {
                    isolate
                        .dispatch_proxy_aware_property_read(
                            site,
                            snapshot.options,
                            snapshot.options,
                            key,
                        )
                        .map(|_| ())
                },
            ),
        }
    }

    /// Builds the provider request only after all observable option processing has completed.
    fn finish_intl_number_format_construction(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_number_format_snapshot(state)?;
        let request = self.number_format_request(snapshot)?;
        let creation = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .create_number_format(request)
            .map_err(ExecutionError::IntlProvider)?;
        let prototype_atom = self.prototype_atom()?;
        let default_prototype = self
            .realm
            .intl_number_format_prototype
            .expect("Intl.NumberFormat prototype initializes before construction");
        let prototype = self
            .constructor_prototype_value(snapshot.new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .or_else(|| {
                self.realm_for_callable(snapshot.new_target)
                    .ok()
                    .and_then(|realm| {
                        self.realm_intrinsic_prototype(
                            realm,
                            IntrinsicPrototypeKind::IntlNumberFormat,
                        )
                    })
            })
            .unwrap_or(default_prototype);
        let number_format =
            self.allocate_intl_number_format_object(creation, prototype, AllocationSpace::Young)?;
        if snapshot.format_value.as_immediate() != Some(Immediate::Undefined) {
            self.write(site.caller_base, site.destination, number_format)?;
            return self.finish_intl_number_format_value_to_string(
                site,
                number_format,
                snapshot.format_value,
            );
        }
        if snapshot.legacy_receiver.as_immediate() == Some(Immediate::Undefined)
            || !self.is_object_value(snapshot.legacy_receiver)
        {
            return self.write(site.caller_base, site.destination, number_format);
        }
        self.begin_intl_number_format_chain(site, snapshot.legacy_receiver, number_format)
    }

    /// Filters canonical locales and materializes a fresh intrinsic Array without observable push.
    fn finish_intl_number_format_supported_locales(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_number_format_snapshot(state)?;
        let locales = self.number_format_locale_strings(snapshot.locales)?;
        let supported = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .number_format_supported_locales(&locales, snapshot.locale_matcher)
            .map_err(ExecutionError::IntlProvider)?;
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.NumberFormat"),
        )?;
        self.write(site.caller_base, site.destination, result)?;
        for (index, locale) in supported.into_vec().into_iter().enumerate() {
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

    /// Copies managed string slots into the engine-neutral provider request.
    fn number_format_request(
        &mut self,
        snapshot: PendingIntlNumberFormat,
    ) -> Result<IntlNumberFormatRequest, ExecutionError> {
        Ok(IntlNumberFormatRequest {
            locales: self.number_format_locale_strings(snapshot.locales)?,
            locale_matcher: snapshot.locale_matcher,
            numbering_system: self.optional_number_format_string(snapshot.numbering_system)?,
            options: IntlNumberFormatOptions {
                style: snapshot.style,
                currency: if snapshot.style == IntlNumberFormatStyle::Currency {
                    self.optional_number_format_string(snapshot.currency)?
                } else {
                    None
                },
                currency_display: snapshot.currency_display,
                currency_sign: snapshot.currency_sign,
                unit: if snapshot.style == IntlNumberFormatStyle::Unit {
                    self.optional_number_format_string(snapshot.unit)?
                } else {
                    None
                },
                unit_display: snapshot.unit_display,
                minimum_integer_digits: snapshot.minimum_integer_digits,
                minimum_fraction_digits: snapshot.minimum_fraction_digits,
                maximum_fraction_digits: snapshot.maximum_fraction_digits,
                minimum_significant_digits: snapshot.minimum_significant_digits,
                maximum_significant_digits: snapshot.maximum_significant_digits,
                rounding_increment: snapshot.rounding_increment,
                rounding_mode: snapshot.rounding_mode,
                rounding_priority: snapshot.rounding_priority,
                trailing_zero_display: snapshot.trailing_zero_display,
                notation: snapshot.notation,
                compact_display: snapshot.compact_display,
                use_grouping: snapshot.use_grouping,
                sign_display: snapshot.sign_display,
            },
        })
    }

    /// Drains a synchronous nested operation or leaves its parent below a new JavaScript frame.
    fn dispatch_intl_number_format_nested(
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
        let NativeContinuationKind::IntlNumberFormat(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_pending_intl_number_format(continuation, stage, value)
    }

    /// Allocates the compact pending record under a root set containing every managed Value.
    fn allocate_pending_intl_number_format(
        &mut self,
        pending: PendingIntlNumberFormat,
    ) -> Result<GcRef<PendingIntlNumberFormat>, ExecutionError> {
        let mut roots = PendingIntlNumberFormatRoots {
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
                self.types.pending_intl_number_format,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked pending reference from a traced continuation Value.
    pub(crate) fn pending_intl_number_format_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingIntlNumberFormat>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_intl_number_format)
            .map_err(ExecutionError::HeapReference)
    }

    /// Copies scalar state without retaining a no-GC borrow across callbacks or allocations.
    fn pending_intl_number_format_snapshot(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
    ) -> Result<PendingIntlNumberFormat, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_intl_number_format)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Returns the currently active option/conversion stage for callback resumption.
    pub(crate) fn pending_intl_number_format_stage(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
    ) -> Result<IntlNumberFormatStage, ExecutionError> {
        self.pending_intl_number_format_snapshot(state)
            .map(|pending| pending.stage)
    }

    /// Updates scalar state fields under a short no-GC mutable borrow.
    fn update_pending_intl_number_format(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        update: impl FnOnce(&mut PendingIntlNumberFormat),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                update(
                    no_gc
                        .borrow_mut(state, self.types.pending_intl_number_format)
                        .map_err(ExecutionError::NoGcBorrow)?,
                );
                Ok(())
            })
        })
    }

    /// Updates one managed Value slot and records the generational write barrier.
    fn set_pending_intl_number_format_value(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        select: impl FnOnce(&mut PendingIntlNumberFormat) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                *select(
                    no_gc
                        .borrow_mut(state, self.types.pending_intl_number_format)
                        .map_err(ExecutionError::NoGcBorrow)?,
                ) = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    fn set_pending_intl_number_format_stage(
        &mut self,
        state: GcRef<PendingIntlNumberFormat>,
        stage: IntlNumberFormatStage,
    ) -> Result<(), ExecutionError> {
        self.update_pending_intl_number_format(state, |pending| pending.stage = stage)
    }

    fn number_format_locale_strings(
        &mut self,
        value: Value,
    ) -> Result<Box<[Box<str>]>, ExecutionError> {
        let values = self.copy_packed_intl_array(value)?;
        let mut locales = Vec::new();
        locales
            .try_reserve_exact(values.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for locale in values {
            locales.push(self.intl_number_format_ascii_string(locale)?);
        }
        Ok(locales.into_boxed_slice())
    }

    fn optional_number_format_string(
        &mut self,
        value: Value,
    ) -> Result<Option<Box<str>>, ExecutionError> {
        (value != UNDEFINED)
            .then(|| self.intl_number_format_ascii_string(value))
            .transpose()
    }

    fn intl_number_format_ascii_string(
        &mut self,
        value: Value,
    ) -> Result<Box<str>, ExecutionError> {
        self.intl_ascii_string(value).map_err(|error| match error {
            ExecutionError::InvalidIntlCollatorOption => {
                ExecutionError::InvalidIntlNumberFormatOption
            }
            other => other,
        })
    }

    fn store_undefined_number_format_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlNumberFormat>,
        stage: IntlNumberFormatStage,
    ) -> Result<(), ExecutionError> {
        if is_raw_digit_stage(stage) {
            self.store_raw_digit_value(state, stage, UNDEFINED)?;
        }
        if stage == IntlNumberFormatStage::UseGrouping {
            let notation = self.pending_intl_number_format_snapshot(state)?.notation;
            self.update_pending_intl_number_format(state, |pending| {
                pending.use_grouping = if notation == IntlNumberFormatNotation::Compact {
                    IntlNumberFormatUseGrouping::Min2
                } else {
                    IntlNumberFormatUseGrouping::Auto
                };
            })?;
        }
        self.advance_intl_number_format_option(site, state, stage)
    }
}
