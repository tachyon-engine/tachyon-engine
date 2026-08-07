//! Host-owned capabilities used by ECMAScript builtins without platform access in engine core.

use core::{
    cmp::Ordering,
    fmt,
    num::NonZeroUsize,
    task::{Context, Poll},
    time::Duration,
};

use crate::SharedArrayBufferHandle;

/// Stable provider failure code suitable for Rust and FFI adapters without allocating messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostProviderError {
    Unavailable,
    Failure(u32),
}

/// Process-local identity for one shared backing store without exposing its representation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SharedMemoryId(NonZeroUsize);

impl SharedMemoryId {
    pub(crate) fn from_address(address: usize) -> Self {
        Self(NonZeroUsize::new(address).expect("Arc backing addresses are non-zero"))
    }
}

/// Waiter-list key shared by every isolate in one host-defined agent cluster.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AtomicsWaitLocation {
    memory: SharedMemoryId,
    byte_offset: usize,
}

impl AtomicsWaitLocation {
    pub(crate) const fn new(memory: SharedMemoryId, byte_offset: usize) -> Self {
        Self {
            memory,
            byte_offset,
        }
    }

    #[must_use]
    pub const fn memory(self) -> SharedMemoryId {
        self.memory
    }

    #[must_use]
    pub const fn byte_offset(self) -> usize {
        self.byte_offset
    }
}

/// Stable synchronous result returned by an injected Atomics waiter implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtomicsWaitResult {
    Ok,
    NotEqual,
    TimedOut,
}

/// One host-owned asynchronous wait operation polled only by its originating isolate.
///
/// Implementations may retain synchronization handles and wakers, but must never retain
/// isolate-local `Value` or `GcRef` data. Dropping the operation must unregister any waiter that
/// has not already completed.
pub trait AtomicsAsyncWait: Send {
    fn poll(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<AtomicsWaitResult, HostProviderError>>;
}

/// Result of atomically comparing and optionally publishing one asynchronous waiter.
pub enum AtomicsAsyncWaitStart {
    Immediate(AtomicsWaitResult),
    Pending(Box<dyn AtomicsAsyncWait>),
}

/// Engine-neutral scalar accompanying one Test262-style SharedArrayBuffer broadcast.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentBroadcastValue {
    Undefined,
    Int32(i32),
    BigInt(Box<[u16]>),
}

/// Owned message transferred from an agent-cluster provider into one isolate.
#[derive(Clone, Debug)]
pub struct AgentBroadcast {
    pub buffer: SharedArrayBufferHandle,
    pub value: AgentBroadcastValue,
}

/// Host-owned waiter registry and parking capability for one ECMAScript agent cluster.
///
/// `wait` must serialize `condition` and waiter publication against `notify` for the same
/// location. The provider owns blocking, clocks, threads, and wakeup primitives; the engine core
/// only supplies a short comparison callback that cannot execute JavaScript or allocate.
pub trait AtomicsWaiterProvider: Send {
    fn notify(
        &mut self,
        location: AtomicsWaitLocation,
        count: u64,
    ) -> Result<u64, HostProviderError>;

    fn wait(
        &mut self,
        location: AtomicsWaitLocation,
        timeout: Option<Duration>,
        condition: &mut dyn FnMut() -> Result<bool, HostProviderError>,
    ) -> Result<AtomicsWaitResult, HostProviderError>;

    /// Compares and publishes without blocking the isolate thread.
    ///
    /// The provider must serialize the supplied condition and waiter publication against
    /// `notify` exactly as for synchronous `wait`. The default keeps existing embedders source
    /// compatible while making missing asynchronous support explicit.
    fn wait_async(
        &mut self,
        _location: AtomicsWaitLocation,
        _timeout: Option<Duration>,
        _condition: &mut dyn FnMut() -> Result<bool, HostProviderError>,
    ) -> Result<AtomicsAsyncWaitStart, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }
}

/// Host-owned lifecycle and coordination capability for one ECMAScript agent cluster.
///
/// The engine transfers only owned UTF-16 strings and opaque SharedArrayBuffer handles across
/// this boundary. Thread creation, blocking, clocks, cancellation, and worker joining remain
/// entirely host-owned.
pub trait AgentHostProvider: Send {
    fn start(&mut self, source: Box<[u16]>) -> Result<(), HostProviderError>;

    fn broadcast(&mut self, message: AgentBroadcast) -> Result<(), HostProviderError>;

    fn receive_broadcast(&mut self) -> Result<AgentBroadcast, HostProviderError>;

    fn report(&mut self, message: Box<[u16]>) -> Result<(), HostProviderError>;

    fn get_report(&mut self) -> Result<Option<Box<[u16]>>, HostProviderError>;

    fn sleep(&mut self, milliseconds: f64) -> Result<(), HostProviderError>;

    fn monotonic_now(&mut self) -> Result<f64, HostProviderError>;

