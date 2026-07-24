//! Allocation-free implementations of the ECMAScript Math intrinsic family.

use super::super::*;

impl Isolate {
    /// Executes one Math method after applying ToNumber to its primitive arguments.
    pub(crate) fn math_value(
        &mut self,
        function: MathFunction,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        if function == MathFunction::Random {
            return Ok(Value::from_f64(self.next_math_random()));
        }
        if matches!(
            function,
            MathFunction::Max | MathFunction::Min | MathFunction::Hypot
        ) {
            return self.math_variadic_value(function, site);
        }
        let first = self.math_number_argument(site, 0)?;
        let result = match function {
            MathFunction::Abs => first.abs(),
            MathFunction::Acos => first.acos(),
            MathFunction::Acosh => math_acosh(first),
            MathFunction::Asin => first.asin(),
            MathFunction::Asinh => math_asinh(first),
            MathFunction::Atan => first.atan(),
            MathFunction::Atanh => first.atanh(),
            MathFunction::Atan2 => first.atan2(self.math_number_argument(site, 1)?),
            MathFunction::Cbrt => first.cbrt(),
            MathFunction::Ceil => first.ceil(),
            MathFunction::Clz32 => f64::from(to_uint32(first).leading_zeros()),
            MathFunction::Cos => first.cos(),
            MathFunction::Cosh => first.cosh(),
            MathFunction::Exp => first.exp(),
            MathFunction::Expm1 => first.exp_m1(),
            MathFunction::Floor => first.floor(),
            MathFunction::F16Round => round_to_binary16(first),
            MathFunction::Fround => f64::from(first as f32),
            MathFunction::Imul => {
                let second = self.math_number_argument(site, 1)?;
                f64::from((to_uint32(first) as i32).wrapping_mul(to_uint32(second) as i32))
            }
            MathFunction::Log => first.ln(),
            MathFunction::Log1p => first.ln_1p(),
            MathFunction::Log10 => first.log10(),
            MathFunction::Log2 => first.log2(),
            MathFunction::Pow => math_pow(first, self.math_number_argument(site, 1)?),
            MathFunction::Round => math_round(first),
            MathFunction::Sign => math_sign(first),
            MathFunction::Sin => first.sin(),
            MathFunction::Sinh => first.sinh(),
            MathFunction::Sqrt => first.sqrt(),
            MathFunction::Tan => first.tan(),
            MathFunction::Tanh => first.tanh(),
            MathFunction::Trunc => first.trunc(),
            MathFunction::Hypot | MathFunction::Max | MathFunction::Min | MathFunction::Random => {
                unreachable!("special Math methods return before unary dispatch")
            }
        };
        Ok(Value::from_f64(result))
    }

    /// Converts one present-or-undefined Math argument without hiding object conversion gaps.
    fn math_number_argument(&mut self, site: &CallSite, index: u32) -> Result<f64, ExecutionError> {
        let argument = self
            .call_argument(site, index)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let converted = self.convert_to_number(argument)?;
        numeric_value(converted).ok_or(ExecutionError::UnsupportedNumberConversion(argument))
    }

    /// Implements left-to-right variadic conversion and signed-zero/NaN selection.
    fn math_variadic_value(
        &mut self,
        function: MathFunction,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let mut result: f64 = match function {
            MathFunction::Max => f64::NEG_INFINITY,
            MathFunction::Min => f64::INFINITY,
            MathFunction::Hypot => 0.0,
            _ => unreachable!("only variadic Math methods use this path"),
        };
        let mut scale: f64 = 0.0;
        let mut sum: f64 = 0.0;
        let mut saw_nan = false;
        for index in 0..site.argument_count {
            let number = self.math_number_argument(site, index)?;
            match function {
                MathFunction::Max => result = math_max(result, number),
                MathFunction::Min => result = math_min(result, number),
                MathFunction::Hypot if number.is_infinite() => scale = f64::INFINITY,
                MathFunction::Hypot if number.is_nan() => saw_nan = true,
                MathFunction::Hypot if scale.is_finite() => {
                    let absolute = number.abs();
                    if absolute > scale {
                        let ratio = if scale == 0.0 { 0.0 } else { scale / absolute };
                        sum = 1.0 + sum * ratio * ratio;
                        scale = absolute;
                    } else if absolute != 0.0 {
                        let ratio = absolute / scale;
                        sum += ratio * ratio;
                    }
                }
                _ => {}
            }
        }
        if function == MathFunction::Hypot {
            result = if scale.is_infinite() {
                f64::INFINITY
            } else if saw_nan {
                f64::NAN
            } else if scale == 0.0 {
                0.0
            } else {
                scale * sum.sqrt()
            };
        }
        Ok(Value::from_f64(result))
    }

