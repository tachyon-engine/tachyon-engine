//! Number construction, formatting, parsing, and allocation-free numeric operations.

use super::super::*;

impl Isolate {
    /// Implements the shared thisNumberValue brand check for Number prototype methods.
    pub(crate) fn this_number_value(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        if numeric_value(receiver).is_some() {
            return Ok(receiver);
        }
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(receiver))?;
        let number = self
            .heap
            .checked_reference(raw, self.types.number_object)
            .map_err(|_| ExecutionError::NotObject(receiver))?;
        self.heap.with_running_scope(|scope| {
            let number = scope.root(number).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(number, self.types.number_object)
                    .map(|number| number.number_data)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Applies the primitive subset of ToIntegerOrInfinity with an undefined default.
    fn integer_or_infinity_argument(
        &mut self,
        argument: Option<Value>,
        default: f64,
    ) -> Result<f64, ExecutionError> {
        let Some(argument) = argument else {
            return Ok(default);
        };
        if argument.as_immediate() == Some(Immediate::Undefined) {
            return Ok(default);
        }
        let converted = self.convert_to_number(argument)?;
        let number = numeric_value(converted)
            .ok_or(ExecutionError::UnsupportedNumberConversion(argument))?;
        if number.is_nan() || number == 0.0 {
            return Ok(0.0);
        }
        Ok(number.trunc())
    }

    /// Implements Number.prototype.toFixed with the pinned ECMAScript decimal formatter.
    pub(super) fn number_to_fixed(
        &mut self,
        receiver: Value,
        fraction_digits: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let number = self.this_number_value(receiver)?;
        let fraction_digits = self.integer_or_infinity_argument(fraction_digits, 0.0)?;
        if !(0.0..=100.0).contains(&fraction_digits) {
            return Err(ExecutionError::InvalidNumberPrecision(Value::from_f64(
                fraction_digits,
            )));
        }
        let number = numeric_value(number).expect("thisNumberValue always returns a number");
        let mut buffer = ryu_js::Buffer::new();
        let formatted = buffer.format_to_fixed(number, fraction_digits as u8);
        self.allocate_runtime_string(
            JsString::try_from_latin1(formatted.as_bytes())
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Implements Number.prototype.toExponential with exact binary-rational rounding.
    pub(super) fn number_to_exponential(
        &mut self,
        receiver: Value,
        fraction_digits: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let number = self.this_number_value(receiver)?;
        let fraction_digits = match fraction_digits {
            Some(value) if value.as_immediate() != Some(Immediate::Undefined) => {
                Some(self.integer_or_infinity_argument(Some(value), 0.0)?)
            }
            _ => None,
        };
        let number = numeric_value(number).expect("thisNumberValue always returns a number");
        if number.is_finite()
            && fraction_digits.is_some_and(|digits| !(0.0..=100.0).contains(&digits))
        {
            return Err(ExecutionError::InvalidNumberPrecision(Value::from_f64(
                fraction_digits.unwrap_or_default(),
            )));
        }
        let fraction_digits = fraction_digits.map(|digits| digits as u8);
        let mut buffer = [0; tuning::numbers::EXPONENTIAL_FORMAT_BUFFER_SIZE];
        let formatted =
            number::format_exponential(number, fraction_digits, &mut buffer).map_err(|error| {
                match error {
                    number::NumberFormatError::BufferExhausted => {
                        ExecutionError::NumberFormatBufferExhausted
                    }
                    number::NumberFormatError::InvalidDigit => {
                        ExecutionError::NumberFormatInvalidDigit
                    }
                }
            })?;
        self.allocate_runtime_string(
            JsString::try_from_latin1(formatted).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Implements Number.prototype.toPrecision with shared exact significant-digit rounding.
    pub(super) fn number_to_precision(
        &mut self,
        receiver: Value,
        precision: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let number = self.this_number_value(receiver)?;
        let Some(precision) =
            precision.filter(|value| value.as_immediate() != Some(Immediate::Undefined))
        else {
            let mut units = Vec::new();
            self.append_primitive_string_units(number, &mut units)?;
            return self.allocate_runtime_string(
                JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?,
            );
        };
        let precision = self.integer_or_infinity_argument(Some(precision), 0.0)?;
        let number = numeric_value(number).expect("thisNumberValue always returns a number");
        if number.is_finite() && !(1.0..=100.0).contains(&precision) {
            return Err(ExecutionError::InvalidNumberPrecision(Value::from_f64(
                precision,
            )));
        }
        let mut buffer = [0; tuning::numbers::EXPONENTIAL_FORMAT_BUFFER_SIZE];
        let formatted =
            number::format_precision(number, precision as u8, &mut buffer).map_err(|error| {
                match error {
                    number::NumberFormatError::BufferExhausted => {
                        ExecutionError::NumberFormatBufferExhausted
                    }
                    number::NumberFormatError::InvalidDigit => {
                        ExecutionError::NumberFormatInvalidDigit
                    }
                }
            })?;
        self.allocate_runtime_string(
            JsString::try_from_latin1(formatted).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Allocates a Number wrapper only after constructor argument conversion has completed.
    pub(super) fn box_number_from_constructor(
        &mut self,
        number: Value,
        new_target: Value,
    ) -> Result<Value, ExecutionError> {
        let prototype_atom = self.prototype_atom()?;
        let prototype = self
            .get_data_property(new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .unwrap_or_else(|| {
                self.realm
                    .number_prototype
                    .expect("Number prototype initializes before construction")
            });
        self.allocate_number_object(number, prototype, AllocationSpace::Young)
    }

    /// Implements Number::toString for decimal and shortest round-trip radix representations.
    pub(super) fn number_to_string(
        &mut self,
        receiver: Value,
        radix: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let number = self.this_number_value(receiver)?;
        let radix_number = if let Some(radix) = radix
            && radix.as_immediate() != Some(Immediate::Undefined)
        {
            let converted = self.convert_to_number(radix)?;
            let radix_number = numeric_value(converted)
                .ok_or(ExecutionError::UnsupportedNumberConversion(radix))?;
            let integer = if radix_number.is_nan() {
                0.0
            } else {
                radix_number.trunc()
            };
            if !(2.0..=36.0).contains(&integer) {
                return Err(ExecutionError::InvalidNumberRadix(radix));
            }
            integer as u8
        } else {
            10
        };
        let numeric = numeric_value(number).expect("thisNumberValue always returns a number");
        if radix_number != 10 && numeric.is_finite() && numeric != 0.0 {
            let mut buffer = [0; tuning::numbers::RADIX_FORMAT_BUFFER_SIZE];
            let bytes =
                number::format_radix(numeric, radix_number, &mut buffer).map_err(|error| {
                    match error {
                        number::NumberFormatError::BufferExhausted => {
                            ExecutionError::NumberFormatBufferExhausted
                        }
                        number::NumberFormatError::InvalidDigit => {
                            ExecutionError::NumberFormatInvalidDigit
                        }
                    }
                })?;
            return self.allocate_runtime_string(
                JsString::try_from_latin1(bytes).map_err(ExecutionError::PropertyKeyString)?,
            );
        }
        let mut units = Vec::new();
        self.append_primitive_string_units(number, &mut units)?;
        self.allocate_runtime_string(
            JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?,
        )
    }
}

#[derive(Clone, Copy)]
enum NumericInput {
    Int32(i32),
    Float(f64),
}

impl NumericInput {
    #[inline(always)]
    fn decode(value: Value) -> Option<Self> {
        value
            .as_i32()
            .map(Self::Int32)
            .or_else(|| value.as_f64().map(Self::Float))
    }

    #[inline(always)]
    fn into_f64(self) -> f64 {
        match self {
            Self::Int32(value) => f64::from(value),
            Self::Float(value) => value,
        }
    }
}

/// Applies one numeric-only binary opcode after both operands have completed ToNumber.
#[inline(always)]
pub(crate) fn numeric_binary_operation(opcode: Opcode, left: Value, right: Value) -> Value {
    match opcode {
        Opcode::Sub | Opcode::Mul | Opcode::Div => numeric_binary(opcode, left, right),
        Opcode::BitwiseAnd | Opcode::BitwiseOr | Opcode::BitwiseXor => {
            numeric_bitwise_binary(opcode, left, right)
        }
        Opcode::ShiftLeft | Opcode::ShiftRight | Opcode::ShiftRightUnsigned => {
            numeric_shift(opcode, left, right)
        }
        Opcode::Remainder | Opcode::Exponentiate => numeric_remainder_or_power(opcode, left, right),
        _ => unreachable!("numeric binary continuation received a non-numeric opcode"),
    }
}

#[inline(always)]
pub(crate) fn numeric_binary(opcode: Opcode, left: Value, right: Value) -> Value {
    let left = NumericInput::decode(left).unwrap_or(NumericInput::Float(f64::NAN));
    let right = NumericInput::decode(right).unwrap_or(NumericInput::Float(f64::NAN));
    numeric_binary_inputs(opcode, left, right)
}

#[inline(always)]
pub(crate) fn numeric_binary_hot(opcode: Opcode, left: Value, right: Value) -> Option<Value> {
    let left = NumericInput::decode(left)?;
    let right = NumericInput::decode(right)?;
    Some(numeric_binary_inputs(opcode, left, right))
}

/// Preserves int32 results when both already-classified operands fit the arithmetic operation.
#[inline(always)]
fn numeric_binary_inputs(opcode: Opcode, left: NumericInput, right: NumericInput) -> Value {
    if let (NumericInput::Int32(left), NumericInput::Int32(right)) = (left, right) {
        let integer = match opcode {
            Opcode::Add => left.checked_add(right),
            Opcode::Sub => left.checked_sub(right),
            Opcode::Mul => left.checked_mul(right),
            Opcode::Div if left.checked_rem(right) == Some(0) => left.checked_div(right),
            _ => None,
        };
        if let Some(integer) = integer {
            return Value::from_i32(integer);
        }
    }
    let left_number = left.into_f64();
    let right_number = right.into_f64();
    Value::from_f64(match opcode {
        Opcode::Add => left_number + right_number,
        Opcode::Sub => left_number - right_number,
        Opcode::Mul => left_number * right_number,
        Opcode::Div => left_number / right_number,
        _ => unreachable!("numeric binary dispatch only supplies arithmetic opcodes"),
    })
}

#[inline(always)]
pub(crate) fn numeric_negate(value: Value) -> Value {
    if let Some(integer) = value.as_i32() {
        if integer == 0 {
            return Value::from_f64(-0.0);
        }
        return integer
            .checked_neg()
            .map_or_else(|| Value::from_f64(-f64::from(integer)), Value::from_i32);
    }
    if let Some(number) = value.as_f64() {
        return Value::from_f64(-number);
    }
    Value::from_f64(match value.as_immediate() {
        Some(Immediate::Null | Immediate::False) => -0.0,
        Some(Immediate::True) => -1.0,
        _ => f64::NAN,
    })
}

/// Applies ECMAScript ToInt32 before complementing, including modulo-2^32 wrapping.
#[inline(always)]
pub(crate) fn numeric_bitwise_not(value: Value) -> Value {
    let number = value
        .as_i32()
        .map(f64::from)
        .or_else(|| value.as_f64())
        .unwrap_or(f64::NAN);
    let integer = if number.is_finite() && number != 0.0 {
        let modulo = number.trunc().rem_euclid(4_294_967_296.0);
        if modulo >= 2_147_483_648.0 {
            modulo - 4_294_967_296.0
        } else {
            modulo
        }
    } else {
        0.0
    };
    Value::from_i32(!(integer as i32))
}

/// Applies ToInt32 to both operands and performs one supported bitwise operation.
#[inline(always)]
fn numeric_bitwise_binary(opcode: Opcode, left: Value, right: Value) -> Value {
    let left = numeric_bitwise_int32(left);
    let right = numeric_bitwise_int32(right);
    let result = match opcode {
        Opcode::BitwiseAnd => left & right,
        Opcode::BitwiseOr => left | right,
        Opcode::BitwiseXor => left ^ right,
        _ => unreachable!("bitwise binary dispatch only supplies bitwise opcodes"),
    };
    Value::from_i32(result)
}

#[inline(always)]
fn numeric_bitwise_int32(value: Value) -> i32 {
    value.as_i32().unwrap_or_else(|| {
        let number = value.as_f64().unwrap_or(f64::NAN);
        if !number.is_finite() || number == 0.0 {
            return 0;
        }
        let modulo = number.trunc().rem_euclid(4_294_967_296.0);
        let signed = if modulo >= 2_147_483_648.0 {
            modulo - 4_294_967_296.0
        } else {
            modulo
        };
        signed as i32
    })
}

/// Applies ECMAScript shift-count masking and signed/unsigned left operand conversion.
#[inline(always)]
fn numeric_shift(opcode: Opcode, left: Value, right: Value) -> Value {
    let left_number = left
        .as_i32()
        .map(f64::from)
        .or_else(|| left.as_f64())
        .unwrap_or(f64::NAN);
    let right_number = right
        .as_i32()
        .map(f64::from)
        .or_else(|| right.as_f64())
        .unwrap_or(f64::NAN);
    let shift = numeric_bitwise_uint32(right_number) & 31;
    match opcode {
        Opcode::ShiftLeft => Value::from_i32(numeric_bitwise_int32(left) << shift),
        Opcode::ShiftRight => Value::from_i32(numeric_bitwise_int32(left) >> shift),
        Opcode::ShiftRightUnsigned => {
            Value::from_f64(f64::from(numeric_bitwise_uint32(left_number) >> shift))
        }
        _ => unreachable!("shift dispatch only supplies shift opcodes"),
    }
}

/// Executes `%` and `**` after both operands have crossed the numeric conversion boundary.
#[inline(always)]
fn numeric_remainder_or_power(opcode: Opcode, left: Value, right: Value) -> Value {
    let left = left
        .as_i32()
        .map(f64::from)
        .or_else(|| left.as_f64())
        .unwrap_or(f64::NAN);
    let right = right
        .as_i32()
        .map(f64::from)
        .or_else(|| right.as_f64())
        .unwrap_or(f64::NAN);
    let result = match opcode {
        Opcode::Remainder => left % right,
        Opcode::Exponentiate => left.powf(right),
        _ => unreachable!("arithmetic dispatch only supplies remainder or exponentiation"),
    };
    Value::from_f64(result)
}

/// Compares converted numeric operands while preserving false results for NaN.
#[inline(always)]
pub(crate) fn numeric_relational(opcode: Opcode, left: Value, right: Value) -> Value {
    let left = NumericInput::decode(left)
        .unwrap_or(NumericInput::Float(f64::NAN))
        .into_f64();
    let right = NumericInput::decode(right)
        .unwrap_or(NumericInput::Float(f64::NAN))
        .into_f64();
    numeric_relational_numbers(opcode, left, right)
}

#[inline(always)]
pub(crate) fn numeric_relational_hot(opcode: Opcode, left: Value, right: Value) -> Option<Value> {
    let left = NumericInput::decode(left)?.into_f64();
    let right = NumericInput::decode(right)?.into_f64();
    Some(numeric_relational_numbers(opcode, left, right))
}

#[inline(always)]
fn numeric_relational_numbers(opcode: Opcode, left: f64, right: f64) -> Value {
    let result = match opcode {
        Opcode::LessThan => left < right,
        Opcode::GreaterThan => left > right,
        Opcode::LessEqual => left <= right,
        Opcode::GreaterEqual => left >= right,
        _ => unreachable!("relational dispatch only supplies relational opcodes"),
    };
    Value::from_immediate(if result {
        Immediate::True
    } else {
        Immediate::False
    })
}

#[inline(always)]
fn numeric_bitwise_uint32(number: f64) -> u32 {
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    number.trunc().rem_euclid(4_294_967_296.0) as u32
}

#[inline(always)]
pub(crate) fn numeric_value(value: Value) -> Option<f64> {
    value.as_i32().map(f64::from).or_else(|| value.as_f64())
}

#[inline(always)]
pub(crate) fn safe_integer_value(value: u64) -> Value {
    i32::try_from(value)
        .map(Value::from_i32)
        .unwrap_or_else(|_| Value::from_f64(value as f64))
}

/// Parses ECMAScript numeric string forms after the string has been detached from the heap.
pub(super) fn parse_number_code_units(units: &[u16]) -> f64 {
    let text = String::from_utf16_lossy(units);
    let text = text.trim_matches(is_ecmascript_whitespace);
    if text.is_empty() {
        return 0.0;
    }
    let (radix, digits) = if let Some(digits) = text.strip_prefix("0x") {
        (16, digits)
    } else if let Some(digits) = text.strip_prefix("0X") {
        (16, digits)
    } else if let Some(digits) = text.strip_prefix("0b") {
        (2, digits)
    } else if let Some(digits) = text.strip_prefix("0B") {
        (2, digits)
    } else if let Some(digits) = text.strip_prefix("0o") {
        (8, digits)
    } else if let Some(digits) = text.strip_prefix("0O") {
        (8, digits)
    } else {
        return text.parse::<f64>().unwrap_or(f64::NAN);
    };
    if digits.is_empty() {
        return f64::NAN;
    }
    u64::from_str_radix(digits, radix)
        .map(|value| value as f64)
        .unwrap_or(f64::NAN)
}

#[inline]
fn is_ecmascript_whitespace(character: char) -> bool {
    character.is_whitespace() || character == '\u{feff}'
}