    fn leaving(&mut self) -> Result<(), HostProviderError>;
}

/// Supplies Unix wall-clock milliseconds for `Date` without exposing `std::time` to the VM.
pub trait WallClockProvider: Send {
    fn unix_time_milliseconds(&mut self) -> Result<i64, HostProviderError>;
}

/// Supplies ECMAScript-compatible local-time conversions without filesystem or locale access.
pub trait TimeZoneProvider: Send {
    /// Returns the canonical identifier selected as the embedding's default time zone.
    fn default_time_zone_identifier(&mut self) -> Result<Box<str>, HostProviderError>;

    /// Returns the local offset applied to UTC; values must remain within one civil day.
    fn offset_milliseconds_for_utc(
        &mut self,
        utc_milliseconds: i64,
    ) -> Result<i64, HostProviderError>;

    /// Resolves a local wall-time value to UTC using ECMAScript gap/overlap disambiguation.
    fn utc_milliseconds_for_local(
        &mut self,
        local_milliseconds: i64,
    ) -> Result<i64, HostProviderError>;

    /// Returns the offset for an explicitly requested canonical zone identifier.
    fn offset_milliseconds_for_utc_in_zone(
        &mut self,
        identifier: &str,
        utc_milliseconds: i64,
    ) -> Result<i64, HostProviderError> {
        let default = self.default_time_zone_identifier()?;
        if !default.eq_ignore_ascii_case(identifier) {
            return Err(HostProviderError::Unavailable);
        }
        self.offset_milliseconds_for_utc(utc_milliseconds)
    }
}

/// Provider-owned data collections exposed by `Intl.supportedValuesOf`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntlSupportedValuesKey {
    Calendar,
    Collation,
    Currency,
    NumberingSystem,
    TimeZone,
    Unit,
}

/// Locale-selection algorithm requested by an Intl service constructor.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IntlLocaleMatcher {
    Lookup,
    #[default]
    BestFit,
}

/// Collator operation selected by the ECMAScript `usage` option.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlCollatorUsage {
    #[default]
    Sort,
    Search,
}

/// Strength exposed through `Intl.Collator.prototype.resolvedOptions`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlCollatorSensitivity {
    Base,
    Accent,
    Case,
    Variant,
}

/// Case ordering exposed through the optional `kf` extension key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlCollatorCaseFirst {
    Upper,
    Lower,
    False,
}

/// Fully converted, owned request passed from the VM to one Collator adapter.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct IntlCollatorRequest {
    pub locales: Box<[Box<str>]>,
    pub locale_matcher: IntlLocaleMatcher,
    pub usage: IntlCollatorUsage,
    pub collation: Option<Box<str>>,
    pub numeric: Option<bool>,
    pub case_first: Option<IntlCollatorCaseFirst>,
    pub sensitivity: Option<IntlCollatorSensitivity>,
    pub ignore_punctuation: Option<bool>,
}

/// Provider-resolved immutable slots stored by one initialized Collator object.
#[derive(Debug, Eq, PartialEq)]
pub struct IntlCollatorResolved {
    pub locale: Box<str>,
    pub usage: IntlCollatorUsage,
    pub sensitivity: IntlCollatorSensitivity,
    pub ignore_punctuation: bool,
    pub collation: Box<str>,
    pub numeric: bool,
    pub case_first: IntlCollatorCaseFirst,
}

/// Opaque compiled collation state retained by a GC-owned external payload.
pub trait IntlCollatorBackend: Send {
    /// Compares potentially ill-formed ECMAScript UTF-16 without allocating managed data.
    fn compare_utf16(&self, left: &[u16], right: &[u16]) -> Result<Ordering, HostProviderError>;

    /// Reports only heap backing retained beyond the boxed trait object itself.
    fn external_memory_bytes(&self) -> usize;
}

/// One resolved Collator snapshot paired with its reusable compiled backend.
pub struct IntlCollatorCreation {
    pub resolved: IntlCollatorResolved,
    pub backend: Box<dyn IntlCollatorBackend>,
}

/// Semantic list type selected by `Intl.ListFormat`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlListFormatType {
    #[default]
    Conjunction,
    Disjunction,
    Unit,
}

/// Pattern width selected by `Intl.ListFormat`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlListFormatStyle {
    #[default]
    Long,
    Short,
    Narrow,
}

/// Fully converted constructor request passed to the list-format provider.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct IntlListFormatRequest {
    pub locales: Box<[Box<str>]>,
    pub locale_matcher: IntlLocaleMatcher,
    pub list_type: IntlListFormatType,
    pub style: IntlListFormatStyle,
}

/// Immutable slots stored by one initialized ListFormat object.
#[derive(Debug, Eq, PartialEq)]
pub struct IntlListFormatResolved {
    pub locale: Box<str>,
    pub list_type: IntlListFormatType,
    pub style: IntlListFormatStyle,
}