    /// Advances the realm-local deterministic PRNG without host I/O or shared atomics.
    fn next_math_random(&mut self) -> f64 {
        let mut state = self.math_random_state;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.math_random_state = state;
        let bits = state.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11;
        bits as f64 * (1.0 / 9_007_199_254_740_992.0)
    }
}

#[inline]
fn to_uint32(number: f64) -> u32 {
    if !number.is_finite() || number == 0.0 {
        0
    } else {
        number.trunc().rem_euclid(4_294_967_296.0) as u32
    }
}

#[inline]
fn math_round(number: f64) -> f64 {
    if !number.is_finite() || number == 0.0 {
        return number;
    }
    if number.fract() == 0.0 {
        return number;
    }
    if (-0.5..0.0).contains(&number) || number == -0.5 {
        return -0.0;
    }
    let lower = number.floor();
    if number - lower < 0.5 {
        lower
    } else {
        lower + 1.0
    }
}

#[inline]
fn math_sign(number: f64) -> f64 {
    if number.is_nan() || number == 0.0 {
        number
    } else if number.is_sign_negative() {
        -1.0
    } else {
        1.0
    }
}

#[inline]
fn math_max(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else if left == right && left == 0.0 {
        if left.is_sign_positive() || right.is_sign_positive() {
            0.0
        } else {
            -0.0
        }
    } else {
        left.max(right)
    }
}

#[inline]
fn math_min(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else if left == right && left == 0.0 {
        if left.is_sign_negative() || right.is_sign_negative() {
            -0.0
        } else {
            0.0
        }
    } else {
        left.min(right)
    }
}

#[inline]
fn math_pow(base: f64, exponent: f64) -> f64 {
    if exponent == 0.0 {
        1.0
    } else if exponent.is_nan() || base.is_nan() || (base.abs() == 1.0 && exponent.is_infinite()) {
        f64::NAN
    } else {
        base.powf(exponent)
    }
}

#[inline]
fn math_acosh(number: f64) -> f64 {
    if number.is_finite() && number > 67_108_864.0 {
        number.ln() + std::f64::consts::LN_2
    } else {
        number.acosh()
    }
}

#[inline]
fn math_asinh(number: f64) -> f64 {
    if number.is_finite() && number.abs() > 67_108_864.0 {
        number.signum() * (number.abs().ln() + std::f64::consts::LN_2)
    } else {
        number.asinh()
    }
}

/// Rounds directly from binary64 to binary16, including ties-to-even and signed zero.
fn round_to_binary16(number: f64) -> f64 {
    if !number.is_finite() || number == 0.0 {
        return number;
    }
    let sign = if number.is_sign_negative() { -1.0 } else { 1.0 };
    let absolute = number.abs();
    if absolute >= 65_520.0 {
        return sign * f64::INFINITY;
    }
    let rounded = if absolute < 2f64.powi(-14) {
        (absolute * 2f64.powi(24)).round_ties_even() * 2f64.powi(-24)
    } else {
        let exponent = (((absolute.to_bits() >> 52) & 0x7ff) as i32) - 1023;
        let step = 2f64.powi(exponent - 10);
        (absolute / step).round_ties_even() * step
    };
    if rounded == 0.0 && sign < 0.0 {
        -0.0
    } else {
        sign * rounded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary16_rounding_handles_ties_subnormals_and_overflow() {
        assert_eq!(round_to_binary16(1.000_488_281_25), 1.0);
        assert_eq!(round_to_binary16(1.000_488_281_250_000_2), 1.000_976_562_5);
        assert_eq!(round_to_binary16(2f64.powi(-25)), 0.0);
        assert_eq!(round_to_binary16(65_520.0), f64::INFINITY);
        assert!(round_to_binary16(-2f64.powi(-26)).is_sign_negative());
    }

    #[test]
    fn selection_and_rounding_preserve_ecmascript_signed_zero() {
        assert!(math_min(0.0, -0.0).is_sign_negative());
        assert!(math_max(-0.0, 0.0).is_sign_positive());
        assert!(math_round(-0.1).is_sign_negative());
        assert_eq!(math_round(0.5), 1.0);
        assert_eq!(math_round(0.5 - f64::EPSILON / 4.0), 0.0);
        assert_eq!(
            math_round((1.0 / f64::EPSILON) + 1.0),
            4_503_599_627_370_497.0
        );
        assert_eq!(math_pow(f64::NAN, 0.0), 1.0);
        assert!(math_pow(1.0, f64::INFINITY).is_nan());
    }
}
