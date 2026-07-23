//! Date branded-object construction and clock-independent primitive operations.

use super::super::*;

const MAX_TIME_VALUE: f64 = 8.64e15;
const MS_PER_SECOND: i64 = 1_000;
const MS_PER_MINUTE: i64 = 60 * MS_PER_SECOND;
const MS_PER_HOUR: i64 = 60 * MS_PER_MINUTE;
const MS_PER_DAY: i64 = 24 * MS_PER_HOUR;
const MONTH_START_DAYS: [[i64; 12]; 2] = [
    [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334],
    [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335],
];

impl Isolate {
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

    /// Implements Date.UTC with ordered primitive ToNumber conversion and UTC field normalization.
    pub(crate) fn date_utc_from_site(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let mut fields = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        for index in 0..site.argument_count.min(fields.len() as u32) {
            let argument = self
                .call_argument(site, index)?
                .expect("Date.UTC argument remains inside the call window");
            let converted = self.convert_to_number(argument)?;
            fields[index as usize] = numeric_value(converted)
                .ok_or(ExecutionError::UnsupportedNumberConversion(argument))?
                .trunc();
        }
        if (0.0..=99.0).contains(&fields[0]) {
            fields[0] += 1900.0;
        }
        Ok(Value::from_f64(make_utc_date(fields)))
    }

    /// Implements Date.prototype.setTime for the currently synchronous ToNumber subset.
    pub(crate) fn date_set_time_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        self.date_time_value(site.this_value)?
            .ok_or(ExecutionError::NotObject(site.this_value))?;
        let argument = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let converted = self.convert_to_number(argument)?;
        let clipped = time_clip(
            numeric_value(converted)
                .ok_or(ExecutionError::UnsupportedNumberConversion(argument))?,
        );
        self.set_date_time_value(site.this_value, clipped)?;
        Ok(Value::from_f64(clipped))
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
}
