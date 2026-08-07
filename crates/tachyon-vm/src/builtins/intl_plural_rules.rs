//! Provider-backed `Intl.PluralRules` construction and plural selection.

use super::super::*;
use crate::runtime::fiber::IntlPluralRulesStage;

mod options;
mod state;

use options::*;
pub(crate) use state::PendingIntlPluralRules;

const UNDEFINED: Value = Value::from_immediate(Immediate::Undefined);

impl Isolate {
    /// Starts construction after enforcing the new-only PluralRules contract.
    pub(crate) fn begin_intl_plural_rules_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(site.new_target) {
            return Err(ExecutionError::NonConstructor(site.callee));
        }
        self.begin_intl_plural_rules_options(site, site.new_target, false)
    }

    /// Starts locale filtering while observing only the localeMatcher option.
    pub(crate) fn begin_intl_plural_rules_supported_locales_of(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_intl_plural_rules_options(site, UNDEFINED, true)
    }

    /// Allocates traced state and nests CanonicalizeLocaleList beneath its typed continuation.
    fn begin_intl_plural_rules_options(
        &mut self,
        site: &CallSite,
        new_target: Value,
        supported_locales: bool,
    ) -> Result<(), ExecutionError> {
        let locales = self.call_argument(site, 0)?.unwrap_or(UNDEFINED);
        let options = self.call_argument(site, 1)?.unwrap_or(UNDEFINED);
        let state = self.allocate_pending_intl_plural_rules(PendingIntlPluralRules::new(
            new_target,
            options,
            supported_locales,
        ))?;
        self.dispatch_intl_plural_rules_nested(
            NativeContinuation::intl_plural_rules(
                Self::native_site(site),
                IntlPluralRulesStage::Locales,
                Value::from_heap_ref(state.raw()),
                locales,
            ),
            |isolate| isolate.begin_intl_get_canonical_locales(site),
        )
    }

    /// Resumes locale canonicalization, an option Get, or delayed digit conversion.
    pub(crate) fn resume_intl_plural_rules(
        &mut self,
        continuation: NativeContinuation,
        stage: IntlPluralRulesStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.pending_intl_plural_rules_reference(continuation.first())?;
        if stage == IntlPluralRulesStage::Locales {
            return self.resume_intl_plural_rules_locales(continuation.site(), state, value);
        }
        if plural_rules_digit_conversion_stage(stage) {
            return self.resume_intl_plural_rules_option_primitive(
                continuation.site(),
                state,
                value,
            );
        }
        if value == UNDEFINED {
            if plural_rules_raw_digit_stage(stage) {
                self.store_plural_rules_raw_digit(state, stage, UNDEFINED)?;
            }
            return self.advance_intl_plural_rules_option(continuation.site(), state, stage);
        }
        if plural_rules_raw_digit_stage(stage) {
            self.store_plural_rules_raw_digit(state, stage, value)?;
            return self.advance_intl_plural_rules_option(continuation.site(), state, stage);
        }
        self.set_pending_intl_plural_rules_stage(state, stage)?;
        if self.is_object_value(value) {
            let consumer = if plural_rules_numeric_stage(stage) {
                ConversionConsumer::IntlPluralRulesNumberOption
            } else {
                ConversionConsumer::IntlPluralRulesStringOption
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
        self.resume_intl_plural_rules_option_primitive(continuation.site(), state, value)
    }

    /// Stores canonical locales and enters the option machine without consulting Object.prototype.
    fn resume_intl_plural_rules_locales(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlPluralRules>,
        locales: Value,
    ) -> Result<(), ExecutionError> {
        self.set_pending_intl_plural_rules_value(state, |pending| &mut pending.locales, locales)?;
        let snapshot = self.pending_intl_plural_rules_snapshot(state)?;
        if snapshot.options == UNDEFINED {
            if snapshot.supported_locales {
                return self.finish_intl_plural_rules_supported_locales(site, state);
            }
            self.update_pending_intl_plural_rules(state, |pending| {
                pending.minimum_fraction_digits = Some(0);
                pending.maximum_fraction_digits = Some(3);
            })?;
            return self.finish_intl_plural_rules_construction(site, state);
        }
        let options = self.coerce_to_object(snapshot.options)?;
        self.set_pending_intl_plural_rules_value(state, |pending| &mut pending.options, options)?;
        self.dispatch_intl_plural_rules_option_get(site, state, IntlPluralRulesStage::LocaleMatcher)
    }

    /// Continues an option after ToPrimitive with its specification-selected coercion hint.
    pub(crate) fn resume_intl_plural_rules_option_primitive(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlPluralRules>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let stage = self.pending_intl_plural_rules_stage(state)?;
        if plural_rules_numeric_stage(stage) || plural_rules_digit_conversion_stage(stage) {
            let number = numeric_value(self.convert_to_number(primitive)?)
                .ok_or(ExecutionError::InvalidIntlPluralRulesOption)?;
            return self.store_intl_plural_rules_numeric_option(site, state, stage, number);
        }
        let string = self.primitive_to_string_value(primitive)?;
        self.store_intl_plural_rules_string_option(site, state, stage, string)
    }

    /// Parses one string-valued option and advances to the next observable Get.
    fn store_intl_plural_rules_string_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlPluralRules>,
        stage: IntlPluralRulesStage,
        string: Value,
    ) -> Result<(), ExecutionError> {
        let text = self
            .intl_ascii_string(string)
            .map_err(|error| match error {
                ExecutionError::InvalidIntlCollatorOption => {
                    ExecutionError::InvalidIntlPluralRulesOption
                }
                other => other,
            })?;
        self.update_pending_intl_plural_rules(state, |pending| match stage {
            IntlPluralRulesStage::LocaleMatcher => {
                pending.locale_matcher = match text.as_ref() {
                    "lookup" => IntlLocaleMatcher::Lookup,
                    "best fit" => IntlLocaleMatcher::BestFit,
                    _ => pending.locale_matcher,
                };
            }
            IntlPluralRulesStage::Type => {
                pending.rule_type = match text.as_ref() {
                    "cardinal" => IntlPluralRuleType::Cardinal,
                    "ordinal" => IntlPluralRuleType::Ordinal,
                    _ => pending.rule_type,
                };
            }
            IntlPluralRulesStage::Notation => {
                pending.notation = match text.as_ref() {
                    "standard" => IntlNumberFormatNotation::Standard,
                    "scientific" => IntlNumberFormatNotation::Scientific,
                    "engineering" => IntlNumberFormatNotation::Engineering,
                    "compact" => IntlNumberFormatNotation::Compact,
                    _ => pending.notation,
                };
            }
            IntlPluralRulesStage::CompactDisplay => {
                pending.compact_display = match text.as_ref() {
                    "short" => IntlNumberFormatCompactDisplay::Short,
                    "long" => IntlNumberFormatCompactDisplay::Long,
                    _ => pending.compact_display,
                };
            }
            IntlPluralRulesStage::RoundingMode => {
                pending.rounding_mode = match text.as_ref() {
                    "ceil" => IntlNumberFormatRoundingMode::Ceil,
                    "floor" => IntlNumberFormatRoundingMode::Floor,
                    "expand" => IntlNumberFormatRoundingMode::Expand,
                    "trunc" => IntlNumberFormatRoundingMode::Trunc,
                    "halfCeil" => IntlNumberFormatRoundingMode::HalfCeil,
                    "halfFloor" => IntlNumberFormatRoundingMode::HalfFloor,
                    "halfExpand" => IntlNumberFormatRoundingMode::HalfExpand,
                    "halfTrunc" => IntlNumberFormatRoundingMode::HalfTrunc,
                    "halfEven" => IntlNumberFormatRoundingMode::HalfEven,
                    _ => pending.rounding_mode,
                };
            }
            IntlPluralRulesStage::RoundingPriority => {
                pending.rounding_priority = match text.as_ref() {
                    "auto" => IntlNumberFormatRoundingPriority::Auto,
                    "morePrecision" => IntlNumberFormatRoundingPriority::MorePrecision,
                    "lessPrecision" => IntlNumberFormatRoundingPriority::LessPrecision,
                    _ => pending.rounding_priority,
                };
            }
            IntlPluralRulesStage::TrailingZeroDisplay => {
                pending.trailing_zero_display = match text.as_ref() {
                    "auto" => IntlNumberFormatTrailingZeroDisplay::Auto,
                    "stripIfInteger" => IntlNumberFormatTrailingZeroDisplay::StripIfInteger,
                    _ => pending.trailing_zero_display,
                };
            }
            _ => {}
        })?;
        if !plural_rules_valid_string_option(stage, &text) {
            return Err(ExecutionError::InvalidIntlPluralRulesOption);
        }
        self.advance_intl_plural_rules_option(site, state, stage)
    }

    /// Floors and range-checks one numeric digit option without retaining a managed borrow.
    fn store_intl_plural_rules_numeric_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlPluralRules>,
        stage: IntlPluralRulesStage,
        number: f64,
    ) -> Result<(), ExecutionError> {
        let (minimum, maximum) = plural_rules_numeric_range(stage)?;
        if !number.is_finite() || number < minimum || number > maximum {
            return Err(ExecutionError::InvalidIntlPluralRulesOption);
        }
        let value = number.floor() as u16;
        if stage == IntlPluralRulesStage::RoundingIncrement
            && !valid_plural_rules_rounding_increment(value)
        {
            return Err(ExecutionError::InvalidIntlPluralRulesOption);
        }
        self.update_pending_intl_plural_rules(state, |pending| match stage {
            IntlPluralRulesStage::MinimumIntegerDigits => {
                pending.minimum_integer_digits = value as u8
            }
            IntlPluralRulesStage::RoundingIncrement => pending.rounding_increment = value,
            IntlPluralRulesStage::ConvertMinimumSignificantDigits => {
                pending.minimum_significant_digits = Some(value as u8)
            }
            IntlPluralRulesStage::ConvertMaximumSignificantDigits => {
                pending.maximum_significant_digits = Some(value as u8)
            }
            IntlPluralRulesStage::ConvertMinimumFractionDigits => {
                pending.minimum_fraction_digits = Some(value as u8)
            }
            IntlPluralRulesStage::ConvertMaximumFractionDigits => {
                pending.maximum_fraction_digits = Some(value as u8)
            }
            _ => {}
        })?;
        if plural_rules_digit_conversion_stage(stage) {
            self.advance_intl_plural_rules_digit_conversion(site, state, stage)
        } else {
            self.advance_intl_plural_rules_option(site, state, stage)
        }
    }

    /// Preserves raw digit Values until SetNumberFormatDigitOptions chooses active groups.
    fn store_plural_rules_raw_digit(
        &mut self,
        state: GcRef<PendingIntlPluralRules>,
        stage: IntlPluralRulesStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match stage {
            IntlPluralRulesStage::MinimumFractionDigits => self
                .set_pending_intl_plural_rules_value(
                    state,
                    |pending| &mut pending.minimum_fraction_raw,
                    value,
                ),
            IntlPluralRulesStage::MaximumFractionDigits => self
                .set_pending_intl_plural_rules_value(
                    state,
                    |pending| &mut pending.maximum_fraction_raw,
                    value,
                ),
            IntlPluralRulesStage::MinimumSignificantDigits => self
                .set_pending_intl_plural_rules_value(
                    state,
                    |pending| &mut pending.minimum_significant_raw,
                    value,
                ),
            IntlPluralRulesStage::MaximumSignificantDigits => self
                .set_pending_intl_plural_rules_value(
                    state,
                    |pending| &mut pending.maximum_significant_raw,
                    value,
                ),
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Advances the exact PluralRules option order and then resolves digit defaults.
    fn advance_intl_plural_rules_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlPluralRules>,
        stage: IntlPluralRulesStage,
    ) -> Result<(), ExecutionError> {
        if stage == IntlPluralRulesStage::LocaleMatcher
            && self
                .pending_intl_plural_rules_snapshot(state)?
                .supported_locales
        {
            return self.finish_intl_plural_rules_supported_locales(site, state);
        }
        if stage == IntlPluralRulesStage::TrailingZeroDisplay {
            return self.begin_intl_plural_rules_digit_conversion(site, state);
        }
        self.dispatch_intl_plural_rules_option_get(site, state, next_plural_rules_option(stage)?)
    }

    /// Selects significant and fraction digit groups after every raw property was observed.
    fn begin_intl_plural_rules_digit_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlPluralRules>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_plural_rules_snapshot(state)?;
        let has_significant = snapshot.minimum_significant_raw != UNDEFINED
            || snapshot.maximum_significant_raw != UNDEFINED;
        let has_fraction = snapshot.minimum_fraction_raw != UNDEFINED
            || snapshot.maximum_fraction_raw != UNDEFINED;
        let need_significant =
            snapshot.rounding_priority != IntlNumberFormatRoundingPriority::Auto || has_significant;
        let need_fraction = snapshot.rounding_priority != IntlNumberFormatRoundingPriority::Auto
            || (!has_significant
                && (has_fraction || snapshot.notation != IntlNumberFormatNotation::Compact));
        self.update_pending_intl_plural_rules(state, |pending| {
            pending.need_significant = need_significant;
            pending.need_fraction = need_fraction;
        })?;
        if need_significant {
            return self.dispatch_intl_plural_rules_digit_conversion(
                site,
                state,
                IntlPluralRulesStage::ConvertMinimumSignificantDigits,
            );
        }
        if need_fraction {
            return self.dispatch_intl_plural_rules_digit_conversion(
                site,
                state,
                IntlPluralRulesStage::ConvertMinimumFractionDigits,
            );
        }
        self.finish_intl_plural_rules_digit_options(site, state)
    }

    /// Converts an active raw digit option with number-hint ToPrimitive semantics.
    fn dispatch_intl_plural_rules_digit_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlPluralRules>,
        stage: IntlPluralRulesStage,
    ) -> Result<(), ExecutionError> {
        self.set_pending_intl_plural_rules_stage(state, stage)?;
        let raw =
            plural_rules_raw_digit_value(self.pending_intl_plural_rules_snapshot(state)?, stage)?;
        if raw == UNDEFINED {
            return self.advance_intl_plural_rules_digit_conversion(site, state, stage);
        }
        if self.is_object_value(raw) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlPluralRulesNumberOption,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                raw,
                site.call_site,
            );
        }
        self.resume_intl_plural_rules_option_primitive(site, state, raw)
    }

    /// Walks the selected raw digit conversion sequence and closes digit normalization.
    fn advance_intl_plural_rules_digit_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlPluralRules>,
        stage: IntlPluralRulesStage,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_plural_rules_snapshot(state)?;
        match stage {
            IntlPluralRulesStage::ConvertMinimumSignificantDigits => self
                .dispatch_intl_plural_rules_digit_conversion(
                    site,
                    state,
                    IntlPluralRulesStage::ConvertMaximumSignificantDigits,
                ),
            IntlPluralRulesStage::ConvertMaximumSignificantDigits if snapshot.need_fraction => self
                .dispatch_intl_plural_rules_digit_conversion(
                    site,
                    state,
                    IntlPluralRulesStage::ConvertMinimumFractionDigits,
                ),
            IntlPluralRulesStage::ConvertMaximumSignificantDigits => {
                self.finish_intl_plural_rules_digit_options(site, state)
            }
            IntlPluralRulesStage::ConvertMinimumFractionDigits => self
                .dispatch_intl_plural_rules_digit_conversion(
                    site,
                    state,
                    IntlPluralRulesStage::ConvertMaximumFractionDigits,
                ),
            IntlPluralRulesStage::ConvertMaximumFractionDigits => {
                self.finish_intl_plural_rules_digit_options(site, state)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Applies PluralRules digit defaults and validates incompatible rounding combinations.
    fn finish_intl_plural_rules_digit_options(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlPluralRules>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_plural_rules_snapshot(state)?;
        let significant = normalize_plural_rules_significant_digits(snapshot)?;
        let fraction = normalize_plural_rules_fraction_digits(snapshot)?;
        if snapshot.rounding_increment != 1 {
            if snapshot.need_significant
                || snapshot.rounding_priority != IntlNumberFormatRoundingPriority::Auto
            {
                return Err(ExecutionError::InvalidIntlPluralRulesOption);
            }
            if fraction.0 != fraction.1 {
                return Err(ExecutionError::InvalidIntlPluralRulesOption);
            }
        }
        self.update_pending_intl_plural_rules(state, |pending| {
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
        self.finish_intl_plural_rules_construction(site, state)
    }

    /// Performs one Proxy/accessor-aware option Get under a typed continuation.
    fn dispatch_intl_plural_rules_option_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlPluralRules>,
        stage: IntlPluralRulesStage,
    ) -> Result<(), ExecutionError> {
        self.set_pending_intl_plural_rules_stage(state, stage)?;
        let snapshot = self.pending_intl_plural_rules_snapshot(state)?;
        let key = self
            .intern_intrinsic_name(plural_rules_option_name(stage)?)?
            .into();
        let continuation = NativeContinuation::intl_plural_rules(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            snapshot.options,
        );
        match self.resolve_property_read_until_proxy(snapshot.options, key)? {
            PropertyReadResolution::Read(PropertyRead::Missing) => {
                self.resume_intl_plural_rules(continuation, stage, UNDEFINED)
            }
            PropertyReadResolution::Read(PropertyRead::Data(value)) => {
                self.resume_intl_plural_rules(continuation, stage, value)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) if getter == UNDEFINED => {
                self.resume_intl_plural_rules(continuation, stage, UNDEFINED)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => self
                .dispatch_property_callback(
                    NativeContinuation::intl_plural_rules_property_get(
                        site,
                        Value::from_heap_ref(state.raw()),
                        snapshot.options,
                    ),
                    getter,
                )
                .map(|_| ()),
            PropertyReadResolution::Proxy(_) => {
                self.dispatch_intl_plural_rules_nested(continuation, |isolate| {
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

    /// Calls the provider and allocates a branded object with newTarget-Realm fallback semantics.
    fn finish_intl_plural_rules_construction(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlPluralRules>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_plural_rules_snapshot(state)?;
        let request = self.intl_plural_rules_request(snapshot)?;
        let creation = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .create_plural_rules(request)
            .map_err(ExecutionError::IntlProvider)?;
        let prototype_atom = self.prototype_atom()?;
        let default_prototype = self
            .realm
            .intl_plural_rules_prototype
            .expect("Intl.PluralRules prototype initializes before construction");
        let prototype = self
            .constructor_prototype_value(snapshot.new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .or_else(|| {
                self.realm_for_callable(snapshot.new_target)
                    .ok()
                    .and_then(|realm| {
                        self.realm_intrinsic_prototype(
                            realm,
                            IntrinsicPrototypeKind::IntlPluralRules,
                        )
                    })
            })
            .unwrap_or(default_prototype);
        let object =
            self.allocate_intl_plural_rules_object(creation, prototype, AllocationSpace::Young)?;
        self.write(site.caller_base, site.destination, object)
    }

    /// Filters canonical requested locales and publishes a fresh intrinsic Array.
    fn finish_intl_plural_rules_supported_locales(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlPluralRules>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_plural_rules_snapshot(state)?;
        let locales = self.intl_plural_rules_locale_strings(snapshot.locales)?;
        let supported = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .plural_rules_supported_locales(&locales, snapshot.locale_matcher)
            .map_err(ExecutionError::IntlProvider)?;
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.PluralRules"),
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

    /// Brands the receiver before starting the resumable ToNumber argument conversion.
    pub(crate) fn begin_intl_plural_rules_select(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.intl_plural_rules_reference(site.this_value)?;
        let value = self.call_argument(site, 0)?.unwrap_or(UNDEFINED);
        if self.is_object_value(value) {
            let mut pending = PendingIntlPluralRules::new(site.this_value, UNDEFINED, false);
            pending.stage = IntlPluralRulesStage::Select;
            let state = self.allocate_pending_intl_plural_rules(pending)?;
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlPluralRulesValue,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_intl_plural_rules_select(Self::native_site(site), site.this_value, value)
    }

    /// Continues `select` after an object argument yielded its primitive Number input.
    pub(crate) fn resume_intl_plural_rules_value_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlPluralRules>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let receiver = self.pending_intl_plural_rules_snapshot(state)?.new_target;
        self.finish_intl_plural_rules_select(site, receiver, primitive)
    }

    /// Converts one primitive with ToNumber and asks the immutable provider payload for a category.
    fn finish_intl_plural_rules_select(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let object = self.intl_plural_rules_reference(receiver)?;
        let input = self.intl_plural_rules_mathematical_value(value)?;
        let payload = self.intl_plural_rules_snapshot(object)?.payload;
        let category = self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(payload, self.types.intl_plural_rules_payload)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .backend
                    .select(&input)
                    .map_err(ExecutionError::IntlProvider)
            })
        })?;
        let result = self.allocate_runtime_string(
            JsString::try_from_latin1(intl_plural_category_name(category))
                .map_err(ExecutionError::PropertyKeyString)?,
        )?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Keeps the v3 method surface explicit until the provider ABI gains plural-range data.
    pub(crate) fn call_intl_plural_rules_select_range(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.intl_plural_rules_reference(site.this_value)?;
        Err(ExecutionError::InvalidIntlPluralRulesOption)
    }

    /// Publishes a fresh resolved-options record in ECMA-402 property order.
    pub(crate) fn call_intl_plural_rules_resolved_options(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let object = self.intl_plural_rules_reference(site.this_value)?;
        let resolved = self.intl_plural_rules_resolved(object)?;
        let result = self.create_ordinary_object()?;
        self.write(site.caller_base, site.destination, result)?;
        self.set_intl_plural_rules_string(result, b"locale", resolved.locale.as_bytes())?;
        self.set_intl_plural_rules_string(
            result,
            b"type",
            match resolved.rule_type {
                IntlPluralRuleType::Cardinal => b"cardinal",
                IntlPluralRuleType::Ordinal => b"ordinal",
            },
        )?;
        self.set_intl_plural_rules_string(
            result,
            b"notation",
            intl_plural_rules_notation_name(resolved.options.notation),
        )?;
        self.set_intl_plural_rules_number(
            result,
            b"minimumIntegerDigits",
            u32::from(resolved.options.minimum_integer_digits),
        )?;
        if let Some(value) = resolved.options.minimum_fraction_digits {
            self.set_intl_plural_rules_number(result, b"minimumFractionDigits", u32::from(value))?;
        }
        if let Some(value) = resolved.options.maximum_fraction_digits {
            self.set_intl_plural_rules_number(result, b"maximumFractionDigits", u32::from(value))?;
        }
        if let Some(value) = resolved.options.minimum_significant_digits {
            self.set_intl_plural_rules_number(
                result,
                b"minimumSignificantDigits",
                u32::from(value),
            )?;
        }
        if let Some(value) = resolved.options.maximum_significant_digits {
            self.set_intl_plural_rules_number(
                result,
                b"maximumSignificantDigits",
                u32::from(value),
            )?;
        }
        self.set_intl_plural_rules_categories(result, &resolved.categories)?;
        self.set_intl_plural_rules_number(
            result,
            b"roundingIncrement",
            u32::from(resolved.options.rounding_increment),
        )?;
        self.set_intl_plural_rules_string(
            result,
            b"roundingMode",
            intl_plural_rules_rounding_mode_name(resolved.options.rounding_mode),
        )?;
        self.set_intl_plural_rules_string(
            result,
            b"roundingPriority",
            intl_plural_rules_rounding_priority_name(resolved.options.rounding_priority),
        )?;
        self.set_intl_plural_rules_string(
            result,
            b"trailingZeroDisplay",
            intl_plural_rules_trailing_zero_name(resolved.options.trailing_zero_display),
        )?;
        if resolved.options.notation == IntlNumberFormatNotation::Compact {
            self.set_intl_plural_rules_string(
                result,
                b"compactDisplay",
                match resolved.options.compact_display {
                    IntlNumberFormatCompactDisplay::Short => b"short",
                    IntlNumberFormatCompactDisplay::Long => b"long",
                },
            )?;
        }
        Ok(())
    }

    /// Copies managed locale strings and scalar options into the provider-neutral request.
    fn intl_plural_rules_request(
        &mut self,
        snapshot: PendingIntlPluralRules,
    ) -> Result<IntlPluralRulesRequest, ExecutionError> {
        Ok(IntlPluralRulesRequest {
            locales: self.intl_plural_rules_locale_strings(snapshot.locales)?,
            locale_matcher: snapshot.locale_matcher,
            rule_type: snapshot.rule_type,
            options: IntlNumberFormatOptions {
                style: IntlNumberFormatStyle::Decimal,
                currency: None,
                currency_display: IntlNumberFormatCurrencyDisplay::Symbol,
                currency_sign: IntlNumberFormatCurrencySign::Standard,
                unit: None,
                unit_display: IntlNumberFormatUnitDisplay::Short,
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
                use_grouping: IntlNumberFormatUseGrouping::Auto,
                sign_display: IntlNumberFormatSignDisplay::Auto,
            },
        })
    }

    /// Converts canonical locale Array elements into provider-owned ASCII strings.
    fn intl_plural_rules_locale_strings(
        &mut self,
        value: Value,
    ) -> Result<Box<[Box<str>]>, ExecutionError> {
        let values = self.copy_packed_intl_array(value)?;
        let mut locales = Vec::new();
        locales
            .try_reserve_exact(values.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for locale in values {
            locales.push(self.intl_ascii_string(locale)?);
        }
        Ok(locales.into_boxed_slice())
    }

    /// Converts a primitive through ToNumber while preserving all non-finite categories.
    fn intl_plural_rules_mathematical_value(
        &mut self,
        value: Value,
    ) -> Result<IntlMathematicalValue, ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        if number.is_nan() {
            return Ok(IntlMathematicalValue::NaN);
        }
        if number == 0.0 && number.is_sign_negative() {
            return Ok(IntlMathematicalValue::NegativeZero);
        }
        if number == f64::INFINITY {
            return Ok(IntlMathematicalValue::PositiveInfinity);
        }
        if number == f64::NEG_INFINITY {
            return Ok(IntlMathematicalValue::NegativeInfinity);
        }
        let string = self.number_to_string(Value::from_f64(number), None)?;
        let units = self.string_value_to_utf16(string)?;
        let value = String::from_utf16(&units)
            .map(String::into_boxed_str)
            .map_err(|_| ExecutionError::NumberFormatInvalidDigit)?;
        Ok(IntlMathematicalValue::Finite(value))
    }

    /// Materializes the provider category list without observable Array prototype calls.
    fn set_intl_plural_rules_categories(
        &mut self,
        result: Value,
        categories: &[IntlPluralCategory],
    ) -> Result<(), ExecutionError> {
        let array = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.PluralRules"),
        )?;
        let key = self.intern_intrinsic_name(b"pluralCategories")?;
        self.set_own_data_property(result, key, array)?;
        for (index, category) in categories.iter().copied().enumerate() {
            let (value, array) = self.allocate_runtime_string_retaining(
                JsString::try_from_latin1(intl_plural_category_name(category))
                    .map_err(ExecutionError::PropertyKeyString)?,
                array,
            )?;
            let key = self.property_key_atom(safe_integer_value(
                u64::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?,
            ))?;
            self.set_own_data_property(array, key, value)?;
        }
        Ok(())
    }

    fn set_intl_plural_rules_string(
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

    fn set_intl_plural_rules_number(
        &mut self,
        result: Value,
        name: &[u8],
        value: u32,
    ) -> Result<(), ExecutionError> {
        let key = self.intern_intrinsic_name(name)?;
        self.set_own_data_property(result, key, safe_integer_value(u64::from(value)))
    }

    fn intl_plural_rules_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<IntlPluralRulesObject>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::IncompatibleIntlPluralRulesReceiver(value))?;
        self.heap
            .checked_reference(raw, self.types.intl_plural_rules_object)
            .map_err(|_| ExecutionError::IncompatibleIntlPluralRulesReceiver(value))
    }

    fn intl_plural_rules_snapshot(
        &mut self,
        object: GcRef<IntlPluralRulesObject>,
    ) -> Result<IntlPluralRulesObject, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(object, self.types.intl_plural_rules_object)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn intl_plural_rules_resolved(
        &mut self,
        object: GcRef<IntlPluralRulesObject>,
    ) -> Result<IntlPluralRulesResolved, ExecutionError> {
        let payload = self.intl_plural_rules_snapshot(object)?.payload;
        self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(payload, self.types.intl_plural_rules_payload)
                    .map(|payload| payload.resolved.clone())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Drains synchronous nested operations while preserving the typed parent continuation.
    fn dispatch_intl_plural_rules_nested(
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
        let NativeContinuationKind::IntlPluralRules(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_intl_plural_rules(continuation, stage, value)
    }
}
