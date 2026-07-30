//! Canonical arbitrary-precision BigInt payload and primitive helpers.

use super::*;

const DECIMAL_CHUNK_BASE: u64 = 1_000_000_000;
const DECIMAL_CHUNK_DIGITS: usize = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BigIntBuildError {
    InvalidDecimal,
    AllocationFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BigIntArithmeticError {
    AllocationFailed,
    DivisionByZero,
    NegativeExponent,
    ResultTooLarge,
}

/// Canonical sign-magnitude BigInt with little-endian fixed-capacity limbs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BigIntValue {
    negative: bool,
    limbs: Box<[u64]>,
}

impl BigIntValue {
    /// Canonicalizes one owned magnitude before it becomes published GC payload.
    #[inline]
    fn from_owned_limbs(negative: bool, mut limbs: Vec<u64>) -> Self {
        while limbs.last() == Some(&0) {
            limbs.pop();
        }
        Self {
            negative: negative && !limbs.is_empty(),
            limbs: limbs.into_boxed_slice(),
        }
    }

    /// Builds one canonical unsigned magnitude without decimal parsing.
    #[inline(always)]
    fn from_u64(value: u64) -> Self {
        if value == 0 {
            Self {
                negative: false,
                limbs: Box::default(),
            }
        } else {
            Self {
                negative: false,
                limbs: Box::new([value]),
            }
        }
    }

    /// Builds one canonical signed machine integer without passing through Number.
    #[inline(always)]
    fn from_i64(value: i64) -> Self {
        let mut bigint = Self::from_u64(value.unsigned_abs());
        bigint.negative = value.is_negative();
        bigint
    }

