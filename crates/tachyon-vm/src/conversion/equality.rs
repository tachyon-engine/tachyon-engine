//! Primitive Abstract Equality redo rules and resumable object conversion entry.

use super::{super::*, numeric_value};

impl Isolate {
    /// Implements the supported primitive subset of Abstract Equality Comparison.
    pub(crate) fn loose_equal_values(
        &mut self,
        mut left: Value,
        mut right: Value,
    ) -> Result<bool, ExecutionError> {
        loop {
            if self.strict_equal_values(left, right)? {
                return Ok(true);
            }
            let left_immediate = left.as_immediate();
            let right_immediate = right.as_immediate();
            let left_nullish =
                matches!(left_immediate, Some(Immediate::Undefined | Immediate::Null));
            let right_nullish = matches!(
                right_immediate,
                Some(Immediate::Undefined | Immediate::Null)
            );
            if left_nullish || right_nullish {
                return Ok(left_nullish && right_nullish);
            }
            if let Some(immediate @ (Immediate::True | Immediate::False)) = left_immediate {
                left = Value::from_i32(i32::from(immediate == Immediate::True));
                continue;
            }
            if let Some(immediate @ (Immediate::True | Immediate::False)) = right_immediate {
                right = Value::from_i32(i32::from(immediate == Immediate::True));
                continue;
            }
            let left_number = numeric_value(left).is_some();
            let right_number = numeric_value(right).is_some();
            let left_string = self.is_string_value(left);
            let right_string = self.is_string_value(right);
            if left_number && right_string {
                right = self.convert_to_number(right)?;
                continue;
            }
            if left_string && right_number {
                left = self.convert_to_number(left)?;
                continue;
            }
            return Ok(false);
        }
    }

    /// Converts the sole object operand only when Abstract Equality requires ToPrimitive.
    pub(crate) fn dispatch_object_loose_equality(
        &mut self,
        opcode: Opcode,
        caller_base: u32,
        destination: u32,
        left: Value,
        right: Value,
        call_site: WordOffset,
    ) -> Result<(), ExecutionError> {
        debug_assert!(matches!(opcode, Opcode::LooseEqual | Opcode::LooseNotEqual));
        let left_object = self.is_object_value(left);
        let right_object = self.is_object_value(right);
        debug_assert!(left_object || right_object);
        if self.strict_equal_values(left, right)? || (left_object && right_object) {
            let equal = left == right;
            return self.write_loose_equality_result(caller_base, destination, opcode, equal);
        }
        let (object, primitive) = if left_object {
            (left, right)
        } else {
            (right, left)
        };
        if is_nullish(primitive) {
            return self.write_loose_equality_result(caller_base, destination, opcode, false);
        }
        let primitive_is_eligible = numeric_value(primitive).is_some()
            || self.is_string_value(primitive)
            || self.is_symbol_value(primitive)
            || matches!(
                primitive.as_immediate(),
                Some(Immediate::True | Immediate::False)
            );
        if !primitive_is_eligible {
            return self.write_loose_equality_result(caller_base, destination, opcode, false);
        }
        self.dispatch_object_primitive_conversion(
            ConversionConsumer::Equality(opcode),
            caller_base,
            destination,
            primitive,
            object,
            call_site,
        )
    }

    /// Publishes the boolean result shared by synchronous and resumable loose equality paths.
    fn write_loose_equality_result(
        &mut self,
        caller_base: u32,
        destination: u32,
        opcode: Opcode,
        equal: bool,
    ) -> Result<(), ExecutionError> {
        self.write(
            caller_base,
            destination,
            Value::from_immediate(if equal == (opcode == Opcode::LooseEqual) {
                Immediate::True
            } else {
                Immediate::False
            }),
        )
    }
}
