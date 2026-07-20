//! RegExp construction and the first executable ECMAScript object slice.

use super::super::*;
use crate::regexp::backend::CompiledRegExp;

impl Isolate {
    /// Creates one independent RegExp literal object from verified, module-owned UTF-16 data.
    pub(crate) fn create_regexp_literal(
        &mut self,
        pattern: &[u16],
        flags: u8,
    ) -> Result<Value, ExecutionError> {
        let source = self.allocate_runtime_string(
            JsString::try_from_utf16(pattern).map_err(ExecutionError::ConstantString)?,
        )?;
        let flag_text = regexp_flag_text(flags)?;
        let flags = self.allocate_runtime_string(
            JsString::try_from_latin1(&flag_text).map_err(ExecutionError::ConstantString)?,
        )?;
        self.validate_regexp_flags(flags)?;
        let source_text =
            String::from_utf16(pattern).map_err(|_| ExecutionError::InvalidRegExpPattern)?;
        CompiledRegExp::compile_with_flags(
            &source_text,
            core::str::from_utf8(&flag_text).expect("RegExp flags are ASCII"),
        )
        .map_err(|_| ExecutionError::InvalidRegExpPattern)?;
        let prototype = self
            .realm
            .regexp_prototype
            .expect("RegExp prototype initializes before literal evaluation");
        let regexp = self.allocate_regexp_object(source, flags, prototype)?;
        let last_index = self.intern_intrinsic_name(b"lastIndex")?;
        self.define_fresh_data_property(
            regexp,
            last_index,
            Value::from_i32(0),
            PropertyAttributes::data(true, false, false),
        )?;
        Ok(regexp)
    }

    /// Creates a RegExp exotic after validating the supported flag set and source program.
    pub(crate) fn create_regexp_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<Value, ExecutionError> {
        let pattern_argument = self.call_argument(site, 0)?;
        let flags_argument = self.call_argument(site, 1)?;
        let mut pattern = if pattern_argument.is_none()
            || pattern_argument
                .is_some_and(|value| value.as_immediate() == Some(Immediate::Undefined))
        {
            self.allocate_runtime_string(
                JsString::try_from_latin1(b"(?:)").map_err(ExecutionError::ConstantString)?,
            )?
        } else {
            self.regexp_string_argument(pattern_argument)?
        };
        if self.regexp_string_units(pattern)?.is_empty() {
            pattern = self.allocate_runtime_string(
                JsString::try_from_latin1(b"(?:)").map_err(ExecutionError::ConstantString)?,
            )?;
        }
        let flags = if flags_argument.is_none()
            || flags_argument
                .is_some_and(|value| value.as_immediate() == Some(Immediate::Undefined))
        {
            self.allocate_runtime_string(
                JsString::try_from_latin1(b"").map_err(ExecutionError::ConstantString)?,
            )?
        } else {
            self.regexp_string_argument(flags_argument)?
        };
        let source_units = self.regexp_string_units(pattern)?;
        let source =
            String::from_utf16(&source_units).map_err(|_| ExecutionError::InvalidRegExpPattern)?;
        self.validate_regexp_flags(flags)?;
        let backend_flags = String::from_utf16(&self.regexp_string_units(flags)?)
            .map_err(|_| ExecutionError::InvalidRegExpFlags)?;
        CompiledRegExp::compile_with_flags(&source, &backend_flags)
            .map_err(|_| ExecutionError::InvalidRegExpPattern)?;
        let prototype = if self.is_object_value(site.new_target) {
            let prototype_atom = self.prototype_atom()?;
            self.get_data_property(site.new_target, prototype_atom)?
                .filter(|value| self.is_object_value(*value))
                .unwrap_or_else(|| {
                    self.realm
                        .regexp_prototype
                        .expect("RegExp prototype initializes before construction")
                })
        } else {
            self.realm
                .regexp_prototype
                .expect("RegExp prototype initializes before construction")
        };
        let regexp = self.allocate_regexp_object(pattern, flags, prototype)?;
        let last_index = self.intern_intrinsic_name(b"lastIndex")?;
        self.define_fresh_data_property(
            regexp,
            last_index,
            Value::from_i32(0),
            PropertyAttributes::data(true, false, false),
        )?;
        Ok(regexp)
    }