    /// Parses one canonical or signed decimal integer into exact persistent limb storage.
    pub(crate) fn from_decimal(text: &str) -> Result<Self, BigIntBuildError> {
        let (negative, digits) = text
            .strip_prefix('-')
            .map_or((false, text), |digits| (true, digits));
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(BigIntBuildError::InvalidDecimal);
        }
        let significant = digits.trim_start_matches('0');
        if significant.is_empty() {
            return Ok(Self {
                negative: false,
                limbs: Box::default(),
            });
        }
        let estimated_limbs = significant
            .len()
            .checked_mul(3_322)
            .and_then(|bits_milli| bits_milli.checked_add(63_999))
            .map(|bits_milli| bits_milli / 64_000)
            .ok_or(BigIntBuildError::AllocationFailed)?
            .max(1);
        let mut limbs = Vec::new();
        limbs
            .try_reserve_exact(estimated_limbs)
            .map_err(|_| BigIntBuildError::AllocationFailed)?;
        limbs.push(0_u64);
        for digit in significant.bytes() {
            let mut carry = u128::from(digit - b'0');
            for limb in &mut limbs {
                let product = u128::from(*limb) * 10 + carry;
                *limb = product as u64;
                carry = product >> 64;
            }
            if carry != 0 {
                limbs.push(carry as u64);
            }
        }
        debug_assert!(limbs.last().is_some_and(|limb| *limb != 0));
        Ok(Self {
            negative,
            limbs: limbs.into_boxed_slice(),
        })
    }

    /// Parses validated ASCII digits in one radix into canonical persistent limb storage.
    fn from_radix_digits(
        digits: &[u16],
        radix: u32,
        negative: bool,
    ) -> Result<Self, BigIntBuildError> {
        if digits.is_empty() {
            return Err(BigIntBuildError::InvalidDecimal);
        }
        let bits_per_digit = match radix {
            2 => 1,
            8 => 3,
            10 => 4,
            16 => 4,
            _ => return Err(BigIntBuildError::InvalidDecimal),
        };
        let estimated_limbs = digits
            .len()
            .checked_mul(bits_per_digit)
            .and_then(|bits| bits.checked_add(63))
            .map(|bits| bits / 64)
            .ok_or(BigIntBuildError::AllocationFailed)?
            .max(1);
        let mut limbs = Vec::new();
        limbs
            .try_reserve_exact(estimated_limbs)
            .map_err(|_| BigIntBuildError::AllocationFailed)?;
        limbs.push(0_u64);
        for &unit in digits {
            let digit = ascii_radix_digit(unit, radix).ok_or(BigIntBuildError::InvalidDecimal)?;
            let mut carry = u128::from(digit);
            for limb in &mut limbs {
                let product = u128::from(*limb) * u128::from(radix) + carry;
                *limb = product as u64;
                carry = product >> 64;
            }
            if carry != 0 {
                limbs.push(carry as u64);
            }
        }
        while limbs.last() == Some(&0) {
            limbs.pop();
        }
        Ok(Self {
            negative: negative && !limbs.is_empty(),
            limbs: limbs.into_boxed_slice(),
        })
    }

    /// Decodes one integral binary64 value exactly, without decimal formatting or narrowing.
    fn from_integral_f64(number: f64) -> Result<Self, BigIntBuildError> {
        if !number.is_finite() || number.fract() != 0.0 {
            return Err(BigIntBuildError::InvalidDecimal);
        }
        if number == 0.0 {
            return Ok(Self::from_u64(0));
        }
        let bits = number.to_bits();
        let negative = bits >> 63 != 0;
        let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
        let significand = (bits & ((1_u64 << 52) - 1)) | (1_u64 << 52);
        let shift = exponent - 52;
        if shift < 0 {
            let magnitude = significand >> (-shift as u32);
            let mut result = Self::from_u64(magnitude);
            result.negative = negative;
            return Ok(result);
        }
        let word_shift =
            usize::try_from(shift / 64).map_err(|_| BigIntBuildError::AllocationFailed)?;
        let bit_shift = (shift % 64) as u32;
        let limb_count = word_shift
            .checked_add(1 + usize::from(bit_shift != 0 && significand >> (64 - bit_shift) != 0))
            .ok_or(BigIntBuildError::AllocationFailed)?;
        let mut limbs = Vec::new();
        limbs
            .try_reserve_exact(limb_count)
            .map_err(|_| BigIntBuildError::AllocationFailed)?;
        limbs.resize(word_shift, 0);
        limbs.push(significand << bit_shift);
        if bit_shift != 0 {
            let high = significand >> (64 - bit_shift);
            if high != 0 {
                limbs.push(high);
            }
        }
        Ok(Self {
            negative,
            limbs: limbs.into_boxed_slice(),
        })
    }

    /// Returns the immediate representation when the mathematical value fits signed 48 bits.
    pub(crate) fn small_value(&self) -> Option<i64> {
        let magnitude = match self.limbs.as_ref() {
            [] => return Some(0),
            [magnitude] => *magnitude,
            _ => return None,
        };
        if self.negative {
            if magnitude <= (1_u64 << 47) {
                Some(-(magnitude as i64))
            } else {
                None
            }
        } else if magnitude < (1_u64 << 47) {
            Some(magnitude as i64)
        } else {
            None
        }
    }

    /// Returns the exact mathematical negation while preserving canonical zero.
    #[inline(always)]
    fn negate(mut self) -> Self {
        if !self.limbs.is_empty() {
            self.negative = !self.negative;
        }
        self
    }

    /// Computes mathematical modulo 2^64 without converting through Number.
    #[inline(always)]
    pub(crate) fn modulo_u64(&self) -> u64 {
        let low = self.limbs.first().copied().unwrap_or(0);
        if self.negative {
            0_u64.wrapping_sub(low)
        } else {
            low
        }
    }

    /// Converts the mathematical integer to binary64 for the explicit Number(BigInt) exception.
    #[inline]
    fn to_f64(&self) -> f64 {
        const LIMB_SCALE: f64 = 18_446_744_073_709_551_616.0;
        let mut number = 0.0;
        for &limb in self.limbs.iter().rev() {
            number = number * LIMB_SCALE + limb as f64;
        }
        if self.negative { -number } else { number }
    }

    #[inline(always)]
    fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    #[inline(always)]
    fn is_one(&self) -> bool {
        !self.negative && self.limbs.as_ref() == [1]
    }

    #[inline(always)]
    fn bit_length(&self) -> usize {
        self.limbs.last().map_or(0, |high| {
            (self.limbs.len() - 1) * u64::BITS as usize
                + (u64::BITS - high.leading_zeros()) as usize
        })
    }

    #[inline(always)]
    fn magnitude_cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.limbs
            .len()
            .cmp(&other.limbs.len())
            .then_with(|| self.limbs.iter().rev().cmp(other.limbs.iter().rev()))
    }

    /// Adds two canonical values with one exact result-capacity allocation.
    fn add(&self, other: &Self) -> Result<Self, BigIntArithmeticError> {
        if self.negative == other.negative {
            return add_magnitudes(&self.limbs, &other.limbs)
                .map(|limbs| Self::from_owned_limbs(self.negative, limbs));
        }
        match self.magnitude_cmp(other) {
            core::cmp::Ordering::Greater => subtract_magnitudes(&self.limbs, &other.limbs)
                .map(|limbs| Self::from_owned_limbs(self.negative, limbs)),
            core::cmp::Ordering::Less => subtract_magnitudes(&other.limbs, &self.limbs)
                .map(|limbs| Self::from_owned_limbs(other.negative, limbs)),
            core::cmp::Ordering::Equal => Ok(Self::from_u64(0)),
        }
    }

    #[inline]
    fn subtract(&self, other: &Self) -> Result<Self, BigIntArithmeticError> {
        self.add(&other.clone().negate())
    }

    /// Multiplies canonical magnitudes with checked exact capacity and sign publication.
    fn multiply(&self, other: &Self) -> Result<Self, BigIntArithmeticError> {
        if self.is_zero() || other.is_zero() {
            return Ok(Self::from_u64(0));
        }
        let limb_count = self
            .limbs
            .len()
            .checked_add(other.limbs.len())
            .ok_or(BigIntArithmeticError::ResultTooLarge)?;
        ensure_result_limbs(limb_count)?;
        let mut limbs = zeroed_limbs(limb_count)?;
        for (left_index, &left) in self.limbs.iter().enumerate() {
            let mut carry = 0_u128;
            for (right_index, &right) in other.limbs.iter().enumerate() {
                let index = left_index + right_index;
                let product =
                    u128::from(left) * u128::from(right) + u128::from(limbs[index]) + carry;
                limbs[index] = product as u64;
                carry = product >> 64;
            }
            limbs[left_index + other.limbs.len()] = carry as u64;
        }
        Ok(Self::from_owned_limbs(
            self.negative != other.negative,
            limbs,
        ))
    }

    /// Computes truncating quotient and dividend-signed remainder without host integer narrowing.
    fn divide_or_remainder(
        &self,
        other: &Self,
        remainder: bool,
    ) -> Result<Self, BigIntArithmeticError> {
        if other.is_zero() {
            return Err(BigIntArithmeticError::DivisionByZero);
        }
        if self.is_zero() {
            return Ok(Self::from_u64(0));
        }
        let ordering = self.magnitude_cmp(other);
        if ordering == core::cmp::Ordering::Less {
            return if remainder {
                Ok(self.clone())
            } else {
                Ok(Self::from_u64(0))
            };
        }
        if other.limbs.len() == 1 {
            let (quotient, residual) = divide_magnitude_by_limb(&self.limbs, other.limbs[0])?;
            return if remainder {
                Ok(Self::from_owned_limbs(self.negative, vec![residual]))
            } else {
                Ok(Self::from_owned_limbs(
                    self.negative != other.negative,
                    quotient,
                ))
            };
        }
        let (quotient, residual) = divide_magnitudes(&self.limbs, &other.limbs)?;
        if remainder {
            Ok(Self::from_owned_limbs(self.negative, residual))
        } else {
            Ok(Self::from_owned_limbs(
                self.negative != other.negative,
                quotient,
            ))
        }
    }

    /// Raises one value by a non-negative exponent using checked squaring and a result-bit cap.
    fn exponentiate(&self, exponent: &Self) -> Result<Self, BigIntArithmeticError> {
        if exponent.negative {
            return Err(BigIntArithmeticError::NegativeExponent);
        }
        if exponent.is_zero() {
            return Ok(Self::from_u64(1));
        }
        if self.is_zero() || self.is_one() {
            return Ok(self.clone());
        }
        if self.negative && self.limbs.as_ref() == [1] {
            return Ok(Self::from_i64(if exponent.limbs[0] & 1 == 0 {
                1
            } else {
                -1
            }));
        }
        let exponent = exponent.as_bounded_usize(tuning::bigints::MAX_RESULT_BITS)?;
        let estimated_bits = self
            .bit_length()
            .checked_mul(exponent)
            .ok_or(BigIntArithmeticError::ResultTooLarge)?;
        if estimated_bits > tuning::bigints::MAX_RESULT_BITS {
            return Err(BigIntArithmeticError::ResultTooLarge);
        }
        let mut power = self.clone();
        let mut result = Self::from_u64(1);
        let mut remaining = exponent;
        while remaining != 0 {
            if remaining & 1 != 0 {
                result = result.multiply(&power)?;
            }
            remaining >>= 1;
            if remaining != 0 {
                power = power.multiply(&power)?;
            }
        }
        Ok(result)
    }

    /// Applies an infinite two's-complement binary operation through one explicit sign limb.
    fn bitwise(&self, other: &Self, opcode: Opcode) -> Result<Self, BigIntArithmeticError> {
        let width = self
            .limbs
            .len()
            .max(other.limbs.len())
            .checked_add(1)
            .ok_or(BigIntArithmeticError::ResultTooLarge)?;
        ensure_result_limbs(width)?;
        let left = self.twos_complement(width)?;
        let right = other.twos_complement(width)?;
        let mut result = zeroed_limbs(width)?;
        for index in 0..width {
            result[index] = match opcode {
                Opcode::BitwiseAnd => left[index] & right[index],
                Opcode::BitwiseOr => left[index] | right[index],
                Opcode::BitwiseXor => left[index] ^ right[index],
                _ => unreachable!("BigInt bitwise dispatch only supplies binary bitwise opcodes"),
            };
        }
        from_twos_complement(result)
    }

    /// Implements `~x` as `-x - 1`, avoiding a second temporary published BigInt.
    fn bitwise_not(&self) -> Result<Self, BigIntArithmeticError> {
        let one = Self::from_u64(1);
        self.add(&one)?.negate_checked()
    }

    #[inline]
    fn negate_checked(self) -> Result<Self, BigIntArithmeticError> {
        Ok(self.negate())
    }

    /// Applies signed BigInt shift counts, reversing direction for negative counts.
    fn shift(&self, count: &Self, left: bool) -> Result<Self, BigIntArithmeticError> {
        if self.is_zero() || count.is_zero() {
            return Ok(self.clone());
        }
        let effective_left = left != count.negative;
        if effective_left {
            let shift = count.as_bounded_usize(tuning::bigints::MAX_RESULT_BITS)?;
            return self.shift_left_magnitude(shift);
        }
        let Some(shift) = count.as_usize_if_fits() else {
            return Ok(Self::from_i64(if self.negative { -1 } else { 0 }));
        };
        self.shift_right_arithmetic(shift)
    }

    /// Materializes a bounded positive magnitude as usize for loop/allocation control.
    fn as_bounded_usize(&self, maximum: usize) -> Result<usize, BigIntArithmeticError> {
        let value = self
            .as_usize_if_fits()
            .ok_or(BigIntArithmeticError::ResultTooLarge)?;
        if value > maximum {
            return Err(BigIntArithmeticError::ResultTooLarge);
        }
        Ok(value)
    }

    #[inline(always)]
    fn as_usize_if_fits(&self) -> Option<usize> {
        match self.limbs.as_ref() {
            [] => Some(0),
            [value] => usize::try_from(*value).ok(),
            _ => None,
        }
    }

    /// Converts canonical sign-magnitude to fixed-width two's-complement limbs.
    fn twos_complement(&self, width: usize) -> Result<Vec<u64>, BigIntArithmeticError> {
        let mut result = zeroed_limbs(width)?;
        result[..self.limbs.len()].copy_from_slice(&self.limbs);
        if self.negative {
            for limb in &mut result {
                *limb = !*limb;
            }
            add_one_in_place(&mut result);
        }
        Ok(result)
    }

    /// Shifts one sign-magnitude value left with exact limb capacity.
    fn shift_left_magnitude(&self, shift: usize) -> Result<Self, BigIntArithmeticError> {
        let result_bits = self
            .bit_length()
            .checked_add(shift)
            .ok_or(BigIntArithmeticError::ResultTooLarge)?;
        if result_bits > tuning::bigints::MAX_RESULT_BITS {
            return Err(BigIntArithmeticError::ResultTooLarge);
        }
        let word_shift = shift / 64;
        let bit_shift = shift % 64;
        let limb_count = self
            .limbs
            .len()
            .checked_add(word_shift)
            .and_then(|count| count.checked_add(usize::from(bit_shift != 0)))
            .ok_or(BigIntArithmeticError::ResultTooLarge)?;
        let mut limbs = zeroed_limbs(limb_count)?;
        for (index, &limb) in self.limbs.iter().enumerate() {
            limbs[index + word_shift] |= limb << bit_shift;
            if bit_shift != 0 {
                limbs[index + word_shift + 1] |= limb >> (64 - bit_shift);
            }
        }
        Ok(Self::from_owned_limbs(self.negative, limbs))
    }

    /// Arithmetic-right-shifts sign magnitude, rounding negative values toward minus infinity.
    fn shift_right_arithmetic(&self, shift: usize) -> Result<Self, BigIntArithmeticError> {
        if shift >= self.bit_length() {
            return Ok(Self::from_i64(if self.negative { -1 } else { 0 }));
        }
        let word_shift = shift / 64;
        let bit_shift = shift % 64;
        let output_len = self.limbs.len() - word_shift;
        let mut limbs = zeroed_limbs(output_len)?;
        for (index, output) in limbs.iter_mut().enumerate() {
            let source = index + word_shift;
            *output = self.limbs[source] >> bit_shift;
            if bit_shift != 0 && source + 1 < self.limbs.len() {
                *output |= self.limbs[source + 1] << (64 - bit_shift);
            }
        }
        if self.negative && discarded_bits_nonzero(&self.limbs, shift) {
            add_one_in_place(&mut limbs);
        }
        Ok(Self::from_owned_limbs(self.negative, limbs))
    }

    /// Compares against one signed immediate without allocating a temporary magnitude.
    pub(crate) fn equals_small(&self, value: i64) -> bool {
        if value == 0 {
            return self.limbs.is_empty();
        }
        let negative = value.is_negative();
        let magnitude = value.unsigned_abs();
        self.negative == negative && self.limbs.as_ref() == [magnitude]
    }

    /// Formats canonical decimal bytes using an exact-capacity base-1e9 chunk plan.
    pub(crate) fn decimal_bytes(&self) -> Result<Vec<u8>, BigIntBuildError> {
        if self.limbs.is_empty() {
            return Ok(vec![b'0']);
        }
        let mut scratch = Vec::new();
        scratch
            .try_reserve_exact(self.limbs.len())
            .map_err(|_| BigIntBuildError::AllocationFailed)?;
        scratch.extend_from_slice(&self.limbs);
        let estimated_digits = self
            .limbs
            .len()
            .checked_mul(20)
            .ok_or(BigIntBuildError::AllocationFailed)?;
        let estimated_chunks = estimated_digits.div_ceil(DECIMAL_CHUNK_DIGITS);
        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(estimated_chunks)
            .map_err(|_| BigIntBuildError::AllocationFailed)?;
        while !scratch.is_empty() {
            let mut remainder = 0_u128;
            for limb in scratch.iter_mut().rev() {
                let dividend = (remainder << 64) | u128::from(*limb);
                *limb = (dividend / u128::from(DECIMAL_CHUNK_BASE)) as u64;
                remainder = dividend % u128::from(DECIMAL_CHUNK_BASE);
            }
            chunks.push(remainder as u32);
            while scratch.last() == Some(&0) {
                scratch.pop();
            }
        }
        let capacity = chunks
            .len()
            .checked_mul(DECIMAL_CHUNK_DIGITS)
            .and_then(|length| length.checked_add(usize::from(self.negative)))
            .ok_or(BigIntBuildError::AllocationFailed)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(capacity)
            .map_err(|_| BigIntBuildError::AllocationFailed)?;
        if self.negative {
            output.push(b'-');
        }
        let most_significant = chunks.pop().expect("non-zero BigInt produces one chunk");
        append_decimal_chunk(&mut output, most_significant, false);
        while let Some(chunk) = chunks.pop() {
            append_decimal_chunk(&mut output, chunk, true);
        }
        Ok(output)
    }

    /// Formats one canonical value in an ECMAScript BigInt radix without host narrowing.
    fn radix_bytes(&self, radix: u8) -> Result<Vec<u8>, BigIntBuildError> {
        debug_assert!((2..=36).contains(&radix));
        if self.is_zero() {
            return Ok(vec![b'0']);
        }
        let estimated_digits = self
            .bit_length()
            .checked_add(radix.ilog2() as usize)
            .ok_or(BigIntBuildError::AllocationFailed)?;
        let mut digits = Vec::new();
        digits
            .try_reserve_exact(estimated_digits)
            .map_err(|_| BigIntBuildError::AllocationFailed)?;
        let mut magnitude = self.limbs.to_vec();
        while !magnitude.is_empty() {
            let mut remainder = 0_u128;
            for limb in magnitude.iter_mut().rev() {
                let dividend = (remainder << 64) | u128::from(*limb);
                *limb = (dividend / u128::from(radix)) as u64;
                remainder = dividend % u128::from(radix);
            }
            while magnitude.last() == Some(&0) {
                magnitude.pop();
            }
            let digit = remainder as u8;
            digits.push(if digit < 10 {
                b'0' + digit
            } else {
                b'a' + digit - 10
            });
        }
        if self.negative {
            digits.push(b'-');
        }
        digits.reverse();
        Ok(digits)
    }

    /// Returns this value modulo 2^bits, optionally interpreting the retained sign bit.
    fn truncate_to_bits(&self, bits: usize, signed: bool) -> Result<Self, BigIntArithmeticError> {
        if bits == 0 {
            return Ok(Self::from_u64(0));
        }
        let limb_count = bits
            .checked_add(63)
            .map(|count| count / 64)
            .ok_or(BigIntArithmeticError::ResultTooLarge)?;
        ensure_result_limbs(limb_count)?;
        let mut residue = zeroed_limbs(limb_count)?;
        let copied = self.limbs.len().min(limb_count);
        residue[..copied].copy_from_slice(&self.limbs[..copied]);
        if self.negative {
            for limb in &mut residue {
                *limb = !*limb;
            }
            add_one_in_place(&mut residue);
        }
        let retained_bits = bits % 64;
        if retained_bits != 0 {
            residue[limb_count - 1] &= (1_u64 << retained_bits) - 1;
        }
        let sign_set = signed && {
            let sign_index = bits - 1;
            residue[sign_index / 64] & (1_u64 << (sign_index % 64)) != 0
        };
        if !sign_set {
            return Ok(Self::from_owned_limbs(false, residue));
        }
        let mut modulus = zeroed_limbs(limb_count + 1)?;
        modulus[bits / 64] = 1_u64 << (bits % 64);
        let magnitude = subtract_magnitudes(&modulus, &residue)?;
        Ok(Self::from_owned_limbs(true, magnitude))
    }
}