/// Part classification emitted directly by a list-format provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlListFormatPartType {
    Element,
    Literal,
}

/// One gap-free UTF-16 span in a formatted list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntlListFormatPartSpan {
    pub kind: IntlListFormatPartType,
    pub start: u32,
    pub end: u32,
}

/// Provider-owned formatted UTF-16 list and its ordered semantic spans.
#[derive(Debug, Eq, PartialEq)]
pub struct IntlFormattedListParts {
    pub formatted: Box<[u16]>,
    pub spans: Box<[IntlListFormatPartSpan]>,
}

/// High-level presentation style selected by `Intl.NumberFormat`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlNumberFormatStyle {
    #[default]
    Decimal,
    Percent,
    Currency,
    Unit,
}

/// Currency token presentation selected for currency formatting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlNumberFormatCurrencyDisplay {
    Code,
    #[default]
    Symbol,
    NarrowSymbol,
    Name,
}

/// Sign convention selected for negative currency values.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlNumberFormatCurrencySign {
    #[default]
    Standard,
    Accounting,
}

/// Unit name width selected for unit formatting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlNumberFormatUnitDisplay {
    #[default]
    Short,
    Narrow,
    Long,
}

/// Numeric notation selected before digit rounding and localized rendering.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlNumberFormatNotation {
    #[default]
    Standard,
    Scientific,
    Engineering,
    Compact,
}

/// Compact-notation suffix width.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlNumberFormatCompactDisplay {
    #[default]
    Short,
    Long,
}

/// Locale grouping policy after boolean/string option normalization.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlNumberFormatUseGrouping {
    Never,
    Min2,
    #[default]
    Auto,
    Always,
}

/// Sign visibility selected after applying numeric rounding.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlNumberFormatSignDisplay {
    #[default]
    Auto,
    Never,
    Always,
    ExceptZero,
    Negative,
}

/// Rounding direction used by FormatNumericToString.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlNumberFormatRoundingMode {
    Ceil,
    Floor,
    Expand,
    Trunc,
    HalfCeil,
    HalfFloor,
    #[default]
    HalfExpand,
    HalfTrunc,
    HalfEven,
}

/// Conflict policy when both fraction and significant digit constraints are present.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlNumberFormatRoundingPriority {
    #[default]
    Auto,
    MorePrecision,
    LessPrecision,
}

/// Whether an all-zero fractional suffix is retained after rounding.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlNumberFormatTrailingZeroDisplay {
    #[default]
    Auto,
    StripIfInteger,
}

/// Fully converted NumberFormat option slots with no isolate-local or ICU-specific values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntlNumberFormatOptions {
    pub style: IntlNumberFormatStyle,
    pub currency: Option<Box<str>>,
    pub currency_display: IntlNumberFormatCurrencyDisplay,
    pub currency_sign: IntlNumberFormatCurrencySign,
    pub unit: Option<Box<str>>,
    pub unit_display: IntlNumberFormatUnitDisplay,
    pub minimum_integer_digits: u8,
    pub minimum_fraction_digits: Option<u8>,
    pub maximum_fraction_digits: Option<u8>,
    pub minimum_significant_digits: Option<u8>,
    pub maximum_significant_digits: Option<u8>,
    pub rounding_increment: u16,
    pub rounding_mode: IntlNumberFormatRoundingMode,
    pub rounding_priority: IntlNumberFormatRoundingPriority,
    pub trailing_zero_display: IntlNumberFormatTrailingZeroDisplay,
    pub notation: IntlNumberFormatNotation,
    pub compact_display: IntlNumberFormatCompactDisplay,
    pub use_grouping: IntlNumberFormatUseGrouping,
    pub sign_display: IntlNumberFormatSignDisplay,
}

impl Default for IntlNumberFormatOptions {
    fn default() -> Self {
        Self {
            style: IntlNumberFormatStyle::Decimal,
            currency: None,
            currency_display: IntlNumberFormatCurrencyDisplay::Symbol,
            currency_sign: IntlNumberFormatCurrencySign::Standard,
            unit: None,
            unit_display: IntlNumberFormatUnitDisplay::Short,
            minimum_integer_digits: 1,
            minimum_fraction_digits: Some(0),
            maximum_fraction_digits: Some(3),
            minimum_significant_digits: None,
            maximum_significant_digits: None,
            rounding_increment: 1,
            rounding_mode: IntlNumberFormatRoundingMode::HalfExpand,
            rounding_priority: IntlNumberFormatRoundingPriority::Auto,
            trailing_zero_display: IntlNumberFormatTrailingZeroDisplay::Auto,
            notation: IntlNumberFormatNotation::Standard,
            compact_display: IntlNumberFormatCompactDisplay::Short,
            use_grouping: IntlNumberFormatUseGrouping::Auto,
            sign_display: IntlNumberFormatSignDisplay::Auto,
        }
    }
}

