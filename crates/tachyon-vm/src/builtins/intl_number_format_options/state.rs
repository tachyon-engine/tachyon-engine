//! Traced pending state for resumable NumberFormat construction.

use super::*;

/// GC-managed constructor state retained across property getters and primitive conversions.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingIntlNumberFormat {
    pub(crate) new_target: Value,
    pub(crate) legacy_receiver: Value,
    pub(crate) format_value: Value,
    pub(crate) options: Value,
    pub(crate) locales: Value,
    pub(crate) numbering_system: Value,
    pub(crate) currency: Value,
    pub(crate) unit: Value,
    pub(crate) minimum_fraction_raw: Value,
    pub(crate) maximum_fraction_raw: Value,
    pub(crate) minimum_significant_raw: Value,
    pub(crate) maximum_significant_raw: Value,
    pub(crate) style: IntlNumberFormatStyle,
    pub(crate) currency_display: IntlNumberFormatCurrencyDisplay,
    pub(crate) currency_sign: IntlNumberFormatCurrencySign,
    pub(crate) unit_display: IntlNumberFormatUnitDisplay,
    pub(crate) notation: IntlNumberFormatNotation,
    pub(crate) compact_display: IntlNumberFormatCompactDisplay,
    pub(crate) use_grouping: IntlNumberFormatUseGrouping,
    pub(crate) sign_display: IntlNumberFormatSignDisplay,
    pub(crate) rounding_mode: IntlNumberFormatRoundingMode,
    pub(crate) rounding_priority: IntlNumberFormatRoundingPriority,
    pub(crate) trailing_zero_display: IntlNumberFormatTrailingZeroDisplay,
    pub(crate) locale_matcher: IntlLocaleMatcher,
    pub(crate) minimum_integer_digits: u8,
    pub(crate) minimum_fraction_digits: Option<u8>,
    pub(crate) maximum_fraction_digits: Option<u8>,
    pub(crate) minimum_significant_digits: Option<u8>,
    pub(crate) maximum_significant_digits: Option<u8>,
    pub(crate) rounding_increment: u16,
    pub(crate) need_fraction: bool,
    pub(crate) need_significant: bool,
    pub(crate) stage: IntlNumberFormatStage,
    pub(crate) supported_locales: bool,
}

impl Trace for PendingIntlNumberFormat {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.new_target.trace(tracer);
        self.legacy_receiver.trace(tracer);
        self.format_value.trace(tracer);
        self.options.trace(tracer);
        self.locales.trace(tracer);
        self.numbering_system.trace(tracer);
        self.currency.trace(tracer);
        self.unit.trace(tracer);
        self.minimum_fraction_raw.trace(tracer);
        self.maximum_fraction_raw.trace(tracer);
        self.minimum_significant_raw.trace(tracer);
        self.maximum_significant_raw.trace(tracer);
    }
}

pub(super) struct PendingIntlNumberFormatRoots<'a> {
    pub(super) vm: VmRoots<'a>,
    pub(super) pending: PendingIntlNumberFormat,
}

impl Trace for PendingIntlNumberFormatRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl PendingIntlNumberFormat {
    /// Creates a scalar-only default record with every managed field explicitly undefined.
    pub(super) fn new(new_target: Value, options: Value, supported_locales: bool) -> Self {
        Self {
            new_target,
            legacy_receiver: UNDEFINED,
            format_value: UNDEFINED,
            options,
            locales: UNDEFINED,
            numbering_system: UNDEFINED,
            currency: UNDEFINED,
            unit: UNDEFINED,
            minimum_fraction_raw: UNDEFINED,
            maximum_fraction_raw: UNDEFINED,
            minimum_significant_raw: UNDEFINED,
            maximum_significant_raw: UNDEFINED,
            style: IntlNumberFormatStyle::Decimal,
            currency_display: IntlNumberFormatCurrencyDisplay::Symbol,
            currency_sign: IntlNumberFormatCurrencySign::Standard,
            unit_display: IntlNumberFormatUnitDisplay::Short,
            notation: IntlNumberFormatNotation::Standard,
            compact_display: IntlNumberFormatCompactDisplay::Short,
            use_grouping: IntlNumberFormatUseGrouping::Auto,
            sign_display: IntlNumberFormatSignDisplay::Auto,
            rounding_mode: IntlNumberFormatRoundingMode::HalfExpand,
            rounding_priority: IntlNumberFormatRoundingPriority::Auto,
            trailing_zero_display: IntlNumberFormatTrailingZeroDisplay::Auto,
            locale_matcher: IntlLocaleMatcher::BestFit,
            minimum_integer_digits: 1,
            minimum_fraction_digits: None,
            maximum_fraction_digits: None,
            minimum_significant_digits: None,
            maximum_significant_digits: None,
            rounding_increment: 1,
            need_fraction: true,
            need_significant: false,
            stage: IntlNumberFormatStage::Locales,
            supported_locales,
        }
    }
}