/// Adds unsigned little-endian magnitudes with exact capacity and one carry limb.
fn add_magnitudes(left: &[u64], right: &[u64]) -> Result<Vec<u64>, BigIntArithmeticError> {
    let common = left.len().max(right.len());
    let capacity = common
        .checked_add(1)
        .ok_or(BigIntArithmeticError::ResultTooLarge)?;
    ensure_result_limbs(capacity)?;
    let mut result = zeroed_limbs(capacity)?;
    let mut carry = 0_u128;
    for (index, output) in result.iter_mut().take(common).enumerate() {
        let sum = u128::from(left.get(index).copied().unwrap_or(0))
            + u128::from(right.get(index).copied().unwrap_or(0))
            + carry;
        *output = sum as u64;
        carry = sum >> 64;
    }
    result[common] = carry as u64;
    Ok(result)
}

/// Subtracts unsigned magnitudes after the caller proves `left >= right`.
fn subtract_magnitudes(left: &[u64], right: &[u64]) -> Result<Vec<u64>, BigIntArithmeticError> {
    debug_assert!(magnitude_slice_cmp(left, right) != core::cmp::Ordering::Less);
    let mut result = zeroed_limbs(left.len())?;
    let mut borrow = 0_u128;
    for (index, &left_limb) in left.iter().enumerate() {
        let subtrahend = u128::from(right.get(index).copied().unwrap_or(0)) + borrow;
        let minuend = u128::from(left_limb);
        result[index] = minuend.wrapping_sub(subtrahend) as u64;
        borrow = u128::from(minuend < subtrahend);
    }
    debug_assert_eq!(borrow, 0);
    Ok(result)
}

