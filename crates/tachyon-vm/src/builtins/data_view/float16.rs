//! Deterministic IEEE 754 binary16 conversion for DataView and future Float16Array use.

const F64_SIGN_SHIFT: u32 = 63;
const F64_EXPONENT_SHIFT: u32 = 52;
const F64_EXPONENT_MASK: u64 = 0x7ff;
const F64_FRACTION_MASK: u64 = (1_u64 << F64_EXPONENT_SHIFT) - 1;
const F64_EXPONENT_BIAS: i32 = 1023;

const F16_SIGN_SHIFT: u32 = 15;
const F16_EXPONENT_SHIFT: u32 = 10;
const F16_EXPONENT_MASK: u16 = 0x1f;
const F16_FRACTION_MASK: u16 = (1_u16 << F16_EXPONENT_SHIFT) - 1;
const F16_EXPONENT_BIAS: i32 = 15;
const F16_INFINITY: u16 = F16_EXPONENT_MASK << F16_EXPONENT_SHIFT;
const F16_CANONICAL_NAN: u16 = F16_INFINITY | (1 << (F16_EXPONENT_SHIFT - 1));

/// Decodes one IEEE 754 binary16 bit pattern exactly into an ECMAScript Number.
#[inline(always)]
pub(super) fn decode_float16(bits: u16) -> f64 {
    let sign = u64::from(bits >> F16_SIGN_SHIFT) << F64_SIGN_SHIFT;
    let exponent = (bits >> F16_EXPONENT_SHIFT) & F16_EXPONENT_MASK;
    let fraction = bits & F16_FRACTION_MASK;

    let magnitude = match (exponent, fraction) {
        (0, 0) => 0,
        (0, fraction) => decode_subnormal_magnitude(fraction),
        (F16_EXPONENT_MASK, 0) => F64_EXPONENT_MASK << F64_EXPONENT_SHIFT,
        (F16_EXPONENT_MASK, _) => {
            (F64_EXPONENT_MASK << F64_EXPONENT_SHIFT) | (1 << (F64_EXPONENT_SHIFT - 1))
        }
        (exponent, fraction) => {
            let unbiased = i32::from(exponent) - F16_EXPONENT_BIAS;
            let f64_exponent = u64::try_from(unbiased + F64_EXPONENT_BIAS)
                .expect("binary16 normal exponents always fit binary64");
            (f64_exponent << F64_EXPONENT_SHIFT)
                | (u64::from(fraction) << (F64_EXPONENT_SHIFT - F16_EXPONENT_SHIFT))
        }
    };
    f64::from_bits(sign | magnitude)
}

/// Encodes an ECMAScript Number using IEEE round-to-nearest, ties-to-even.
#[inline(always)]
pub(super) fn encode_float16(number: f64) -> u16 {
    let bits = number.to_bits();
    let sign = ((bits >> F64_SIGN_SHIFT) as u16) << F16_SIGN_SHIFT;
    let exponent = ((bits >> F64_EXPONENT_SHIFT) & F64_EXPONENT_MASK) as u16;
    let fraction = bits & F64_FRACTION_MASK;

    if exponent == F64_EXPONENT_MASK as u16 {
        return sign
            | if fraction == 0 {
                F16_INFINITY
            } else {
                F16_CANONICAL_NAN
            };
    }
    if exponent == 0 {
        return sign;
    }

    let unbiased = i32::from(exponent) - F64_EXPONENT_BIAS;
    if unbiased > 15 {
        return sign | F16_INFINITY;
    }
    let significand = (1_u64 << F64_EXPONENT_SHIFT) | fraction;
    if unbiased >= -14 {
        return sign | encode_normal_magnitude(significand, unbiased);
    }
    sign | encode_subnormal_magnitude(significand, unbiased)
}

/// Normalizes a ten-bit binary16 subnormal into a binary64 exponent/fraction pair.
#[inline(always)]
fn decode_subnormal_magnitude(fraction: u16) -> u64 {
    let leading_bit = (u16::BITS - 1) - fraction.leading_zeros();
    let unbiased = i32::try_from(leading_bit).expect("binary16 bit index fits i32") - 24;
    let leading_mask = 1_u16 << leading_bit;
    let remainder = fraction ^ leading_mask;
    let f64_exponent = u64::try_from(unbiased + F64_EXPONENT_BIAS)
        .expect("binary16 subnormal exponents always fit binary64");
    (f64_exponent << F64_EXPONENT_SHIFT)
        | (u64::from(remainder) << (F64_EXPONENT_SHIFT - leading_bit))
}

/// Rounds a finite value in the normal binary16 exponent range.
#[inline(always)]
fn encode_normal_magnitude(significand: u64, unbiased: i32) -> u16 {
    let rounded = round_right_ties_even(significand, F64_EXPONENT_SHIFT - F16_EXPONENT_SHIFT);
    let mut exponent = unbiased + F16_EXPONENT_BIAS;
    let fraction = if rounded == (1 << (F16_EXPONENT_SHIFT + 1)) {
        exponent += 1;
        0
    } else {
        rounded as u16 & F16_FRACTION_MASK
    };
    if exponent >= i32::from(F16_EXPONENT_MASK) {
        F16_INFINITY
    } else {
        (exponent as u16) << F16_EXPONENT_SHIFT | fraction
    }
}

/// Rounds a finite value below the normal binary16 exponent range.
#[inline(always)]
fn encode_subnormal_magnitude(significand: u64, unbiased: i32) -> u16 {
    if unbiased < -25 {
        return 0;
    }
    let shift = u32::try_from(28 - unbiased).expect("binary16 subnormal shift fits u32");
    round_right_ties_even(significand, shift) as u16
}

/// Performs an unsigned right shift with deterministic nearest-even rounding.
#[inline(always)]
fn round_right_ties_even(value: u64, shift: u32) -> u64 {
    debug_assert!((1..u64::BITS).contains(&shift));
    let rounded = value >> shift;
    let remainder_mask = (1_u64 << shift) - 1;
    let remainder = value & remainder_mask;
    let halfway = 1_u64 << (shift - 1);
    rounded + u64::from(remainder > halfway || (remainder == halfway && rounded & 1 != 0))
}

#[cfg(test)]
#[path = "../../tests/data_view_float16.rs"]
mod tests;