/// NumberFormat locale negotiation plus the already converted option record.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct IntlNumberFormatRequest {
    pub locales: Box<[Box<str>]>,
    pub locale_matcher: IntlLocaleMatcher,
    pub numbering_system: Option<Box<str>>,
    pub options: IntlNumberFormatOptions,
}

/// Provider-normalized NumberFormat slots published through `resolvedOptions`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntlNumberFormatResolved {
    pub locale: Box<str>,
    pub numbering_system: Box<str>,
    pub options: IntlNumberFormatOptions,
}

/// Engine-neutral mathematical input retaining exact decimal spelling for BigInt and strings.
#[derive(Clone, Debug, PartialEq)]
pub enum IntlMathematicalValue {
    Finite(Box<str>),
    NegativeZero,
    PositiveInfinity,
    NegativeInfinity,
    NaN,
}

/// Provider-neutral field classification exposed by NumberFormat formatted-parts APIs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlNumberFormatPartType {
    Literal,
    Nan,
    Infinity,
    Integer,
    Group,
    Decimal,
    Fraction,
    PlusSign,
    MinusSign,
    PercentSign,
    Currency,
    Unit,
    ExponentSeparator,
    ExponentMinusSign,
    ExponentInteger,
    Compact,
    ApproximatelySign,
}

/// One UTF-16 code-unit range in a provider-owned formatted number buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntlNumberFormatPartSpan {
    pub kind: IntlNumberFormatPartType,
    pub start: u32,
    pub end: u32,
}

/// One formatted number and the ordered, gap-free fields that partition it.
#[derive(Debug, Eq, PartialEq)]
pub struct IntlFormattedNumberParts {
    pub formatted: Box<[u16]>,
    pub spans: Box<[IntlNumberFormatPartSpan]>,
}

/// Opaque compiled number-formatting state retained by a GC external payload.
pub trait IntlNumberFormatBackend: Send {
    /// Formats one already converted mathematical value into owned UTF-16 code units.
    fn format(&self, value: &IntlMathematicalValue) -> Result<Box<[u16]>, HostProviderError>;

    /// Formats one mathematical value and classifies every emitted UTF-16 code unit.
    fn format_to_parts(
        &self,
        value: &IntlMathematicalValue,
    ) -> Result<IntlFormattedNumberParts, HostProviderError>;

    /// Reports only heap backing retained beyond the boxed trait object itself.
    fn external_memory_bytes(&self) -> usize;
}

/// One resolved NumberFormat snapshot paired with its reusable compiled backend.
pub struct IntlNumberFormatCreation {
    pub resolved: IntlNumberFormatResolved,
    pub backend: Box<dyn IntlNumberFormatBackend>,
}

/// Cardinal or ordinal rule family selected by `Intl.PluralRules`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlPluralRuleType {
    #[default]
    Cardinal,
    Ordinal,
}

/// Closed ECMA-402 plural category set in specification order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlPluralCategory {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

/// Provider-neutral constructor inputs after all observable option conversion has completed.
#[derive(Debug, Eq, PartialEq)]
pub struct IntlPluralRulesRequest {
    pub locales: Box<[Box<str>]>,
    pub locale_matcher: IntlLocaleMatcher,
    pub rule_type: IntlPluralRuleType,
    pub options: IntlNumberFormatOptions,
}

/// Resolved scalar slots published by `Intl.PluralRules.prototype.resolvedOptions`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntlPluralRulesResolved {
    pub locale: Box<str>,
    pub rule_type: IntlPluralRuleType,
    pub options: IntlNumberFormatOptions,
    pub categories: Box<[IntlPluralCategory]>,
}

/// Opaque compiled plural data retained by one branded PluralRules object.
pub trait IntlPluralRulesBackend: Send {
    /// Selects one category after applying the object's digit and notation options.
    fn select(
        &self,
        value: &IntlMathematicalValue,
    ) -> Result<IntlPluralCategory, HostProviderError>;

    /// Reports only heap backing retained beyond the boxed trait object itself.
    fn external_memory_bytes(&self) -> usize;
}

/// One resolved PluralRules snapshot paired with reusable compiled CLDR state.
pub struct IntlPluralRulesCreation {
    pub resolved: IntlPluralRulesResolved,
    pub backend: Box<dyn IntlPluralRulesBackend>,
}

/// Width selected by `Intl.RelativeTimeFormat`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlRelativeTimeFormatStyle {
    #[default]
    Long,
    Short,
    Narrow,
}

/// Whether lexical relative phrases may replace numeric patterns.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlRelativeTimeFormatNumeric {
    #[default]
    Always,
    Auto,
}

/// Canonical singular relative-time unit set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlRelativeTimeUnit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