/// Divides an unsigned magnitude by one non-zero limb in a single high-to-low pass.
fn divide_magnitude_by_limb(
    dividend: &[u64],
    divisor: u64,
) -> Result<(Vec<u64>, u64), BigIntArithmeticError> {
    debug_assert_ne!(divisor, 0);
    let mut quotient = zeroed_limbs(dividend.len())?;
    let mut residual = 0_u128;
    for index in (0..dividend.len()).rev() {
        let current = (residual << 64) | u128::from(dividend[index]);
        quotient[index] = (current / u128::from(divisor)) as u64;
        residual = current % u128::from(divisor);
    }
    Ok((quotient, residual as u64))
}

/// Uses allocation-bounded binary long division for the multi-limb exact kernel.
///
/// The representation stays canonical at each subtraction, so no signed temporary or f64 path is
/// introduced. A future Knuth/Burnikel-Ziegler kernel can replace this function behind the same
/// contract without changing published payloads or VM dispatch.
fn divide_magnitudes(
    dividend: &[u64],
    divisor: &[u64],
) -> Result<(Vec<u64>, Vec<u64>), BigIntArithmeticError> {
    debug_assert!(divisor.len() > 1);
    debug_assert!(magnitude_slice_cmp(dividend, divisor) != core::cmp::Ordering::Less);
    let mut quotient = zeroed_limbs(dividend.len())?;
    let residual_capacity = divisor
        .len()
        .checked_add(1)
        .ok_or(BigIntArithmeticError::ResultTooLarge)?;
    let mut residual = Vec::new();
    residual
        .try_reserve_exact(residual_capacity)
        .map_err(|_| BigIntArithmeticError::AllocationFailed)?;
    let high = *dividend.last().expect("non-zero dividend has a high limb");
    let bit_length = (dividend.len() - 1) * 64 + (64 - high.leading_zeros() as usize);
    for bit_index in (0..bit_length).rev() {
        shift_magnitude_left_one(&mut residual);
        if dividend[bit_index / 64] & (1_u64 << (bit_index % 64)) != 0 {
            if residual.is_empty() {
                residual.push(1);
            } else {
                residual[0] |= 1;
            }
        }
        if magnitude_slice_cmp(&residual, divisor) != core::cmp::Ordering::Less {
            subtract_magnitude_in_place(&mut residual, divisor);
            quotient[bit_index / 64] |= 1_u64 << (bit_index % 64);
        }
    }
    Ok((quotient, residual))
}

#[inline(always)]
fn magnitude_slice_cmp(left: &[u64], right: &[u64]) -> core::cmp::Ordering {
    left.len()
        .cmp(&right.len())
        .then_with(|| left.iter().rev().cmp(right.iter().rev()))
}

