//! Date branded-object construction and clock-independent primitive operations.

use super::super::*;

const MAX_TIME_VALUE: f64 = 8.64e15;
const MS_PER_SECOND: i64 = 1_000;
const MS_PER_MINUTE: i64 = 60 * MS_PER_SECOND;
const MS_PER_HOUR: i64 = 60 * MS_PER_MINUTE;
const MS_PER_DAY: i64 = 24 * MS_PER_HOUR;
const DATE_FORMAT_CAPACITY: usize = 40;
const INVALID_DATE: &[u8] = b"Invalid Date";
const WEEKDAY_NAMES: [[u8; 3]; 7] = [
    *b"Sun", *b"Mon", *b"Tue", *b"Wed", *b"Thu", *b"Fri", *b"Sat",
];
const MONTH_NAMES: [[u8; 3]; 12] = [
    *b"Jan", *b"Feb", *b"Mar", *b"Apr", *b"May", *b"Jun", *b"Jul", *b"Aug", *b"Sep", *b"Oct",
    *b"Nov", *b"Dec",
];
const MONTH_START_DAYS: [[i64; 12]; 2] = [
    [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334],
    [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335],
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum DateNumericOperation {
    Utc,
    SetTime,
    UtcSetter(DateUtcSetter),
}

/// Traced cold-path state retained while Date numeric arguments invoke JavaScript conversion code.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingDateNumericArguments {
    receiver: Value,
    arguments: [Value; 7],
    fields: [f64; 7],
    operation: DateNumericOperation,
    argument_count: u8,
    next_argument: u8,
    preserve_invalid_receiver: bool,
}

impl Trace for PendingDateNumericArguments {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.receiver.trace(tracer);
        self.arguments.trace(tracer);
    }
}

struct PendingDateNumericRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingDateNumericArguments,
}