/// Locale negotiation and already converted RelativeTimeFormat options.
#[derive(Debug, Eq, PartialEq)]
pub struct IntlRelativeTimeFormatRequest {
    pub locales: Box<[Box<str>]>,
    pub locale_matcher: IntlLocaleMatcher,
    pub numbering_system: Option<Box<str>>,
    pub style: IntlRelativeTimeFormatStyle,
    pub numeric: IntlRelativeTimeFormatNumeric,
}

/// Resolved scalar slots published through `resolvedOptions`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntlRelativeTimeFormatResolved {
    pub locale: Box<str>,
    pub numbering_system: Box<str>,
    pub style: IntlRelativeTimeFormatStyle,
    pub numeric: IntlRelativeTimeFormatNumeric,
}

/// One formatted RelativeTimeFormat span; number spans retain their canonical unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntlRelativeTimePartSpan {
    pub kind: IntlNumberFormatPartType,
    pub start: u32,
    pub end: u32,
    pub has_unit: bool,
}

/// Gap-free provider-owned RelativeTimeFormat output.
#[derive(Debug, Eq, PartialEq)]
pub struct IntlFormattedRelativeTimeParts {
    pub formatted: Box<[u16]>,
    pub spans: Box<[IntlRelativeTimePartSpan]>,
}

/// Opaque locale-pattern, number-format, and plural-rule state retained by one object.
pub trait IntlRelativeTimeFormatBackend: Send {
    fn format(
        &self,
        value: &IntlMathematicalValue,
        unit: IntlRelativeTimeUnit,
    ) -> Result<Box<[u16]>, HostProviderError>;

    fn format_to_parts(
        &self,
        value: &IntlMathematicalValue,
        unit: IntlRelativeTimeUnit,
    ) -> Result<IntlFormattedRelativeTimeParts, HostProviderError>;

    fn external_memory_bytes(&self) -> usize;
}

/// One resolved RelativeTimeFormat snapshot paired with its reusable provider backend.
pub struct IntlRelativeTimeFormatCreation {
    pub resolved: IntlRelativeTimeFormatResolved,
    pub backend: Box<dyn IntlRelativeTimeFormatBackend>,
}

/// Locale-sensitive width shared by textual date-time fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlDateTimeTextStyle {
    Long,
    Short,
    Narrow,
}

/// Width used by numeric date-time fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlDateTimeNumericStyle {
    Numeric,
    TwoDigit,
}

/// Month formatting width, including both numeric and textual forms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlDateTimeMonthStyle {
    Numeric,
    TwoDigit,
    Long,
    Short,
    Narrow,
}

/// Preset date/time style selected by the ECMA-402 style options.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlDateTimeStyle {
    Full,
    Long,
    Medium,
    Short,
}

/// Resolved hour cycle used by a DateTimeFormat backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlDateTimeHourCycle {
    H11,
    H12,
    H23,
    H24,
}

/// Width of the locale-sensitive time-zone name field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlDateTimeZoneNameStyle {
    Long,
    Short,
    ShortOffset,
    LongOffset,
    ShortGeneric,
    LongGeneric,
}

/// Already converted component and preset options for DateTimeFormat creation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IntlDateTimeFormatOptions {
    pub weekday: Option<IntlDateTimeTextStyle>,
    pub era: Option<IntlDateTimeTextStyle>,
    pub year: Option<IntlDateTimeNumericStyle>,
    pub month: Option<IntlDateTimeMonthStyle>,
    pub day: Option<IntlDateTimeNumericStyle>,
    pub day_period: Option<IntlDateTimeTextStyle>,
    pub hour: Option<IntlDateTimeNumericStyle>,
    pub minute: Option<IntlDateTimeNumericStyle>,
    pub second: Option<IntlDateTimeNumericStyle>,
    pub fractional_second_digits: Option<u8>,
    pub time_zone_name: Option<IntlDateTimeZoneNameStyle>,
    pub date_style: Option<IntlDateTimeStyle>,
    pub time_style: Option<IntlDateTimeStyle>,
}

/// DateTimeFormat locale negotiation plus converted options and a canonical time-zone ID.
#[derive(Debug, Eq, PartialEq)]
pub struct IntlDateTimeFormatRequest {
    pub locales: Box<[Box<str>]>,
    pub locale_matcher: IntlLocaleMatcher,
    pub calendar: Option<Box<str>>,
    pub numbering_system: Option<Box<str>>,
    pub hour_cycle: Option<IntlDateTimeHourCycle>,
    pub hour12: Option<bool>,
    pub time_zone: Option<Box<str>>,
    pub options: IntlDateTimeFormatOptions,
}

/// Provider-normalized DateTimeFormat slots published through `resolvedOptions`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntlDateTimeFormatResolved {
    pub locale: Box<str>,
    pub calendar: Box<str>,
    pub numbering_system: Box<str>,
    pub time_zone: Box<str>,
    pub hour_cycle: Option<IntlDateTimeHourCycle>,
    pub options: IntlDateTimeFormatOptions,
}