/// Shifts one canonical scratch magnitude left by one without exceeding reserved capacity.
fn shift_magnitude_left_one(limbs: &mut Vec<u64>) {
    let mut carry = 0_u64;
    for limb in limbs.iter_mut() {
        let next = *limb >> 63;
        *limb = (*limb << 1) | carry;
        carry = next;
    }
    if carry != 0 {
        limbs.push(carry);
    }
}

/// Subtracts a smaller canonical magnitude from scratch storage and trims high zero limbs.
fn subtract_magnitude_in_place(left: &mut Vec<u64>, right: &[u64]) {
    let mut borrow = 0_u128;
    for (index, limb) in left.iter_mut().enumerate() {
        let subtrahend = u128::from(right.get(index).copied().unwrap_or(0)) + borrow;
        let minuend = u128::from(*limb);
        *limb = minuend.wrapping_sub(subtrahend) as u64;
        borrow = u128::from(minuend < subtrahend);
    }
    debug_assert_eq!(borrow, 0);
    while left.last() == Some(&0) {
        left.pop();
    }
}

/// Converts fixed-width two's-complement scratch limbs back to canonical sign magnitude.
fn from_twos_complement(mut limbs: Vec<u64>) -> Result<BigIntValue, BigIntArithmeticError> {
    let negative = limbs.last().is_some_and(|limb| limb >> 63 != 0);
    if negative {
        for limb in &mut limbs {
            *limb = !*limb;
        }
        add_one_in_place(&mut limbs);
    }
    Ok(BigIntValue::from_owned_limbs(negative, limbs))
}

#[inline(always)]
fn add_one_in_place(limbs: &mut [u64]) {
    for limb in limbs {
        let (next, overflow) = limb.overflowing_add(1);
        *limb = next;
        if !overflow {
            break;
        }
    }
}

/// Reports whether arithmetic-right-shift rounding must increment the retained magnitude.
fn discarded_bits_nonzero(limbs: &[u64], shift: usize) -> bool {
    let whole_limbs = shift / 64;
    if limbs.iter().take(whole_limbs).any(|limb| *limb != 0) {
        return true;
    }
    let partial = shift % 64;
    partial != 0
        && limbs
            .get(whole_limbs)
            .is_some_and(|limb| limb & ((1_u64 << partial) - 1) != 0)
}

#[inline]
fn zeroed_limbs(length: usize) -> Result<Vec<u64>, BigIntArithmeticError> {
    ensure_result_limbs(length)?;
    let mut limbs = Vec::new();
    limbs
        .try_reserve_exact(length)
        .map_err(|_| BigIntArithmeticError::AllocationFailed)?;
    limbs.resize(length, 0);
    Ok(limbs)
}

#[inline(always)]
fn ensure_result_limbs(length: usize) -> Result<(), BigIntArithmeticError> {
    let maximum = tuning::bigints::MAX_RESULT_BITS.div_ceil(64);
    if length > maximum {
        return Err(BigIntArithmeticError::ResultTooLarge);
    }
    Ok(())
}

impl Trace for BigIntValue {
    #[inline(always)]
    fn trace(&mut self, _tracer: &mut dyn Tracer) {}
}

impl GcExternalMemory for BigIntValue {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.limbs.len() * core::mem::size_of::<u64>()
    }
}