impl Trace for PendingDateNumericRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Implements Date.prototype[@@toPrimitive] through forced ordinary conversion ordering.
    pub(crate) fn begin_date_to_primitive(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(site.this_value) {
            return Err(ExecutionError::NotObject(site.this_value));
        }
        let hint = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_string_value(hint) {
            return Err(ExecutionError::InvalidDatePrimitiveHint(hint));
        }
        let hints = self.realm.primitive_hint_strings;
        let string_first = self.strict_equal_values(hint, hints.string)?
            || self.strict_equal_values(hint, hints.default)?;
        let (consumer, stage) = if string_first {
            (
                ConversionConsumer::DateToPrimitiveString,
                ToPrimitiveStage::ToString,
            )
        } else if self.strict_equal_values(hint, hints.number)? {
            (
                ConversionConsumer::DateToPrimitiveNumber,
                ToPrimitiveStage::ValueOf,
            )
        } else {
            return Err(ExecutionError::InvalidDatePrimitiveHint(hint));
        };
        self.advance_native_conversion(
            ConversionContinuation {
                site: Self::date_continuation_site(site),
                consumer,
                receiver: Value::from_immediate(Immediate::Undefined),
                object: site.this_value,
                stage,
                callback_stage: ConversionCallbackStage::MethodCall,
            },
            None,
        )
    }

    /// Constructs the clock-independent single-argument Date form with Realm-correct prototype.
    pub(crate) fn create_date_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let argument =
            self.call_argument(site, 0)?
                .ok_or(ExecutionError::UnsupportedNumberConversion(
                    Value::from_immediate(Immediate::Undefined),
                ))?;
        let date_value = if let Some(date_value) = self.date_time_value(argument)? {
            date_value
        } else {
            let number = self.convert_to_number(argument)?;
            time_clip(numeric_value(number).expect("ToNumber returns a numeric value"))
        };
        let default_prototype = self
            .realm
            .date_prototype
            .expect("Date prototype initializes before Date construction");
        let prototype = if self.is_object_value(site.new_target) {
            let prototype_atom = self.prototype_atom()?;
            self.constructor_prototype_value(site.new_target, prototype_atom)?
                .filter(|value| self.is_object_value(*value))
                .or_else(|| {
                    self.realm_for_callable(site.new_target)
                        .ok()
                        .and_then(|realm| {
                            self.realm_intrinsic_prototype(realm, IntrinsicPrototypeKind::Date)
                        })
                })
                .unwrap_or(default_prototype)
        } else {
            default_prototype
        };
        self.allocate_date_object(date_value, prototype, AllocationSpace::Young)
    }

    /// Implements the shared thisTimeValue operation for Date.prototype.getTime/valueOf.
    pub(crate) fn date_prototype_time_value(
        &mut self,
        receiver: Value,
    ) -> Result<Value, ExecutionError> {
        let date_value = self
            .date_time_value(receiver)?
            .ok_or(ExecutionError::NotObject(receiver))?;
        Ok(Value::from_f64(date_value))
    }

    /// Returns one UTC calendar field after applying the Date brand check.
    pub(crate) fn date_utc_field_value(
        &mut self,
        receiver: Value,
        field: DateUtcField,
    ) -> Result<Value, ExecutionError> {
        let date_value = self
            .date_time_value(receiver)?
            .ok_or(ExecutionError::NotObject(receiver))?;
        if date_value.is_nan() {
            return Ok(Value::from_f64(f64::NAN));
        }
        let parts = UtcDateParts::from_time(date_value as i64);
        let value = match field {
            DateUtcField::FullYear => parts.year,
            DateUtcField::Month => parts.month,
            DateUtcField::Date => parts.date,
            DateUtcField::Day => parts.day,
            DateUtcField::Hours => parts.hours,
            DateUtcField::Minutes => parts.minutes,
            DateUtcField::Seconds => parts.seconds,
            DateUtcField::Milliseconds => parts.milliseconds,
        };
        Ok(Value::from_f64(value as f64))
    }

    /// Starts Date.UTC argument conversion, allocating continuation state only for object operands.
    pub(crate) fn begin_date_utc(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let pending = self.pending_date_arguments(
            site,
            DateNumericOperation::Utc,
            Value::from_immediate(Immediate::Undefined),
            [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            site.argument_count.min(7) as u8,
            false,
        )?;
        self.drive_date_numeric_arguments(Self::date_continuation_site(site), pending, None, None)
    }

    /// Starts Date.prototype.setTime after applying its receiver brand check.
    pub(crate) fn begin_date_set_time(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        self.date_time_value(site.this_value)?
            .ok_or(ExecutionError::NotObject(site.this_value))?;
        let pending = self.pending_date_arguments(
            site,
            DateNumericOperation::SetTime,
            site.this_value,
            [f64::NAN; 7],
            1,
            false,
        )?;
        self.drive_date_numeric_arguments(Self::date_continuation_site(site), pending, None, None)
    }

    /// Starts one UTC setter after snapshotting the branded receiver before any conversion.
    pub(crate) fn begin_date_utc_setter(
        &mut self,
        site: &CallSite,
        setter: DateUtcSetter,
    ) -> Result<(), ExecutionError> {
        let original = self
            .date_time_value(site.this_value)?
            .ok_or(ExecutionError::NotObject(site.this_value))?;
        let fields = if original.is_nan() && setter != DateUtcSetter::FullYear {
            [f64::NAN; 7]
        } else {
            UtcDateParts::from_time(if original.is_nan() {
                0
            } else {
                original as i64
            })
            .make_date_fields()
        };
        let count = site.argument_count.min(setter.length() as u32).max(1) as u8;
        let pending = self.pending_date_arguments(
            site,
            DateNumericOperation::UtcSetter(setter),
            site.this_value,
            fields,
            count,
            original.is_nan() && setter != DateUtcSetter::FullYear,
        )?;
        self.drive_date_numeric_arguments(Self::date_continuation_site(site), pending, None, None)
    }

    /// Formats the canonical simplified ISO UTC representation without heap intermediates.
    pub(crate) fn date_to_iso_string(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        let date_value = self
            .date_time_value(receiver)?
            .ok_or(ExecutionError::NotObject(receiver))?;
        if date_value.is_nan() {
            return Err(ExecutionError::InvalidDateValue);
        }
        let parts = UtcDateParts::from_time(date_value as i64);
        let mut output = DateFormatBuffer::new();
        if (0..=9_999).contains(&parts.year) {
            output.push_unsigned(parts.year as u64, 4);
        } else {
            output.push_byte(if parts.year < 0 { b'-' } else { b'+' });
            output.push_unsigned(parts.year.unsigned_abs(), 6);
        }
        output.push_byte(b'-');
        output.push_unsigned(parts.month as u64 + 1, 2);
        output.push_byte(b'-');
        output.push_unsigned(parts.date as u64, 2);
        output.push_byte(b'T');
        output.push_clock(parts);
        output.push_byte(b'.');
        output.push_unsigned(parts.milliseconds as u64, 3);
        output.push_byte(b'Z');
        self.allocate_date_format(output.as_bytes())
    }

    /// Formats the implementation-independent UTC Date string and invalid-Date sentinel.
    pub(crate) fn date_to_utc_string(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        let date_value = self
            .date_time_value(receiver)?
            .ok_or(ExecutionError::NotObject(receiver))?;
        if date_value.is_nan() {
            return self.allocate_date_format(INVALID_DATE);
        }
        let parts = UtcDateParts::from_time(date_value as i64);
        let mut output = DateFormatBuffer::new();
        output.push_bytes(&WEEKDAY_NAMES[parts.day as usize]);
        output.push_bytes(b", ");
        output.push_unsigned(parts.date as u64, 2);
        output.push_byte(b' ');
        output.push_bytes(&MONTH_NAMES[parts.month as usize]);
        output.push_byte(b' ');
        if parts.year < 0 {
            output.push_byte(b'-');
        }
        output.push_unsigned(parts.year.unsigned_abs(), 4);
        output.push_byte(b' ');
        output.push_clock(parts);
        output.push_bytes(b" GMT");
        self.allocate_date_format(output.as_bytes())
    }

    /// Copies one audited ASCII Date format buffer into a managed ECMAScript string.
    fn allocate_date_format(&mut self, bytes: &[u8]) -> Result<Value, ExecutionError> {
        self.allocate_runtime_string(
            JsString::try_from_latin1(bytes).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Resumes a Date conversion after one object argument has produced its primitive value.
    pub(crate) fn resume_date_numeric_arguments(
        &mut self,
        site: NativeContinuationSite,
        state_value: Value,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.pending_date_numeric_reference(state_value)?;
        let pending = self.pending_date_numeric_snapshot(state)?;
        self.drive_date_numeric_arguments(site, pending, Some(state), Some(primitive))
    }

    /// Copies the bounded argument window before any nested JavaScript callback can replace it.
    fn pending_date_arguments(
        &mut self,
        site: &CallSite,
        operation: DateNumericOperation,
        receiver: Value,
        fields: [f64; 7],
        argument_count: u8,
        preserve_invalid_receiver: bool,
    ) -> Result<PendingDateNumericArguments, ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let mut arguments = [undefined; 7];
        for index in 0..argument_count {
            arguments[index as usize] = self
                .call_argument(site, u32::from(index))?
                .unwrap_or(undefined);
        }
        Ok(PendingDateNumericArguments {
            receiver,
            arguments,
            fields,
            operation,
            argument_count,
            next_argument: 0,
            preserve_invalid_receiver,
        })
    }

    /// Drives primitive fast paths and publishes a shared ToPrimitive continuation when required.
    fn drive_date_numeric_arguments(
        &mut self,
        site: NativeContinuationSite,
        mut pending: PendingDateNumericArguments,
        state: Option<GcRef<PendingDateNumericArguments>>,
        returned: Option<Value>,
    ) -> Result<(), ExecutionError> {
        if let Some(primitive) = returned {
            self.store_date_numeric_argument(&mut pending, primitive)?;
        }
        while pending.next_argument < pending.argument_count {
            let argument = pending.arguments[pending.next_argument as usize];
            if self.is_object_value(argument) {
                let state = match state {
                    Some(state) => {
                        self.replace_pending_date_numeric(state, pending)?;
                        state
                    }
                    None => self.allocate_pending_date_numeric(pending)?,
                };
                return self.dispatch_object_primitive_conversion(
                    ConversionConsumer::DateNumericArgument,
                    site.caller_base,
                    site.destination,
                    Value::from_heap_ref(state.raw()),
                    argument,
                    site.call_site,
                );
            }
            self.store_date_numeric_argument(&mut pending, argument)?;
        }
        let result = self.finish_date_numeric_arguments(pending)?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Converts one primitive and stores it in the operation-specific MakeDate field.
    fn store_date_numeric_argument(
        &mut self,
        pending: &mut PendingDateNumericArguments,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        debug_assert!(pending.next_argument < pending.argument_count);
        let converted = self.convert_to_number(primitive)?;
        let number = numeric_value(converted)
            .ok_or(ExecutionError::UnsupportedNumberConversion(primitive))?
            .trunc();
        let field = match pending.operation {
            DateNumericOperation::Utc => pending.next_argument as usize,
            DateNumericOperation::SetTime => 0,
            DateNumericOperation::UtcSetter(setter) => {
                date_utc_setter_start(setter) + pending.next_argument as usize
            }
        };
        pending.fields[field] = number;
        pending.next_argument += 1;
        Ok(())
    }

    /// Commits Date.UTC or one branded Date mutation after every observable conversion completes.
    fn finish_date_numeric_arguments(
        &mut self,
        mut pending: PendingDateNumericArguments,
    ) -> Result<Value, ExecutionError> {
        let date_value = match pending.operation {
            DateNumericOperation::Utc => {
                if (0.0..=99.0).contains(&pending.fields[0]) {
                    pending.fields[0] += 1900.0;
                }
                make_utc_date(pending.fields)
            }
            DateNumericOperation::SetTime => time_clip(pending.fields[0]),
            DateNumericOperation::UtcSetter(_) if pending.preserve_invalid_receiver => f64::NAN,
            DateNumericOperation::UtcSetter(_) => make_utc_date(pending.fields),
        };
        if pending.operation != DateNumericOperation::Utc && !pending.preserve_invalid_receiver {
            self.set_date_time_value(pending.receiver, date_value)?;
        }
        Ok(Value::from_f64(date_value))
    }

    #[inline(always)]
    fn date_continuation_site(site: &CallSite) -> NativeContinuationSite {
        NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        }
    }

    /// Allocates the cold conversion payload under the complete isolate root set.
    fn allocate_pending_date_numeric(
        &mut self,
        pending: PendingDateNumericArguments,
    ) -> Result<GcRef<PendingDateNumericArguments>, ExecutionError> {
        let mut roots = PendingDateNumericRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            pending,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_date_numeric_arguments,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked Date numeric continuation reference from its traced Value handle.
    fn pending_date_numeric_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingDateNumericArguments>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_date_numeric_arguments)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    /// Copies the payload without retaining a heap borrow across conversion or callback work.
    fn pending_date_numeric_snapshot(
        &mut self,
        state: GcRef<PendingDateNumericArguments>,
    ) -> Result<PendingDateNumericArguments, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_date_numeric_arguments)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Publishes scalar progress before another object argument can invoke arbitrary JavaScript.
    fn replace_pending_date_numeric(
        &mut self,
        state: GcRef<PendingDateNumericArguments>,
        pending: PendingDateNumericArguments,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let current = no_gc
                    .borrow_mut(state, self.types.pending_date_numeric_arguments)
                    .map_err(ExecutionError::NoGcBorrow)?;
                debug_assert_eq!(current.receiver, pending.receiver);
                debug_assert_eq!(current.arguments, pending.arguments);
                debug_assert_eq!(current.operation, pending.operation);
                debug_assert_eq!(current.argument_count, pending.argument_count);
                debug_assert_eq!(
                    current.preserve_invalid_receiver,
                    pending.preserve_invalid_receiver
                );
                current.fields = pending.fields;
                current.next_argument = pending.next_argument;
                Ok(())
            })
        })
    }

    /// Reads `[[DateValue]]` only from a genuine Date payload.
    pub(crate) fn date_time_value(&mut self, value: Value) -> Result<Option<f64>, ExecutionError> {
        let Some(raw) = value.as_heap_ref() else {
            return Ok(None);
        };
        let Ok(date) = self.heap.checked_reference(raw, self.types.date_object) else {
            return Ok(None);
        };
        self.heap.with_running_scope(|scope| {
            let date = scope.root(date).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(date, self.types.date_object)
                    .map(|date| Some(date.date_value))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Replaces the private Date value after the receiver has passed its brand check.
    fn set_date_time_value(
        &mut self,
        receiver: Value,
        date_value: f64,
    ) -> Result<(), ExecutionError> {
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(receiver))?;
        let date = self
            .heap
            .checked_reference(raw, self.types.date_object)
            .map_err(|_| ExecutionError::NotObject(receiver))?;
        self.heap.with_running_scope(|scope| {
            let date = scope.root(date).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(date, self.types.date_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .date_value = date_value;
                Ok(())
            })
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UtcDateParts {
    year: i64,
    month: i64,
    date: i64,
    day: i64,
    hours: i64,
    minutes: i64,
    seconds: i64,
    milliseconds: i64,
}

struct DateFormatBuffer {
    bytes: [u8; DATE_FORMAT_CAPACITY],
    length: usize,
}

impl DateFormatBuffer {
    #[inline(always)]
    const fn new() -> Self {
        Self {
            bytes: [0; DATE_FORMAT_CAPACITY],
            length: 0,
        }
    }

    #[inline(always)]
    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }

    #[inline(always)]
    fn push_byte(&mut self, byte: u8) {
        self.bytes[self.length] = byte;
        self.length += 1;
    }

    #[inline(always)]
    fn push_bytes(&mut self, bytes: &[u8]) {
        let end = self.length + bytes.len();
        self.bytes[self.length..end].copy_from_slice(bytes);
        self.length = end;
    }

    /// Emits an unsigned decimal integer with at least the requested zero-padded width.
    fn push_unsigned(&mut self, mut value: u64, minimum_width: usize) {
        let mut scratch = [0; 20];
        let mut cursor = scratch.len();
        loop {
            cursor -= 1;
            scratch[cursor] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        let digits = scratch.len() - cursor;
        for _ in digits..minimum_width {
            self.push_byte(b'0');
        }
        self.push_bytes(&scratch[cursor..]);
    }

    #[inline(always)]
    fn push_clock(&mut self, parts: UtcDateParts) {
        self.push_unsigned(parts.hours as u64, 2);
        self.push_byte(b':');
        self.push_unsigned(parts.minutes as u64, 2);
        self.push_byte(b':');
        self.push_unsigned(parts.seconds as u64, 2);
    }
}

impl UtcDateParts {
    /// Decomposes an already-clipped integral epoch millisecond value without floating rounding.
    fn from_time(time: i64) -> Self {
        let days = time.div_euclid(MS_PER_DAY);
        let within_day = time.rem_euclid(MS_PER_DAY);
        let (year, month, date) = civil_from_days(days);
        Self {
            year,
            month: month - 1,
            date,
            day: (days + 4).rem_euclid(7),
            hours: within_day / MS_PER_HOUR,
            minutes: (within_day / MS_PER_MINUTE) % 60,
            seconds: (within_day / MS_PER_SECOND) % 60,
            milliseconds: within_day % MS_PER_SECOND,
        }
    }

    #[inline(always)]
    fn make_date_fields(self) -> [f64; 7] {
        [
            self.year as f64,
            self.month as f64,
            self.date as f64,
            self.hours as f64,
            self.minutes as f64,
            self.seconds as f64,
            self.milliseconds as f64,
        ]
    }
}

#[inline(always)]
const fn date_utc_setter_start(setter: DateUtcSetter) -> usize {
    match setter {
        DateUtcSetter::FullYear => 0,
        DateUtcSetter::Month => 1,
        DateUtcSetter::Date => 2,
        DateUtcSetter::Hours => 3,
        DateUtcSetter::Minutes => 4,
        DateUtcSetter::Seconds => 5,
        DateUtcSetter::Milliseconds => 6,
    }
}

/// Converts days since 1970-01-01 to a proleptic Gregorian civil date.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let date = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, date)
}

/// Applies MakeDay, MakeTime, MakeDate, and TimeClip in specification evaluation order.
fn make_utc_date(fields: [f64; 7]) -> f64 {
    if fields.iter().any(|field| !field.is_finite()) {
        return f64::NAN;
    }
    let year_month = fields[0] + (fields[1] / 12.0).floor();
    let mut month = fields[1] % 12.0;
    if month < 0.0 {
        month += 12.0;
    }
    if !(-271_821.0..=275_760.0).contains(&year_month) {
        return f64::NAN;
    }
    let year = year_month as i64;
    let leap = usize::from(is_leap_year(year));
    let days = days_from_year(year) + MONTH_START_DAYS[leap][month as usize];
    let day = days as f64 + fields[2] - 1.0;
    let mut time = fields[3] * MS_PER_HOUR as f64;
    time += fields[4] * MS_PER_MINUTE as f64;
    time += fields[5] * MS_PER_SECOND as f64;
    time += fields[6];
    let date = day * MS_PER_DAY as f64 + time;
    time_clip(date)
}

#[inline(always)]
fn is_leap_year(year: i64) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

#[inline(always)]
fn days_from_year(year: i64) -> i64 {
    365 * (year - 1970) + (year - 1969).div_euclid(4) - (year - 1901).div_euclid(100)
        + (year - 1601).div_euclid(400)
}

/// Applies ECMAScript TimeClip without consulting a host clock or timezone provider.
#[inline(always)]
fn time_clip(value: f64) -> f64 {
    if !value.is_finite() || value.abs() > MAX_TIME_VALUE {
        f64::NAN
    } else if value == 0.0 {
        0.0
    } else {
        let clipped = value.trunc();
        if clipped == 0.0 { 0.0 } else { clipped }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_numeric_argument_state_keeps_the_audited_cold_path_size() {
        assert_eq!(core::mem::size_of::<PendingDateNumericArguments>(), 128);
    }

    #[test]
    fn time_clip_rejects_the_specification_boundary_and_truncates_finite_values() {
        assert_eq!(time_clip(1.9), 1.0);
        assert_eq!(time_clip(-1.9), -1.0);
        assert_eq!(time_clip(-0.0).to_bits(), 0.0_f64.to_bits());
        assert_eq!(time_clip(-0.5).to_bits(), 0.0_f64.to_bits());
        assert_eq!(time_clip(MAX_TIME_VALUE), MAX_TIME_VALUE);
        assert!(time_clip(MAX_TIME_VALUE + 1.0).is_nan());
        assert!(time_clip(f64::INFINITY).is_nan());
    }

    #[test]
    /// Covers civil decomposition boundaries that differ under truncating division.
    fn utc_date_parts_cover_epoch_negative_time_leap_day_and_upper_boundary() {
        assert_eq!(
            UtcDateParts::from_time(0),
            UtcDateParts {
                year: 1970,
                month: 0,
                date: 1,
                day: 4,
                hours: 0,
                minutes: 0,
                seconds: 0,
                milliseconds: 0,
            }
        );
        let before_epoch = UtcDateParts::from_time(-1);
        assert_eq!(
            (before_epoch.year, before_epoch.month, before_epoch.date),
            (1969, 11, 31)
        );
        assert_eq!(
            (
                before_epoch.hours,
                before_epoch.minutes,
                before_epoch.seconds,
                before_epoch.milliseconds,
            ),
            (23, 59, 59, 999)
        );
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(100_000_000), (275_760, 9, 13));
    }

    #[test]
    fn make_utc_date_normalizes_fields_and_clips_the_time_range() {
        assert_eq!(make_utc_date([1970.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0]), 0.0);
        assert_eq!(
            make_utc_date([2016.0, 12.0, 1.0, 0.0, 0.0, 0.0, 0.0]),
            1_483_228_800_000.0
        );
        assert_eq!(
            make_utc_date([2016.0, 6.0, 5.0, -1.0, 0.0, 0.0, 0.0]),
            1_467_673_200_000.0
        );
        assert_eq!(
            make_utc_date([275_760.0, 8.0, 13.0, 0.0, 0.0, 0.0, 0.0]),
            MAX_TIME_VALUE
        );
        assert!(make_utc_date([275_760.0, 8.0, 13.0, 0.0, 0.0, 0.0, 1.0]).is_nan());
    }

    #[test]
    fn date_format_buffer_zero_pads_and_preserves_extended_years() {
        let mut output = DateFormatBuffer::new();
        output.push_unsigned(20, 4);
        output.push_byte(b' ');
        output.push_byte(b'-');
        output.push_unsigned(1, 4);
        output.push_byte(b' ');
        output.push_byte(b'+');
        output.push_unsigned(275_760, 6);
        assert_eq!(output.as_bytes(), b"0020 -0001 +275760");
    }
}