/// One UTC instant paired with the host-selected civil offset for the requested zone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntlDateTimeInput {
    pub utc_milliseconds: i64,
    pub offset_milliseconds: i64,
}

/// Provider-neutral field classification exposed by DateTimeFormat parts APIs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlDateTimePartType {
    Literal,
    Era,
    Year,
    RelatedYear,
    YearName,
    Month,
    Day,
    Weekday,
    DayPeriod,
    Hour,
    Minute,
    Second,
    FractionalSecond,
    TimeZoneName,
    Unknown,
}

/// One UTF-16 code-unit range in a provider-owned formatted date-time buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntlDateTimePartSpan {
    pub kind: IntlDateTimePartType,
    pub start: u32,
    pub end: u32,
}

/// One formatted date-time and the ordered, gap-free fields that partition it.
#[derive(Debug, Eq, PartialEq)]
pub struct IntlFormattedDateTimeParts {
    pub formatted: Box<[u16]>,
    pub spans: Box<[IntlDateTimePartSpan]>,
}

/// Identifies whether one interval field belongs to the start, end, or shared pattern.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum IntlDateTimeRangeSource {
    StartRange,
    EndRange,
    Shared,
}

/// One UTF-16 field range in a provider-owned formatted date-time interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntlDateTimeRangePartSpan {
    pub kind: IntlDateTimePartType,
    pub source: IntlDateTimeRangeSource,
    pub start: u32,
    pub end: u32,
}

/// One formatted interval and the ordered, gap-free fields that partition it.
#[derive(Debug, Eq, PartialEq)]
pub struct IntlFormattedDateTimeRangeParts {
    pub formatted: Box<[u16]>,
    pub spans: Box<[IntlDateTimeRangePartSpan]>,
}

/// Opaque compiled date-time formatting state retained by a GC external payload.
pub trait IntlDateTimeFormatBackend: Send {
    /// Formats one finite TimeClip result and its host-provided civil offset.
    fn format(&self, input: IntlDateTimeInput) -> Result<Box<[u16]>, HostProviderError>;

    /// Formats one instant and classifies every emitted UTF-16 code unit.
    fn format_to_parts(
        &self,
        input: IntlDateTimeInput,
    ) -> Result<IntlFormattedDateTimeParts, HostProviderError>;

    /// Formats an interval, permitting providers to apply locale-specific field collapsing.
    fn format_range(
        &self,
        start: IntlDateTimeInput,
        end: IntlDateTimeInput,
    ) -> Result<Box<[u16]>, HostProviderError> {
        Ok(self.format_range_to_parts(start, end)?.formatted)
    }

    /// Formats an interval with field ownership; the default preserves all endpoint fields.
    fn format_range_to_parts(
        &self,
        start: IntlDateTimeInput,
        end: IntlDateTimeInput,
    ) -> Result<IntlFormattedDateTimeRangeParts, HostProviderError> {
        const SEPARATOR: &[u16] = &[0x20, 0x2013, 0x20];
        const DATA_FAILURE: HostProviderError = HostProviderError::Failure(3);

        let start = self.format_to_parts(start)?;
        let end = self.format_to_parts(end)?;
        if start.formatted == end.formatted {
            let mut spans = Vec::new();
            spans
                .try_reserve_exact(start.spans.len())
                .map_err(|_| DATA_FAILURE)?;
            spans.extend(start.spans.iter().map(|span| IntlDateTimeRangePartSpan {
                kind: span.kind,
                source: IntlDateTimeRangeSource::Shared,
                start: span.start,
                end: span.end,
            }));
            return Ok(IntlFormattedDateTimeRangeParts {
                formatted: start.formatted,
                spans: spans.into_boxed_slice(),
            });
        }

        let separator_start = u32::try_from(start.formatted.len()).map_err(|_| DATA_FAILURE)?;
        let separator_end = separator_start
            .checked_add(u32::try_from(SEPARATOR.len()).map_err(|_| DATA_FAILURE)?)
            .ok_or(DATA_FAILURE)?;
        let total_units = start
            .formatted
            .len()
            .checked_add(SEPARATOR.len())
            .and_then(|length| length.checked_add(end.formatted.len()))
            .ok_or(DATA_FAILURE)?;
        let mut formatted = Vec::new();
        formatted
            .try_reserve_exact(total_units)
            .map_err(|_| DATA_FAILURE)?;
        formatted.extend_from_slice(&start.formatted);
        formatted.extend_from_slice(SEPARATOR);
        formatted.extend_from_slice(&end.formatted);

        let span_capacity = start
            .spans
            .len()
            .checked_add(end.spans.len())
            .and_then(|length| length.checked_add(1))
            .ok_or(DATA_FAILURE)?;
        let mut spans = Vec::new();
        spans
            .try_reserve_exact(span_capacity)
            .map_err(|_| DATA_FAILURE)?;
        spans.extend(start.spans.iter().map(|span| IntlDateTimeRangePartSpan {
            kind: span.kind,
            source: IntlDateTimeRangeSource::StartRange,
            start: span.start,
            end: span.end,
        }));
        spans.push(IntlDateTimeRangePartSpan {
            kind: IntlDateTimePartType::Literal,
            source: IntlDateTimeRangeSource::Shared,
            start: separator_start,
            end: separator_end,
        });
        for span in &end.spans {
            spans.push(IntlDateTimeRangePartSpan {
                kind: span.kind,
                source: IntlDateTimeRangeSource::EndRange,
                start: span.start.checked_add(separator_end).ok_or(DATA_FAILURE)?,
                end: span.end.checked_add(separator_end).ok_or(DATA_FAILURE)?,
            });
        }
        Ok(IntlFormattedDateTimeRangeParts {
            formatted: formatted.into_boxed_slice(),
            spans: spans.into_boxed_slice(),
        })
    }

