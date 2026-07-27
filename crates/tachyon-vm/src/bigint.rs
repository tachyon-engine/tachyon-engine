//! Canonical arbitrary-precision BigInt payload and primitive helpers.

use super::*;

const DECIMAL_CHUNK_BASE: u64 = 1_000_000_000;
const DECIMAL_CHUNK_DIGITS: usize = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BigIntBuildError {
    InvalidDecimal,
    AllocationFailed,
}

/// Canonical sign-magnitude BigInt with little-endian fixed-capacity limbs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BigIntValue {
    negative: bool,
    limbs: Box<[u64]>,
}

impl BigIntValue {
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
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
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
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
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
}

fn bigint_build_error(error: BigIntBuildError) -> ExecutionError {
    match error {
        BigIntBuildError::InvalidDecimal => ExecutionError::InvalidBigIntLiteral,
        BigIntBuildError::AllocationFailed => ExecutionError::BigIntAllocationFailed,
    }
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
}