impl Isolate {
    /// Allocates a code constant while keeping earlier constants visible to a forced collection.
    pub(crate) fn allocate_bigint_code_constant(
        &mut self,
        text: &str,
        constant_values: &mut Vec<Option<Value>>,
    ) -> Result<Value, ExecutionError> {
        let bigint = BigIntValue::from_decimal(text).map_err(bigint_build_error)?;
        if let Some(value) = bigint.small_value() {
            return Ok(Value::from_small_bigint(value)
                .expect("BigIntValue small_value only returns signed 48-bit values"));
        }
        let mut roots = CodeLoadRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            constant_values,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.bigint,
                0,
                bigint,
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|bigint| Value::from_heap_ref(bigint.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Publishes one canonical BigInt into the immediate or exactly-accounted heap representation.
    pub(crate) fn allocate_bigint(&mut self, bigint: BigIntValue) -> Result<Value, ExecutionError> {
        if let Some(value) = bigint.small_value() {
            return Ok(Value::from_small_bigint(value)
                .expect("BigIntValue small_value only returns signed 48-bit values"));
        }
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.bigint,
                0,
                bigint,
                AllocationSpace::Young,
                roots,
            )
            .map(|bigint| Value::from_heap_ref(bigint.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Implements NumberToBigInt by decoding the exact represented binary64 integer.
    pub(crate) fn number_to_bigint(&mut self, number: f64) -> Result<Value, ExecutionError> {
        let bigint = BigIntValue::from_integral_f64(number)
            .map_err(|_| ExecutionError::InvalidBigIntNumber(Value::from_f64(number)))?;
        self.allocate_bigint(bigint)
    }

    /// Implements primitive ToBigInt after any observable ToPrimitive work has completed.
    pub(crate) fn primitive_to_bigint(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if self.is_bigint_value(value) {
            return Ok(value);
        }
        if let Some(immediate) = value.as_immediate() {
            return match immediate {
                Immediate::False => Ok(Value::from_small_bigint(0).expect("zero fits BigInt")),
                Immediate::True => Ok(Value::from_small_bigint(1).expect("one fits BigInt")),
                Immediate::Undefined
                | Immediate::Null
                | Immediate::Hole
                | Immediate::Uninitialized => {
                    Err(ExecutionError::UnsupportedBigIntConversion(value))
                }
            };
        }
        if numeric_value(value).is_some() || self.is_symbol_value(value) {
            return Err(ExecutionError::UnsupportedBigIntConversion(value));
        }
        if self.is_string_value(value) {
            return self.string_to_bigint(value);
        }
        Err(ExecutionError::UnsupportedBigIntConversion(value))
    }

    /// Implements the BigInt function's Number exception after one number-hint ToPrimitive.
    pub(crate) fn bigint_constructor_primitive(
        &mut self,
        value: Value,
    ) -> Result<Value, ExecutionError> {
        if let Some(number) = numeric_value(value) {
            return self.number_to_bigint(number);
        }
        self.primitive_to_bigint(value)
    }

    /// Copies one rooted string, parses StringIntegerLiteral, and publishes a canonical BigInt.
    fn string_to_bigint(&mut self, value: Value) -> Result<Value, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedBigIntConversion(value))?;
        let string = self
            .heap
            .checked_reference(raw, self.types.string)
            .map_err(|_| ExecutionError::UnsupportedBigIntConversion(value))?;
        let units = self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let string = no_gc
                    .borrow(string, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok::<_, ExecutionError>(match string.as_view() {
                    JsStringView::Latin1(bytes) => {
                        bytes.iter().map(|&byte| u16::from(byte)).collect()
                    }
                    JsStringView::Utf16(units) => units.to_vec(),
                })
            })
        })?;
        let bigint = parse_string_integer_literal(&units).map_err(bigint_build_error)?;
        self.allocate_bigint(bigint)
    }

    /// Decodes one 64-bit TypedArray element into the canonical primitive representation.
    pub(crate) fn allocate_bigint_bits(
        &mut self,
        bits: u64,
        signed: bool,
    ) -> Result<Value, ExecutionError> {
        let immediate = if signed {
            Value::from_small_bigint(bits as i64)
        } else {
            i64::try_from(bits).ok().and_then(Value::from_small_bigint)
        };
        if let Some(value) = immediate {
            return Ok(value);
        }
        let bigint = if signed {
            BigIntValue::from_i64(bits as i64)
        } else {
            BigIntValue::from_u64(bits)
        };
        self.allocate_bigint(bigint)
    }

    /// Negates either canonical representation, allocating only when the result cannot stay inline.
    pub(crate) fn negate_bigint(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if let Some(small) = value.as_small_bigint() {
            if let Some(negated) = small.checked_neg().and_then(Value::from_small_bigint) {
                return Ok(negated);
            }
            return self.allocate_bigint(
                BigIntValue::from_decimal("140737488355328")
                    .expect("signed 48-bit boundary is a valid decimal"),
            );
        }
        let bigint = self.bigint_reference(value)?;
        let bigint = self.heap.with_running_scope(|scope| {
            let bigint = scope.root(bigint).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(bigint, self.types.bigint)
                    .cloned()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        self.allocate_bigint(bigint.negate())
    }

    /// Applies one BigInt binary opcode after ToNumeric has classified both operands as BigInt.
    pub(crate) fn bigint_binary_operation(
        &mut self,
        opcode: Opcode,
        left: Value,
        right: Value,
    ) -> Result<Value, ExecutionError> {
        if opcode == Opcode::ShiftRightUnsigned {
            return Err(ExecutionError::BigIntUnsignedRightShift);
        }
        let left = self.bigint_value_snapshot(left)?;
        let right = self.bigint_value_snapshot(right)?;
        let result = match opcode {
            Opcode::Add => left.add(&right),
            Opcode::Sub => left.subtract(&right),
            Opcode::Mul => left.multiply(&right),
            Opcode::Div => left.divide_or_remainder(&right, false),
            Opcode::Remainder => left.divide_or_remainder(&right, true),
            Opcode::Exponentiate => left.exponentiate(&right),
            Opcode::BitwiseAnd | Opcode::BitwiseOr | Opcode::BitwiseXor => {
                left.bitwise(&right, opcode)
            }
            Opcode::ShiftLeft => left.shift(&right, true),
            Opcode::ShiftRight => left.shift(&right, false),
            Opcode::ShiftRightUnsigned => unreachable!("unsigned shift rejects before snapshot"),
            _ => unreachable!("BigInt binary dispatch received a non-arithmetic opcode"),
        }
        .map_err(bigint_arithmetic_error)?;
        self.allocate_bigint(result)
    }

    /// Complements one BigInt using the same canonical payload and allocation boundary.
    pub(crate) fn bigint_bitwise_not(&mut self, value: Value) -> Result<Value, ExecutionError> {
        let value = self.bigint_value_snapshot(value)?;
        let result = value.bitwise_not().map_err(bigint_arithmetic_error)?;
        self.allocate_bigint(result)
    }

    #[inline(always)]
    pub(crate) fn is_bigint_value(&self, value: Value) -> bool {
        value.as_small_bigint().is_some()
            || value
                .as_heap_ref()
                .is_some_and(|raw| self.heap.checked_reference(raw, self.types.bigint).is_ok())
    }

    /// Compares two already-classified BigInts by mathematical value.
    pub(crate) fn bigint_equal(
        &mut self,
        left: Value,
        right: Value,
    ) -> Result<bool, ExecutionError> {
        match (left.as_small_bigint(), right.as_small_bigint()) {
            (Some(left), Some(right)) => return Ok(left == right),
            (Some(small), None) => return self.heap_bigint_equals_small(right, small),
            (None, Some(small)) => return self.heap_bigint_equals_small(left, small),
            (None, None) => {}
        }
        let left = self.bigint_reference(left)?;
        let right = self.bigint_reference(right)?;
        self.heap.with_running_scope(|scope| {
            let left = scope.root(left).map_err(ExecutionError::Root)?;
            let right = scope.root(right).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let left = no_gc
                    .borrow(left, self.types.bigint)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let right = no_gc
                    .borrow(right, self.types.bigint)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(left == right)
            })
        })
    }

    /// Returns canonical decimal bytes for either BigInt representation.
    pub(crate) fn bigint_decimal_bytes(&mut self, value: Value) -> Result<Vec<u8>, ExecutionError> {
        if let Some(small) = value.as_small_bigint() {
            return Ok(decimal_i64_bytes(small));
        }
        let bigint = self.bigint_reference(value)?;
        self.heap.with_running_scope(|scope| {
            let bigint = scope.root(bigint).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(bigint, self.types.bigint)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .decimal_bytes()
                    .map_err(bigint_build_error)
            })
        })
    }

    /// Returns canonical lower-case digits for either BigInt representation and radix 2..=36.
    pub(crate) fn bigint_radix_bytes(
        &mut self,
        value: Value,
        radix: u8,
    ) -> Result<Vec<u8>, ExecutionError> {
        let bigint = self.bigint_value_snapshot(value)?;
        bigint.radix_bytes(radix).map_err(bigint_build_error)
    }

    /// Implements BigInt.asIntN/asUintN after ToIndex and ToBigInt have completed.
    pub(crate) fn bigint_as_n(
        &mut self,
        bits: usize,
        value: Value,
        signed: bool,
    ) -> Result<Value, ExecutionError> {
        let bigint = self.bigint_value_snapshot(value)?;
        let result = bigint
            .truncate_to_bits(bits, signed)
            .map_err(bigint_arithmetic_error)?;
        self.allocate_bigint(result)
    }

    /// Returns the low 64 bits required by BigInt typed-array and DataView encoders.
    pub(crate) fn bigint_modulo_u64(&mut self, value: Value) -> Result<u64, ExecutionError> {
        if let Some(small) = value.as_small_bigint() {
            return Ok(small as u64);
        }
        let bigint = self.bigint_reference(value)?;
        self.heap.with_running_scope(|scope| {
            let bigint = scope.root(bigint).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(bigint, self.types.bigint)
                    .map(BigIntValue::modulo_u64)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Implements the Number constructor's deliberate BigInt-to-Number conversion exception.
    pub(crate) fn bigint_to_number_value(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if let Some(small) = value.as_small_bigint() {
            return Ok(Value::from_f64(small as f64));
        }
        let bigint = self.bigint_reference(value)?;
        self.heap.with_running_scope(|scope| {
            let bigint = scope.root(bigint).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(bigint, self.types.bigint)
                    .map(|bigint| Value::from_f64(bigint.to_f64()))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn heap_bigint_equals_small(
        &mut self,
        heap_value: Value,
        small: i64,
    ) -> Result<bool, ExecutionError> {
        let bigint = self.bigint_reference(heap_value)?;
        self.heap.with_running_scope(|scope| {
            let bigint = scope.root(bigint).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(bigint, self.types.bigint)
                    .map(|bigint| bigint.equals_small(small))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn bigint_reference(&self, value: Value) -> Result<GcRef<BigIntValue>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::InvalidBigIntValue(value))?;
        self.heap
            .checked_reference(raw, self.types.bigint)
            .map_err(|_| ExecutionError::InvalidBigIntValue(value))
    }

    /// Copies either primitive representation into one unrooted canonical arithmetic operand.
    fn bigint_value_snapshot(&mut self, value: Value) -> Result<BigIntValue, ExecutionError> {
        if let Some(small) = value.as_small_bigint() {
            return Ok(BigIntValue::from_i64(small));
        }
        let bigint = self.bigint_reference(value)?;
        self.heap.with_running_scope(|scope| {
            let bigint = scope.root(bigint).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(bigint, self.types.bigint)
                    .cloned()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }
}

fn bigint_build_error(error: BigIntBuildError) -> ExecutionError {
    match error {
        BigIntBuildError::InvalidDecimal => ExecutionError::InvalidBigIntLiteral,
        BigIntBuildError::AllocationFailed => ExecutionError::BigIntAllocationFailed,
    }
}

#[inline(always)]
fn bigint_arithmetic_error(error: BigIntArithmeticError) -> ExecutionError {
    match error {
        BigIntArithmeticError::AllocationFailed => ExecutionError::BigIntAllocationFailed,
        BigIntArithmeticError::DivisionByZero => ExecutionError::BigIntDivisionByZero,
        BigIntArithmeticError::NegativeExponent => ExecutionError::BigIntNegativeExponent,
        BigIntArithmeticError::ResultTooLarge => ExecutionError::BigIntResultTooLarge,
    }
}

/// Executes allocation-free SmallBigInt success cases inside the verified hot kernel.
///
/// `None` deliberately covers semantic errors and canonical heap fallback, ending the unsafe
/// register epoch before error construction or GC allocation.
#[inline(always)]
pub(crate) fn small_bigint_binary_hot(opcode: Opcode, left: Value, right: Value) -> Option<Value> {
    let left = left.as_small_bigint()?;
    let right = right.as_small_bigint()?;
    let result = match opcode {
        Opcode::Add => left.checked_add(right),
        Opcode::Sub => left.checked_sub(right),
        Opcode::Mul => left.checked_mul(right),
        Opcode::Div => left.checked_div(right),
        Opcode::Remainder if right != 0 => Some(left % right),
        Opcode::Exponentiate if right >= 0 => u32::try_from(right)
            .ok()
            .and_then(|exponent| left.checked_pow(exponent)),
        Opcode::BitwiseAnd => Some(left & right),
        Opcode::BitwiseOr => Some(left | right),
        Opcode::BitwiseXor => Some(left ^ right),
        Opcode::ShiftLeft => small_bigint_shift(left, right, true),
        Opcode::ShiftRight => small_bigint_shift(left, right, false),
        Opcode::ShiftRightUnsigned => None,
        _ => None,
    }?;
    Value::from_small_bigint(result)
}

#[inline(always)]
pub(crate) fn small_bigint_not_hot(value: Value) -> Option<Value> {
    Value::from_small_bigint(!value.as_small_bigint()?)
}

/// Applies BigInt shift-direction reversal while retaining only immediate results.
#[inline(always)]
fn small_bigint_shift(value: i64, count: i64, left: bool) -> Option<i64> {
    let effective_left = left != count.is_negative();
    let magnitude = count.unsigned_abs();
    if !effective_left {
        return Some(if magnitude >= 64 {
            if value.is_negative() { -1 } else { 0 }
        } else {
            value >> magnitude as u32
        });
    }
    let shift = u32::try_from(magnitude).ok()?;
    1_i64
        .checked_shl(shift)
        .and_then(|factor| value.checked_mul(factor))
}

/// Parses the ECMAScript StringIntegerLiteral grammar after trimming String whitespace.
fn parse_string_integer_literal(units: &[u16]) -> Result<BigIntValue, BigIntBuildError> {
    let mut start = 0;
    let mut end = units.len();
    while start < end && is_ecmascript_whitespace(units[start]) {
        start += 1;
    }
    while end > start && is_ecmascript_whitespace(units[end - 1]) {
        end -= 1;
    }
    let text = &units[start..end];
    if text.is_empty() {
        return Ok(BigIntValue::from_u64(0));
    }
    let (negative, unsigned) = match text.first().copied() {
        Some(unit) if unit == u16::from(b'+') => (false, &text[1..]),
        Some(unit) if unit == u16::from(b'-') => (true, &text[1..]),
        _ => (false, text),
    };
    if unsigned.is_empty() {
        return Err(BigIntBuildError::InvalidDecimal);
    }
    let (radix, digits) = if !negative && unsigned.len() >= 2 && unsigned[0] == u16::from(b'0') {
        match unsigned[1] {
            unit if unit == u16::from(b'b') || unit == u16::from(b'B') => (2, &unsigned[2..]),
            unit if unit == u16::from(b'o') || unit == u16::from(b'O') => (8, &unsigned[2..]),
            unit if unit == u16::from(b'x') || unit == u16::from(b'X') => (16, &unsigned[2..]),
            _ => (10, unsigned),
        }
    } else {
        (10, unsigned)
    };
    if radix != 10 && text.first().is_some_and(|unit| *unit == u16::from(b'+')) {
        return Err(BigIntBuildError::InvalidDecimal);
    }
    BigIntValue::from_radix_digits(digits, radix, negative)
}

#[inline(always)]
fn ascii_radix_digit(unit: u16, radix: u32) -> Option<u32> {
    let digit = match unit {
        unit if (u16::from(b'0')..=u16::from(b'9')).contains(&unit) => {
            u32::from(unit - u16::from(b'0'))
        }
        unit if (u16::from(b'a')..=u16::from(b'f')).contains(&unit) => {
            u32::from(unit - u16::from(b'a')) + 10
        }
        unit if (u16::from(b'A')..=u16::from(b'F')).contains(&unit) => {
            u32::from(unit - u16::from(b'A')) + 10
        }
        _ => return None,
    };
    (digit < radix).then_some(digit)
}

/// Covers the WhiteSpace and LineTerminator code points accepted by StringIntegerLiteral.
#[inline(always)]
fn is_ecmascript_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x0009 | 0x000a | 0x000b | 0x000c | 0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000
            ..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

/// Appends one base-1e9 chunk, padding non-leading chunks to exactly nine digits.
fn append_decimal_chunk(output: &mut Vec<u8>, mut chunk: u32, padded: bool) {
    let mut digits = [b'0'; DECIMAL_CHUNK_DIGITS];
    let mut cursor = digits.len();
    loop {
        cursor -= 1;
        digits[cursor] = b'0' + (chunk % 10) as u8;
        chunk /= 10;
        if chunk == 0 {
            break;
        }
    }
    output.extend_from_slice(if padded { &digits } else { &digits[cursor..] });
}

fn decimal_i64_bytes(value: i64) -> Vec<u8> {
    let mut digits = [b'0'; 20];
    let mut cursor = digits.len();
    let mut magnitude = value.unsigned_abs();
    loop {
        cursor -= 1;
        digits[cursor] = b'0' + (magnitude % 10) as u8;
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    if value.is_negative() {
        cursor -= 1;
        digits[cursor] = b'-';
    }
    digits[cursor..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_roundtrip_and_modulo_cover_multi_limb_boundaries() {
        for (text, modulo) in [
            ("0", 0),
            ("140737488355327", 140_737_488_355_327),
            ("140737488355328", 140_737_488_355_328),
            ("18446744073709551615", u64::MAX),
            ("18446744073709551616", 0),
            ("340282366920938463463374607431768211455", u64::MAX),
            ("-18446744073709551617", u64::MAX),
        ] {
            let value = BigIntValue::from_decimal(text).expect("fixture parses");
            assert_eq!(value.decimal_bytes().unwrap(), text.as_bytes());
            assert_eq!(value.modulo_u64(), modulo);
            assert_eq!(
                value.external_memory_bytes(),
                value.limbs.len() * core::mem::size_of::<u64>()
            );
        }
    }

    #[test]
    fn parser_rejects_non_decimal_and_canonicalizes_zero() {
        for invalid in ["", "-", "+1", "1n", "1_0", "x"] {
            assert_eq!(
                BigIntValue::from_decimal(invalid),
                Err(BigIntBuildError::InvalidDecimal)
            );
        }
        let zero = BigIntValue::from_decimal("-000").unwrap();
        assert_eq!(zero.decimal_bytes().unwrap(), b"0");
        assert_eq!(zero.small_value(), Some(0));
    }

    #[test]
    fn string_integer_literal_covers_radices_whitespace_and_invalid_signs() {
        for (text, expected) in [
            ("", "0"),
            ("\u{00a0}\u{2028}123\u{3000}", "123"),
            ("+42", "42"),
            ("-42", "-42"),
            ("0b1111", "15"),
            ("0O70", "56"),
            ("0xfffffffffffffffffff", "75557863725914323419135"),
        ] {
            let units: Vec<u16> = text.encode_utf16().collect();
            let value = parse_string_integer_literal(&units).expect("fixture parses");
            assert_eq!(value.decimal_bytes().unwrap(), expected.as_bytes());
        }
        for invalid in ["+0x1", "-0x1", "0x", "0b2", "00x1", "1_0", "10n"] {
            let units: Vec<u16> = invalid.encode_utf16().collect();
            assert_eq!(
                parse_string_integer_literal(&units),
                Err(BigIntBuildError::InvalidDecimal),
                "{invalid} must not parse"
            );
        }
    }

    #[test]
    fn integral_binary64_conversion_preserves_the_represented_integer() {
        for (number, expected) in [
            (0.0, "0"),
            (-0.0, "0"),
            (9_007_199_254_740_994.0, "9007199254740994"),
            (-9_007_199_254_740_994.0, "-9007199254740994"),
            (
                f64::from_bits(((1023 + 100) as u64) << 52),
                "1267650600228229401496703205376",
            ),
        ] {
            let value = BigIntValue::from_integral_f64(number).expect("integral fixture converts");
            assert_eq!(value.decimal_bytes().unwrap(), expected.as_bytes());
        }
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1.5] {
            assert_eq!(
                BigIntValue::from_integral_f64(invalid),
                Err(BigIntBuildError::InvalidDecimal)
            );
        }
    }

    #[test]
    fn arithmetic_covers_signed_multi_limb_results_and_canonical_boundaries() {
        let left = decimal("340282366920938463463374607431768211455");
        let right = decimal("18446744073709551617");
        assert_decimal(
            left.add(&right).unwrap(),
            "340282366920938463481821351505477763072",
        );
        assert_decimal(
            left.subtract(&right).unwrap(),
            "340282366920938463444927863358058659838",
        );
        let product = right.multiply(&decimal("18446744073709551616")).unwrap();
        assert_decimal(product.clone(), "340282366920938463481821351505477763072");
        assert_decimal(
            product.divide_or_remainder(&right, false).unwrap(),
            "18446744073709551616",
        );
        assert_decimal(product.divide_or_remainder(&right, true).unwrap(), "0");
        assert_decimal(
            decimal("-340282366920938463481821351505477763073")
                .divide_or_remainder(&right, true)
                .unwrap(),
            "-1",
        );
        assert_decimal(
            decimal("140737488355327").add(&decimal("1")).unwrap(),
            "140737488355328",
        );
    }

    #[test]
    fn bitwise_shift_and_power_follow_infinite_twos_complement() {
        let value = decimal("24197857203266734881846307747534221840");
        assert_decimal(
            value.shift(&decimal("64"), true).unwrap(),
            "446371678960830626602075884953218503817583381441874493440",
        );
        assert_decimal(
            value.shift(&decimal("-64"), true).unwrap(),
            "1311768467463790320",
        );
        assert_decimal(decimal("-5").shift(&decimal("2"), false).unwrap(), "-2");
        assert_decimal(decimal("-5").shift(&decimal("-3"), false).unwrap(), "-40");
        assert_decimal(decimal("-1").bitwise_not().unwrap(), "0");
        assert_decimal(decimal("0").bitwise_not().unwrap(), "-1");
        let mask = decimal("18446744073709551615");
        let high = decimal("340282366920938463444927863358058659840");
        assert_decimal(high.bitwise(&mask, Opcode::BitwiseAnd).unwrap(), "0");
        assert_decimal(
            high.bitwise(&mask, Opcode::BitwiseOr).unwrap(),
            "340282366920938463463374607431768211455",
        );
        assert_decimal(
            decimal("-18446744073709551616")
                .bitwise(&mask, Opcode::BitwiseXor)
                .unwrap(),
            "-1",
        );
        assert_decimal(
            decimal("3").exponentiate(&decimal("100")).unwrap(),
            "515377520732011331036461129765621272702107522001",
        );
    }

    #[test]
    fn arithmetic_reports_spec_errors_and_resource_limits() {
        assert_eq!(
            decimal("1").divide_or_remainder(&decimal("0"), false),
            Err(BigIntArithmeticError::DivisionByZero)
        );
        assert_eq!(
            decimal("2").exponentiate(&decimal("-1")),
            Err(BigIntArithmeticError::NegativeExponent)
        );
        assert_eq!(
            decimal("2").shift(&decimal("16777216"), true),
            Err(BigIntArithmeticError::ResultTooLarge)
        );
        assert_decimal(
            decimal("-2")
                .shift(&decimal("18446744073709551616"), false)
                .unwrap(),
            "-1",
        );
    }

    fn decimal(text: &str) -> BigIntValue {
        BigIntValue::from_decimal(text).expect("arithmetic fixture parses")
    }

    fn assert_decimal(value: BigIntValue, expected: &str) {
        assert_eq!(value.decimal_bytes().unwrap(), expected.as_bytes());
        assert_eq!(
            value.negative,
            value.limbs.last().is_some() && expected.starts_with('-')
        );
        assert!(value.limbs.last().is_none_or(|limb| *limb != 0));
    }
}