    /// Reports only heap backing retained beyond the boxed trait object itself.
    fn external_memory_bytes(&self) -> usize;
}

/// One resolved DateTimeFormat snapshot paired with its reusable compiled backend.
pub struct IntlDateTimeFormatCreation {
    pub resolved: IntlDateTimeFormatResolved,
    pub backend: Box<dyn IntlDateTimeFormatBackend>,
}

/// Owned, provider-neutral inputs for applying `Intl.Locale` constructor options.
#[derive(Clone, Debug)]
pub struct IntlLocaleRequest {
    pub tag: Box<str>,
    pub language: Option<Box<str>>,
    pub script: Option<Box<str>>,
    pub region: Option<Box<str>>,
    pub variants: Option<Box<str>>,
    pub calendar: Option<Box<str>>,
    pub collation: Option<Box<str>>,
    pub hour_cycle: Option<Box<str>>,
    pub case_first: Option<Box<str>>,
    pub numeric: Option<bool>,
    pub numbering_system: Option<Box<str>>,
}

/// Supplies locale data operations without allowing the VM to read process or filesystem state.
pub trait IntlProvider: Send {
    /// Returns one canonical BCP 47 locale, or `None` when the input is structurally invalid.
    fn canonicalize_locale(&mut self, locale: &str) -> Result<Option<Box<str>>, HostProviderError>;

    /// Applies already-observed Locale options and performs the required final canonicalization.
    fn create_locale(
        &mut self,
        request: IntlLocaleRequest,
    ) -> Result<Option<Box<str>>, HostProviderError> {
        let _ = request;
        Err(HostProviderError::Unavailable)
    }

    /// Returns the provider's canonical default locale.
    fn default_locale(&mut self) -> Result<Box<str>, HostProviderError>;

    /// Adds likely language/script/region subtags while preserving extensions.
    fn maximize_locale(&mut self, locale: &str) -> Result<Box<str>, HostProviderError> {
        let _ = locale;
        Err(HostProviderError::Unavailable)
    }

    /// Removes redundant likely subtags while preserving extensions.
    fn minimize_locale(&mut self, locale: &str) -> Result<Box<str>, HostProviderError> {
        let _ = locale;
        Err(HostProviderError::Unavailable)
    }

    /// Returns the provider's supported values as owned strings with no borrowed ICU backing.
    fn supported_values(
        &mut self,
        key: IntlSupportedValuesKey,
    ) -> Result<Box<[Box<str>]>, HostProviderError>;