    /// Executes the backend and materializes the match plus every positional capture.
    pub(crate) fn regexp_exec(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let (source, flags) = self.regexp_data(site.this_value)?;
        let input_argument = self.call_argument(site, 0)?;
        let input = self.regexp_string_argument(input_argument)?;
        let source = String::from_utf16(&self.regexp_string_units(source)?)
            .map_err(|_| ExecutionError::InvalidRegExpPattern)?;
        let flags = self.regexp_flags(flags)?;
        let backend_flags = String::from_utf16(&self.regexp_string_units(flags.value)?)
            .map_err(|_| ExecutionError::InvalidRegExpFlags)?;
        let input_units = self.regexp_string_units(input)?;
        let last_index_atom = self.intern_intrinsic_name(b"lastIndex")?;
        let start = if flags.global || flags.sticky {
            self.get_data_property(site.this_value, last_index_atom)?
                .and_then(numeric_value)
                .filter(|value| value.is_finite() && *value >= 0.0)
                .map_or(0, |value| value.trunc() as usize)
        } else {
            0
        };
        let program = CompiledRegExp::compile_with_flags(&source, &backend_flags)
            .map_err(|_| ExecutionError::InvalidRegExpPattern)?;
        let matched = program
            .find_ucs2(&input_units, start)
            .filter(|matched| !flags.sticky || matched.start == start);
        let Some(matched) = matched else {
            if flags.global || flags.sticky {
                self.set_own_data_property(site.this_value, last_index_atom, Value::from_i32(0))?;
            }
            return Ok(Value::from_immediate(Immediate::Null));
        };
        if flags.global || flags.sticky {
            let end =
                i32::try_from(matched.end).map_err(|_| ExecutionError::InvalidStringLength)?;
            self.set_own_data_property(site.this_value, last_index_atom, Value::from_i32(end))?;
        }
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before RegExp result construction");
        let result = self.create_array_object_with_prototype(prototype)?;
        let capture_count = matched.captures.len();
        let mut ranges = Vec::new();
        ranges
            .try_reserve_exact(matched.captures.len() + 1)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        ranges.push(Some(matched.start..matched.end));
        ranges.extend(matched.captures.iter().cloned());
        for (index, range) in ranges.into_iter().enumerate() {
            let value = match range {
                Some(range) => self.allocate_runtime_string(
                    JsString::try_from_utf16(&input_units[range])
                        .map_err(ExecutionError::PropertyKeyString)?,
                )?,
                None => Value::from_immediate(Immediate::Undefined),
            };
            let key = self.property_key_atom(Value::from_i32(
                i32::try_from(index).map_err(|_| ExecutionError::InvalidStringLength)?,
            ))?;
            self.set_own_data_property(result, key, value)?;
        }
        let length = self.intern_intrinsic_name(b"length")?;
        self.set_own_data_property(
            result,
            length,
            Value::from_i32(
                i32::try_from(capture_count + 1)
                    .map_err(|_| ExecutionError::InvalidStringLength)?,
            ),
        )?;
        let index = self.intern_intrinsic_name(b"index")?;
        self.set_own_data_property(
            result,
            index,
            Value::from_i32(
                i32::try_from(matched.start).map_err(|_| ExecutionError::InvalidStringLength)?,
            ),
        )?;
        let input_atom = self.intern_intrinsic_name(b"input")?;
        self.set_own_data_property(result, input_atom, input)?;
        if !matched.named_captures.is_empty() {
            let groups = self.create_ordinary_object()?;
            for (name, range) in matched.named_captures {
                let atom = self.intern_intrinsic_name(name.as_bytes())?;
                let value = match range {
                    Some(range) => self.allocate_runtime_string(
                        JsString::try_from_utf16(&input_units[range])
                            .map_err(ExecutionError::PropertyKeyString)?,
                    )?,
                    None => Value::from_immediate(Immediate::Undefined),
                };
                self.set_own_data_property(groups, atom, value)?;
            }
            let groups_atom = self.intern_intrinsic_name(b"groups")?;
            self.set_own_data_property(result, groups_atom, groups)?;
        }
        Ok(result)
    }

    /// Implements `RegExp.prototype.test` via the same builtin execution and `lastIndex` path.
    pub(crate) fn regexp_test(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        Ok(Value::from_immediate(
            if self.regexp_exec(site)?.as_immediate() == Some(Immediate::Null) {
                Immediate::False
            } else {
                Immediate::True
            },
        ))
    }

