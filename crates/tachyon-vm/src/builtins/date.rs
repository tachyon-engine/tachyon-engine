//! Date branded-object construction and clock-independent primitive operations.

use super::super::*;

const MAX_TIME_VALUE: f64 = 8.64e15;

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
}