    /// Creates reusable provider-owned collation state from already converted ECMAScript inputs.
    fn create_collator(
        &mut self,
        _request: IntlCollatorRequest,
    ) -> Result<IntlCollatorCreation, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    /// Filters canonical requested locales while preserving their original canonical spelling.
    fn collator_supported_locales(
        &mut self,
        _locales: &[Box<str>],
        _matcher: IntlLocaleMatcher,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    /// Resolves ListFormat locale and scalar options without retaining provider borrows.
    fn create_list_format(
        &mut self,
        _request: IntlListFormatRequest,
    ) -> Result<IntlListFormatResolved, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    /// Formats already validated ECMAScript String elements into typed UTF-16 parts.
    fn format_list(
        &mut self,
        _resolved: &IntlListFormatResolved,
        _elements: &[Box<[u16]>],
    ) -> Result<IntlFormattedListParts, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    /// Filters canonical requested locales using ListFormat locale data.
    fn list_format_supported_locales(
        &mut self,
        _locales: &[Box<str>],
        _matcher: IntlLocaleMatcher,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    /// Creates reusable provider-owned numeric formatting state from converted inputs.
    fn create_number_format(
        &mut self,
        _request: IntlNumberFormatRequest,
    ) -> Result<IntlNumberFormatCreation, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    /// Filters canonical requested locales using NumberFormat locale data.
    fn number_format_supported_locales(
        &mut self,
        _locales: &[Box<str>],
        _matcher: IntlLocaleMatcher,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    /// Creates reusable provider-owned plural selection state from converted inputs.
    fn create_plural_rules(
        &mut self,
        _request: IntlPluralRulesRequest,
    ) -> Result<IntlPluralRulesCreation, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    /// Filters canonical requested locales using plural-rule data availability.
    fn plural_rules_supported_locales(
        &mut self,
        _locales: &[Box<str>],
        _matcher: IntlLocaleMatcher,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    /// Creates reusable relative-time pattern, number, and plural state.
    fn create_relative_time_format(
        &mut self,
        _request: IntlRelativeTimeFormatRequest,
    ) -> Result<IntlRelativeTimeFormatCreation, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    /// Filters canonical requests using RelativeTimeFormat locale data availability.
    fn relative_time_format_supported_locales(
        &mut self,
        _locales: &[Box<str>],
        _matcher: IntlLocaleMatcher,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    /// Creates reusable provider-owned date-time formatting state from converted inputs.
    fn create_date_time_format(
        &mut self,
        _request: IntlDateTimeFormatRequest,
    ) -> Result<IntlDateTimeFormatCreation, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    /// Filters canonical requested locales using DateTimeFormat locale data.
    fn date_time_format_supported_locales(
        &mut self,
        _locales: &[Box<str>],
        _matcher: IntlLocaleMatcher,
    ) -> Result<Box<[Box<str>]>, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }

    /// Canonicalizes one ECMA-402 time-zone identifier using provider-owned zone data.
    fn canonicalize_time_zone(
        &mut self,
        _identifier: &str,
    ) -> Result<Option<Box<str>>, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }
}

/// Isolate-owned host capabilities; absence remains explicit instead of consulting the process.
#[derive(Default)]
pub struct HostProviders {
    wall_clock: Option<Box<dyn WallClockProvider>>,
    time_zone: Option<Box<dyn TimeZoneProvider>>,
    intl: Option<Box<dyn IntlProvider>>,
    atomics_waiter: Option<Box<dyn AtomicsWaiterProvider>>,
    agent_host: Option<Box<dyn AgentHostProvider>>,
    agent_can_suspend: bool,
}

impl HostProviders {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            wall_clock: None,
            time_zone: None,
            intl: None,
            atomics_waiter: None,
            agent_host: None,
            agent_can_suspend: false,
        }
    }

    #[must_use]
    pub fn with_wall_clock(mut self, provider: impl WallClockProvider + 'static) -> Self {
        self.wall_clock = Some(Box::new(provider));
        self
    }

    #[must_use]
    pub fn with_time_zone(mut self, provider: impl TimeZoneProvider + 'static) -> Self {
        self.time_zone = Some(Box::new(provider));
        self
    }

    #[must_use]
    pub fn with_intl(mut self, provider: impl IntlProvider + 'static) -> Self {
        self.intl = Some(Box::new(provider));
        self
    }

    #[must_use]
    pub fn with_atomics_waiter(mut self, provider: impl AtomicsWaiterProvider + 'static) -> Self {
        self.atomics_waiter = Some(Box::new(provider));
        self
    }

    #[must_use]
    pub fn with_agent_host(mut self, provider: impl AgentHostProvider + 'static) -> Self {
        self.agent_host = Some(Box::new(provider));
        self
    }

    #[must_use]
    pub const fn with_agent_can_suspend(mut self, can_suspend: bool) -> Self {
        self.agent_can_suspend = can_suspend;
        self
    }

    pub(crate) fn wall_clock_mut(&mut self) -> Option<&mut (dyn WallClockProvider + 'static)> {
        self.wall_clock.as_deref_mut()
    }

    pub(crate) fn time_zone_mut(&mut self) -> Option<&mut (dyn TimeZoneProvider + 'static)> {
        self.time_zone.as_deref_mut()
    }

    pub(crate) fn intl_mut(&mut self) -> Option<&mut (dyn IntlProvider + 'static)> {
        self.intl.as_deref_mut()
    }

    pub(crate) fn atomics_waiter_mut(
        &mut self,
    ) -> Option<&mut (dyn AtomicsWaiterProvider + 'static)> {
        self.atomics_waiter.as_deref_mut()
    }

    pub(crate) fn agent_host_mut(&mut self) -> Option<&mut (dyn AgentHostProvider + 'static)> {
        self.agent_host.as_deref_mut()
    }

    pub(crate) const fn agent_can_suspend(&self) -> bool {
        self.agent_can_suspend
    }
}

impl fmt::Debug for HostProviders {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostProviders")
            .field("wall_clock", &self.wall_clock.is_some())
            .field("time_zone", &self.time_zone.is_some())
            .field("intl", &self.intl.is_some())
            .field("atomics_waiter", &self.atomics_waiter.is_some())
            .field("agent_host", &self.agent_host.is_some())
            .field("agent_can_suspend", &self.agent_can_suspend)
            .finish()
    }
}