    /// Builds the canonical slash-delimited source and flag representation.
    pub(crate) fn regexp_to_string(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        let (source, flags) = self.regexp_data(receiver)?;
        let source = self.regexp_string_units(source)?;
        let flags = self.regexp_string_units(flags)?;
        let mut units = Vec::new();
        units
            .try_reserve_exact(source.len() + flags.len() + 2)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        units.push(u16::from(b'/'));
        units.extend(source);
        units.push(u16::from(b'/'));
        units.extend(flags);
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(units)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Reads the private source and flags slots after validating the receiver's exotic identity.
    pub(crate) fn regexp_data(
        &mut self,
        receiver: Value,
    ) -> Result<(Value, Value), ExecutionError> {
        let raw = receiver
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(receiver))?;
        let regexp = self
            .heap
            .checked_reference(raw, self.types.regexp_object)
            .map_err(|_| ExecutionError::NotObject(receiver))?;
        self.heap.with_running_scope(|scope| {
            let regexp = scope.root(regexp).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(regexp, self.types.regexp_object)
                    .map(|regexp| (regexp.source, regexp.flags))
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Reads one standard RegExp flag getter from the validated private flag string.
    pub(crate) fn regexp_flag_enabled(
        &mut self,
        receiver: Value,
        flag: u16,
    ) -> Result<bool, ExecutionError> {
        let (_, flags) = self.regexp_data(receiver)?;
        Ok(self.regexp_string_units(flags)?.contains(&flag))
    }

    /// Converts the currently supported primitive inputs while rejecting observable object conversion.
    fn regexp_string_argument(&mut self, value: Option<Value>) -> Result<Value, ExecutionError> {
        let value = value.unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.is_string_wrapper(value) {
            return self.string_primitive_value(value);
        }
        if self.is_object_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        self.primitive_string_value(Some(value))
    }

    /// Copies exact UTF-16 code units so backend offsets remain ECMAScript-visible positions.
    fn regexp_string_units(&mut self, value: Value) -> Result<Vec<u16>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedStringValue(value))?;
        let string = self
            .heap
            .checked_reference(raw, self.types.string)
            .map_err(|_| ExecutionError::UnsupportedStringValue(value))?;
        self.heap.with_running_scope(|scope| {
            let string = scope.root(string).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let string = no_gc
                    .borrow(string, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let mut units = Vec::new();
                units
                    .try_reserve_exact(string.len())
                    .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                for index in 0..string.len() {
                    units.push(string.code_unit_at(index).expect("bounded code-unit index"));
                }
                Ok(units)
            })
        })
    }

    /// Parses duplicate-sensitive flag characters and retains only execution-state flags for now.
    fn regexp_flags(&mut self, value: Value) -> Result<RegExpFlags, ExecutionError> {
        let mut flags = RegExpFlags {
            value,
            global: false,
            sticky: false,
            indices: false,
            ignore_case: false,
            multiline: false,
            dot_all: false,
            unicode: false,
            unicode_sets: false,
        };
        let units = self.regexp_string_units(value)?;
        for unit in units.iter().copied() {
            let slot = match unit {
                103 => &mut flags.global,
                121 => &mut flags.sticky,
                100 => &mut flags.indices,
                105 => &mut flags.ignore_case,
                109 => &mut flags.multiline,
                115 => &mut flags.dot_all,
                117 => &mut flags.unicode,
                118 => &mut flags.unicode_sets,
                _ => return Err(ExecutionError::InvalidRegExpFlags),
            };
            if *slot {
                return Err(ExecutionError::InvalidRegExpFlags);
            }
            *slot = true;
        }
        if flags.unicode && flags.unicode_sets {
            return Err(ExecutionError::InvalidRegExpFlags);
        }
        Ok(flags)
    }

    /// Validates flags once at construction even when the backend is not asked to execute yet.
    fn validate_regexp_flags(&mut self, value: Value) -> Result<(), ExecutionError> {
        self.regexp_flags(value).map(|_| ())
    }
}

struct RegExpFlags {
    value: Value,
    global: bool,
    sticky: bool,
    indices: bool,
    ignore_case: bool,
    multiline: bool,
    dot_all: bool,
    unicode: bool,
    unicode_sets: bool,
}

/// Reconstructs canonical flag order from Oxc's stable bit encoding.
fn regexp_flag_text(flags: u8) -> Result<Vec<u8>, ExecutionError> {
    const G: u8 = 1 << 0;
    const I: u8 = 1 << 1;
    const M: u8 = 1 << 2;
    const S: u8 = 1 << 3;
    const U: u8 = 1 << 4;
    const Y: u8 = 1 << 5;
    const D: u8 = 1 << 6;
    const V: u8 = 1 << 7;
    if flags & U != 0 && flags & V != 0 {
        return Err(ExecutionError::InvalidRegExpFlags);
    }
    let mut result = Vec::new();
    result
        .try_reserve_exact(8)
        .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
    for (mask, flag) in [
        (D, b'd'),
        (G, b'g'),
        (I, b'i'),
        (M, b'm'),
        (S, b's'),
        (U, b'u'),
        (V, b'v'),
        (Y, b'y'),
    ] {
        if flags & mask != 0 {
            result.push(flag);
        }
    }
    Ok(result)
}
