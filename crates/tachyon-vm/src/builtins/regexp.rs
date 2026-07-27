//! RegExp construction and the first executable ECMAScript object slice.

use super::super::*;
use crate::regexp::backend::{CompiledRegExp, RegExpMatch};
use crate::regexp_exec::{REGEXP_EXEC_GROUPS, REGEXP_EXEC_RESULT, REGEXP_EXEC_TEMPORARY};

pub(crate) struct RegExpBuiltinOutcome {
    pub(crate) value: Value,
    pub(crate) last_index: Option<Value>,
}

impl Isolate {
    /// Implements the branded RegExp `@@replace` fast path for string replacements.
    pub(crate) fn regexp_replace(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let input_argument = self.call_argument(site, 0)?;
        let input = self.regexp_string_argument(input_argument)?;
        let replacement = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        self.regexp_replace_values(site.this_value, input, replacement)
    }

    /// Runs the non-observable branded replacement kernel for a verified RegExp receiver.
    pub(crate) fn regexp_replace_values(
        &mut self,
        receiver: Value,
        input: Value,
        replacement: Value,
    ) -> Result<Value, ExecutionError> {
        let (source_value, flags_value) = self.regexp_data(receiver)?;
        if self.is_callable_value(replacement)? {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(
                replacement,
            ));
        }
        let replacement_units = self.primitive_string_units(replacement)?;
        let input_units = self.regexp_string_units(input)?;
        let source = self.regexp_string_units(source_value)?;
        let flags = self.regexp_flags(flags_value)?;
        let backend_flags = String::from_utf16(&self.regexp_string_units(flags.value)?)
            .map_err(|_| ExecutionError::InvalidRegExpFlags)?;
        let program = CompiledRegExp::compile_units_with_flags(&source, &backend_flags)
            .map_err(|_| ExecutionError::InvalidRegExpPattern)?;
        let unicode = flags.unicode || flags.unicode_sets;
        let mut output = Vec::new();
        output
            .try_reserve_exact(input_units.len().saturating_add(replacement_units.len()))
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        let mut cursor = 0;
        let mut search = 0;
        let mut replaced = false;
        while search <= input_units.len() {
            let Some(matched) = program.find(&input_units, search, unicode) else {
                break;
            };
            output.extend_from_slice(&input_units[cursor..matched.start]);
            append_regexp_replacement(&mut output, &replacement_units, &input_units, &matched);
            cursor = matched.end;
            replaced = true;
            if !flags.global {
                break;
            }
            search = if matched.end == matched.start {
                advance_regexp_split_index(&input_units, matched.end, unicode)
            } else {
                matched.end
            };
        }
        if !replaced {
            return Ok(input);
        }
        output.extend_from_slice(&input_units[cursor..]);
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(output)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Implements the branded `RegExp.prototype[Symbol.match]` operation.
    pub(crate) fn regexp_match(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let receiver = site.this_value;
        let argument = self.call_argument(site, 0)?;
        let input = self.regexp_string_argument(argument)?;
        self.regexp_match_values(receiver, input)
    }

    pub(crate) fn regexp_match_values(
        &mut self,
        receiver: Value,
        input: Value,
    ) -> Result<Value, ExecutionError> {
        let (source, flags_value) = self.regexp_data(receiver)?;
        let source_units = self.regexp_string_units(source)?;
        let flags = self.regexp_flags(flags_value)?;
        let backend_flags = String::from_utf16(&self.regexp_string_units(flags.value)?)
            .map_err(|_| ExecutionError::InvalidRegExpFlags)?;
        let input_units = self.regexp_string_units(input)?;
        let program = CompiledRegExp::compile_units_with_flags(&source_units, &backend_flags)
            .map_err(|_| ExecutionError::InvalidRegExpPattern)?;
        if !flags.global {
            let state = self.allocate_regexp_exec_state(receiver, input, 0)?;
            let outcome = self.regexp_builtin_exec(receiver, input, state, 0)?;
            return Ok(outcome.value);
        }
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before RegExp match");
        let result = self.create_array_object_with_prototype(prototype)?;
        let unicode = flags.unicode || flags.unicode_sets;
        let mut search = 0;
        let mut index = 0_i32;
        while search <= input_units.len() {
            let Some(matched) = program.find(&input_units, search, unicode) else {
                break;
            };
            let value = self.allocate_runtime_string(
                JsString::try_from_utf16(&input_units[matched.start..matched.end])
                    .map_err(ExecutionError::PropertyKeyString)?,
            )?;
            let key = self.property_key_atom(Value::from_i32(index))?;
            self.set_own_data_property(result, key, value)?;
            index = index.saturating_add(1);
            search = if matched.end == matched.start {
                advance_regexp_split_index(&input_units, matched.end, unicode)
            } else {
                matched.end
            };
        }
        let length = self.intern_intrinsic_name(b"length")?;
        self.set_own_data_property(result, length, Value::from_i32(index))?;
        if index == 0 {
            return Ok(Value::from_immediate(Immediate::Null));
        }
        Ok(result)
    }

    /// Creates the RegExpCreate fallback used after String search has converted its pattern.
    #[allow(dead_code, reason = "wired by the pending RegExp search integration")]
    pub(crate) fn create_regexp_for_string_search(
        &mut self,
        pattern: Value,
    ) -> Result<Value, ExecutionError> {
        let (mut source, mut flags) = if self.is_object_value(pattern)
            && let Ok((source, flags)) = self.regexp_data(pattern)
        {
            (source, flags)
        } else {
            let source = if pattern.as_immediate() == Some(Immediate::Undefined) {
                self.allocate_runtime_string(
                    JsString::try_from_latin1(b"(?:)").map_err(ExecutionError::ConstantString)?,
                )?
            } else {
                self.regexp_string_argument(Some(pattern))?
            };
            let (flags, source) = self.allocate_runtime_string_retaining(
                JsString::try_from_latin1(b"").map_err(ExecutionError::ConstantString)?,
                source,
            )?;
            (source, flags)
        };
        if self.regexp_string_units(source)?.is_empty() {
            (source, flags) = self.allocate_runtime_string_retaining(
                JsString::try_from_latin1(b"(?:)").map_err(ExecutionError::ConstantString)?,
                flags,
            )?;
        }
        let source_units = self.regexp_string_units(source)?;
        let backend_flags = String::from_utf16(&self.regexp_string_units(flags)?)
            .map_err(|_| ExecutionError::InvalidRegExpFlags)?;
        CompiledRegExp::compile_units_with_flags(&source_units, &backend_flags)
            .map_err(|_| ExecutionError::InvalidRegExpPattern)?;
        let prototype = self
            .realm
            .regexp_prototype
            .expect("RegExp prototype initializes before String.prototype.search");
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

    /// Implements `RegExp.escape` over code points while preserving exact UTF-16 output.
    pub(crate) fn regexp_escape(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let input = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let units = self
            .regexp_string_units(input)
            .map_err(|error| match error {
                ExecutionError::UnsupportedStringValue(_) => {
                    ExecutionError::UnsupportedPrimitiveStringConversion(input)
                }
                error => error,
            })?;
        let output_length = regexp_escape_output_length(&units)?;
        let mut escaped = Vec::new();
        escaped
            .try_reserve_exact(output_length)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;

        let mut index = 0;
        while index < units.len() {
            let first = units[index];
            if (0xd800..=0xdbff).contains(&first)
                && units
                    .get(index + 1)
                    .is_some_and(|second| (0xdc00..=0xdfff).contains(second))
            {
                escaped.extend_from_slice(&units[index..index + 2]);
                index += 2;
                continue;
            }
            let first_output = escaped.is_empty();
            append_regexp_escape_unit(&mut escaped, first, first_output);
            index += 1;
        }
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(escaped)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Recognizes RegExp's virtual own/prototype accessors for internal `HasProperty`.
    pub(crate) fn is_regexp_value(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.regexp_object)
                .is_ok()
        })
    }

    /// Checks the standard string-named RegExp accessors without materializing descriptors.
    pub(crate) fn regexp_virtual_property(
        &mut self,
        key: PropertyKey,
    ) -> Result<bool, ExecutionError> {
        if let PropertyKey::Symbol(symbol) = key
            && self.realm.well_known_symbols.replace == Some(symbol.value())
        {
            return Ok(true);
        }
        let Some(atom) = key.atom() else {
            return Ok(false);
        };
        for name in [
            b"source".as_slice(),
            b"flags".as_slice(),
            b"hasIndices".as_slice(),
            b"global".as_slice(),
            b"ignoreCase".as_slice(),
            b"multiline".as_slice(),
            b"dotAll".as_slice(),
            b"unicode".as_slice(),
            b"unicodeSets".as_slice(),
            b"sticky".as_slice(),
        ] {
            if self.intern_intrinsic_name(name)? == atom {
                return Ok(true);
            }
        }
        Ok(false)
    }

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
        let (flags, source) = self.allocate_runtime_string_retaining(
            JsString::try_from_latin1(&flag_text).map_err(ExecutionError::ConstantString)?,
            source,
        )?;
        self.validate_regexp_flags(flags)?;
        CompiledRegExp::compile_units_with_flags(
            pattern,
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
        let flags_are_absent = flags_argument.is_none()
            || flags_argument
                .is_some_and(|value| value.as_immediate() == Some(Immediate::Undefined));
        let copied_regexp = pattern_argument.filter(|value| {
            value.as_heap_ref().is_some_and(|raw| {
                self.heap
                    .checked_reference(raw, self.types.regexp_object)
                    .is_ok()
            })
        });
        let (mut pattern, mut flags) = if flags_are_absent && let Some(regexp) = copied_regexp {
            self.regexp_data(regexp)?
        } else {
            let pattern = if pattern_argument.is_none()
                || pattern_argument
                    .is_some_and(|value| value.as_immediate() == Some(Immediate::Undefined))
            {
                self.allocate_runtime_string(
                    JsString::try_from_latin1(b"(?:)").map_err(ExecutionError::ConstantString)?,
                )?
            } else {
                self.regexp_string_argument(pattern_argument)?
            };
            let (flags, pattern) = if flags_are_absent {
                self.allocate_runtime_string_retaining(
                    JsString::try_from_latin1(b"").map_err(ExecutionError::ConstantString)?,
                    pattern,
                )?
            } else {
                self.regexp_string_argument_retaining(flags_argument, pattern)?
            };
            (pattern, flags)
        };
        if self.regexp_string_units(pattern)?.is_empty() {
            (pattern, flags) = self.allocate_runtime_string_retaining(
                JsString::try_from_latin1(b"(?:)").map_err(ExecutionError::ConstantString)?,
                flags,
            )?;
        }
        let source_units = self.regexp_string_units(pattern)?;
        self.validate_regexp_flags(flags)?;
        let backend_flags = String::from_utf16(&self.regexp_string_units(flags)?)
            .map_err(|_| ExecutionError::InvalidRegExpFlags)?;
        CompiledRegExp::compile_units_with_flags(&source_units, &backend_flags)
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

    /// Matches and materializes RegExpBuiltinExec after observable lastIndex conversion.
    pub(crate) fn regexp_builtin_exec(
        &mut self,
        receiver: Value,
        input: Value,
        state: GcRef<NativeCallState>,
        observed_last_index: u64,
    ) -> Result<RegExpBuiltinOutcome, ExecutionError> {
        let (source, flags) = self.regexp_data(receiver)?;
        let source = self.regexp_string_units(source)?;
        let flags = self.regexp_flags(flags)?;
        let backend_flags = String::from_utf16(&self.regexp_string_units(flags.value)?)
            .map_err(|_| ExecutionError::InvalidRegExpFlags)?;
        let input_units = self.regexp_string_units(input)?;
        let start = if flags.global || flags.sticky {
            usize::try_from(observed_last_index).ok()
        } else {
            Some(0)
        };
        let program = CompiledRegExp::compile_units_with_flags(&source, &backend_flags)
            .map_err(|_| ExecutionError::InvalidRegExpPattern)?;
        let matched = start
            .filter(|start| *start <= input_units.len())
            .and_then(|start| {
                program.find(&input_units, start, flags.unicode || flags.unicode_sets)
            })
            .filter(|matched| !flags.sticky || Some(matched.start) == start);
        let Some(matched) = matched else {
            return Ok(RegExpBuiltinOutcome {
                value: Value::from_immediate(Immediate::Null),
                last_index: (flags.global || flags.sticky).then(|| Value::from_i32(0)),
            });
        };
        let end = safe_integer_value(
            u64::try_from(matched.end).map_err(|_| ExecutionError::InvalidStringLength)?,
        );
        let result =
            self.materialize_regexp_match(input, &input_units, matched, flags.indices, state)?;
        Ok(RegExpBuiltinOutcome {
            value: result,
            last_index: (flags.global || flags.sticky).then_some(end),
        })
    }

    /// Creates the exact match Array while publishing every managed intermediate before GC.
    fn materialize_regexp_match(
        &mut self,
        input: Value,
        input_units: &[u16],
        matched: RegExpMatch,
        has_indices: bool,
        state: GcRef<NativeCallState>,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before RegExp result construction");
        let result = self.create_array_object_with_prototype(prototype)?;
        self.update_regexp_exec_state_value(state, REGEXP_EXEC_RESULT, result)?;
        let capture_count = matched.captures.len();
        let mut ranges = Vec::new();
        ranges
            .try_reserve_exact(matched.captures.len() + 1)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        ranges.push(Some(matched.start..matched.end));
        ranges.extend(matched.captures.iter().cloned());
        for (index, range) in ranges.into_iter().enumerate() {
            let key = self.property_key_atom(Value::from_i32(
                i32::try_from(index).map_err(|_| ExecutionError::InvalidStringLength)?,
            ))?;
            let value = match range {
                Some(range) => self.allocate_runtime_string(
                    JsString::try_from_utf16(&input_units[range])
                        .map_err(ExecutionError::PropertyKeyString)?,
                )?,
                None => Value::from_immediate(Immediate::Undefined),
            };
            self.update_regexp_exec_state_value(state, REGEXP_EXEC_TEMPORARY, value)?;
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
        let groups_atom = self.intern_intrinsic_name(b"groups")?;
        if !matched.named_captures.is_empty() {
            let groups =
                self.create_ordinary_object_with_prototype(Value::from_immediate(Immediate::Null))?;
            self.update_regexp_exec_state_value(state, REGEXP_EXEC_GROUPS, groups)?;
            self.set_own_data_property(result, groups_atom, groups)?;
            for (name, range) in &matched.named_captures {
                let atom = self.intern_intrinsic_name(name.as_bytes())?;
                let value = match range {
                    Some(range) => self.allocate_runtime_string(
                        JsString::try_from_utf16(&input_units[range.clone()])
                            .map_err(ExecutionError::PropertyKeyString)?,
                    )?,
                    None => Value::from_immediate(Immediate::Undefined),
                };
                self.update_regexp_exec_state_value(state, REGEXP_EXEC_TEMPORARY, value)?;
                self.set_own_data_property(groups, atom, value)?;
            }
        } else {
            self.set_own_data_property(
                result,
                groups_atom,
                Value::from_immediate(Immediate::Undefined),
            )?;
        }
        if has_indices {
            self.materialize_regexp_indices(result, &matched, groups_atom, state)?;
        }
        Ok(result)
    }

    /// Builds the `d` result graph, publishing each Array before another allocation can occur.
    fn materialize_regexp_indices(
        &mut self,
        result: Value,
        matched: &RegExpMatch,
        groups_atom: AtomId,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before RegExp indices construction");
        let indices_atom = self.intern_intrinsic_name(b"indices")?;
        let indices = self.create_array_object_with_prototype(prototype)?;
        self.update_regexp_exec_state_value(state, REGEXP_EXEC_GROUPS, indices)?;
        self.set_own_data_property(result, indices_atom, indices)?;
        let mut ranges = Vec::new();
        ranges
            .try_reserve_exact(matched.captures.len() + 1)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        ranges.push(Some(matched.start..matched.end));
        ranges.extend(matched.captures.iter().cloned());
        for (index, range) in ranges.iter().enumerate() {
            let key = self.property_key_atom(Value::from_i32(
                i32::try_from(index).map_err(|_| ExecutionError::InvalidStringLength)?,
            ))?;
            let value = self.regexp_indices_pair(range.as_ref(), prototype, state)?;
            self.set_own_data_property(indices, key, value)?;
        }
        let length = self.intern_intrinsic_name(b"length")?;
        self.set_own_data_property(
            indices,
            length,
            Value::from_i32(
                i32::try_from(ranges.len()).map_err(|_| ExecutionError::InvalidStringLength)?,
            ),
        )?;
        if matched.named_captures.is_empty() {
            self.set_own_data_property(
                indices,
                groups_atom,
                Value::from_immediate(Immediate::Undefined),
            )?;
            return Ok(());
        }
        let groups =
            self.create_ordinary_object_with_prototype(Value::from_immediate(Immediate::Null))?;
        self.update_regexp_exec_state_value(state, REGEXP_EXEC_GROUPS, groups)?;
        self.set_own_data_property(indices, groups_atom, groups)?;
        for (name, range) in &matched.named_captures {
            let atom = self.intern_intrinsic_name(name.as_bytes())?;
            let value = self.regexp_indices_pair(range.as_ref(), prototype, state)?;
            self.set_own_data_property(groups, atom, value)?;
        }
        Ok(())
    }

    /// Allocates one `[start, end]` pair, or returns `undefined` for an unmatched capture.
    fn regexp_indices_pair(
        &mut self,
        range: Option<&core::ops::Range<usize>>,
        prototype: Value,
        state: GcRef<NativeCallState>,
    ) -> Result<Value, ExecutionError> {
        let Some(range) = range else {
            return Ok(Value::from_immediate(Immediate::Undefined));
        };
        let pair = self.create_array_object_with_prototype(prototype)?;
        self.update_regexp_exec_state_value(state, REGEXP_EXEC_TEMPORARY, pair)?;
        for (index, offset) in [range.start, range.end].into_iter().enumerate() {
            let key = self.property_key_atom(Value::from_i32(index as i32))?;
            let value = Value::from_i32(
                i32::try_from(offset).map_err(|_| ExecutionError::InvalidStringLength)?,
            );
            self.set_own_data_property(pair, key, value)?;
        }
        let length = self.intern_intrinsic_name(b"length")?;
        self.set_own_data_property(pair, length, Value::from_i32(2))?;
        Ok(pair)
    }

    /// Matches the branded test-only path without allocating a result Array or capture strings.
    pub(crate) fn regexp_builtin_test(
        &mut self,
        receiver: Value,
        input: Value,
        observed_last_index: u64,
    ) -> Result<RegExpBuiltinOutcome, ExecutionError> {
        let (source, flags) = self.regexp_data(receiver)?;
        let source = self.regexp_string_units(source)?;
        let flags = self.regexp_flags(flags)?;
        let backend_flags = String::from_utf16(&self.regexp_string_units(flags.value)?)
            .map_err(|_| ExecutionError::InvalidRegExpFlags)?;
        let input_units = self.regexp_string_units(input)?;
        let start = if flags.global || flags.sticky {
            usize::try_from(observed_last_index).ok()
        } else {
            Some(0)
        };
        let program = CompiledRegExp::compile_units_with_flags(&source, &backend_flags)
            .map_err(|_| ExecutionError::InvalidRegExpPattern)?;
        let matched = start
            .filter(|start| *start <= input_units.len())
            .and_then(|start| {
                program.find(&input_units, start, flags.unicode || flags.unicode_sets)
            })
            .filter(|matched| !flags.sticky || Some(matched.start) == start);
        let Some(matched) = matched else {
            return Ok(RegExpBuiltinOutcome {
                value: Value::from_immediate(Immediate::False),
                last_index: (flags.global || flags.sticky).then(|| Value::from_i32(0)),
            });
        };
        let end = safe_integer_value(
            u64::try_from(matched.end).map_err(|_| ExecutionError::InvalidStringLength)?,
        );
        Ok(RegExpBuiltinOutcome {
            value: Value::from_immediate(Immediate::True),
            last_index: (flags.global || flags.sticky).then_some(end),
        })
    }

    /// Splits one primitive-coercible input through a genuine RegExp backend fast path.
    pub(crate) fn regexp_split(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let (source_value, flags_value) = self.regexp_data(site.this_value)?;
        let input_argument = self.call_argument(site, 0)?;
        let input = self.regexp_string_argument(input_argument)?;
        let input_units = self.regexp_string_units(input)?;
        let source = self.regexp_string_units(source_value)?;
        let flags = self.regexp_flags(flags_value)?;
        let backend_flags = String::from_utf16(&self.regexp_string_units(flags.value)?)
            .map_err(|_| ExecutionError::InvalidRegExpFlags)?;
        let program = CompiledRegExp::compile_units_with_flags(&source, &backend_flags)
            .map_err(|_| ExecutionError::InvalidRegExpPattern)?;
        let limit_argument = self.call_argument(site, 1)?;
        let limit = self.regexp_split_limit(limit_argument)?;
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before RegExp split");
        let result = self.create_array_object_with_prototype(prototype)?;
        self.write(site.caller_base, site.destination, result)?;
        if limit == 0 {
            return Ok(result);
        }
        let full_unicode = flags.unicode || flags.unicode_sets;
        if input_units.is_empty() {
            if program.find(&input_units, 0, full_unicode).is_none() {
                self.string_split_push_value(result, 0, input)?;
            }
            return self.read(site.caller_base, site.destination);
        }
        let mut output_index = 0_u32;
        let mut segment_start = 0;
        let mut search = 0;
        while search < input_units.len() {
            let matched = program
                .find(&input_units, search, full_unicode)
                .filter(|matched| matched.start == search);
            let Some(matched) = matched else {
                search = advance_regexp_split_index(&input_units, search, full_unicode);
                continue;
            };
            if matched.end == segment_start {
                search = advance_regexp_split_index(&input_units, search, full_unicode);
                continue;
            }
            self.string_split_push_units(
                NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                output_index,
                &input_units[segment_start..search],
            )?;
            output_index += 1;
            if output_index == limit {
                return self.read(site.caller_base, site.destination);
            }
            segment_start = matched.end.min(input_units.len());
            for capture in matched.captures {
                let key = self.property_key_atom(safe_integer_value(u64::from(output_index)))?;
                let value = match capture {
                    Some(range) => self.allocate_runtime_string(
                        JsString::try_from_utf16(&input_units[range])
                            .map_err(ExecutionError::PropertyKeyString)?,
                    )?,
                    None => Value::from_immediate(Immediate::Undefined),
                };
                let result = self.read(site.caller_base, site.destination)?;
                self.set_own_data_property(result, key, value)?;
                output_index += 1;
                if output_index == limit {
                    return self.read(site.caller_base, site.destination);
                }
            }
            search = segment_start;
        }
        self.string_split_push_units(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            output_index,
            &input_units[segment_start..],
        )?;
        self.read(site.caller_base, site.destination)
    }

    /// Normalizes the optional RegExp split limit with the ToUint32 modulo rule.
    fn regexp_split_limit(&mut self, value: Option<Value>) -> Result<u32, ExecutionError> {
        let Some(value) = value.filter(|value| value.as_immediate() != Some(Immediate::Undefined))
        else {
            return Ok(u32::MAX);
        };
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        Ok(if !number.is_finite() || number == 0.0 {
            0
        } else {
            number.trunc().rem_euclid(4_294_967_296.0) as u32
        })
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

    /// Implements the branded source and boolean flag accessors on `%RegExp.prototype%`.
    pub(crate) fn regexp_getter(
        &mut self,
        receiver: Value,
        getter: RegExpGetter,
        getter_realm: RealmId,
    ) -> Result<Value, ExecutionError> {
        let intrinsic_prototype = if getter_realm == self.active_realm {
            self.realm.regexp_prototype
        } else {
            self.inactive_realms
                .iter()
                .find_map(|(id, realm)| (*id == getter_realm).then_some(realm.regexp_prototype))
                .flatten()
        };
        if intrinsic_prototype == Some(receiver) {
            return match getter {
                RegExpGetter::Source => self.allocate_runtime_string(
                    JsString::try_from_latin1(b"(?:)").map_err(ExecutionError::ConstantString)?,
                ),
                RegExpGetter::Flags => self.allocate_runtime_string(
                    JsString::try_from_latin1(b"").map_err(ExecutionError::ConstantString)?,
                ),
                _ => Ok(Value::from_immediate(Immediate::Undefined)),
            };
        }
        if getter == RegExpGetter::Flags {
            return self.regexp_flags_getter(receiver);
        }
        if getter == RegExpGetter::Source {
            let (source, _) = self.regexp_data(receiver)?;
            return self.regexp_source_display(source);
        }
        let flag = match getter {
            RegExpGetter::Source => unreachable!("source returns before flag selection"),
            RegExpGetter::Flags => unreachable!("flags returns before flag selection"),
            RegExpGetter::HasIndices => u16::from(b'd'),
            RegExpGetter::Global => u16::from(b'g'),
            RegExpGetter::IgnoreCase => u16::from(b'i'),
            RegExpGetter::Multiline => u16::from(b'm'),
            RegExpGetter::DotAll => u16::from(b's'),
            RegExpGetter::Unicode => u16::from(b'u'),
            RegExpGetter::UnicodeSets => u16::from(b'v'),
            RegExpGetter::Sticky => u16::from(b'y'),
        };
        Ok(Value::from_immediate(
            if self.regexp_flag_enabled(receiver, flag)? {
                Immediate::True
            } else {
                Immediate::False
            },
        ))
    }

    /// Builds flags in specification order, preserving own data-property overrides.
    fn regexp_flags_getter(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        if !self.is_object_value(receiver) {
            return Err(ExecutionError::NotObject(receiver));
        }
        let private_flags = self
            .regexp_data(receiver)
            .ok()
            .and_then(|(_, value)| self.regexp_string_units(value).ok());
        let mut output = Vec::new();
        output
            .try_reserve_exact(8)
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for (name, flag) in [
            (b"hasIndices".as_slice(), b'd'),
            (b"global".as_slice(), b'g'),
            (b"ignoreCase".as_slice(), b'i'),
            (b"multiline".as_slice(), b'm'),
            (b"dotAll".as_slice(), b's'),
            (b"unicode".as_slice(), b'u'),
            (b"unicodeSets".as_slice(), b'v'),
            (b"sticky".as_slice(), b'y'),
        ] {
            let atom = self.intern_intrinsic_name(name)?;
            let property = if private_flags.is_some() {
                self.own_data_property_with_attributes(receiver, atom)?
                    .map(|(value, _)| value)
            } else {
                self.get_data_property(receiver, atom)?
            };
            let enabled = if let Some(value) = property {
                self.is_truthy_value(value)?
            } else {
                private_flags
                    .as_ref()
                    .is_some_and(|flags| flags.contains(&u16::from(flag)))
            };
            if enabled {
                output.push(u16::from(flag));
            }
        }
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(output)
                .map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Escapes pattern delimiters and line terminators for the observable `source` string.
    fn regexp_source_display(&mut self, source: Value) -> Result<Value, ExecutionError> {
        let units = self.regexp_string_units(source)?;
        let extra = regexp_source_escape_extra(&units);
        if extra == 0 {
            return Ok(source);
        }
        let mut output = Vec::new();
        output
            .try_reserve_exact(units.len().saturating_add(extra))
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        let mut preceding_backslashes = 0_usize;
        for unit in units {
            match unit {
                0x0a => output.extend_from_slice(&[u16::from(b'\\'), u16::from(b'n')]),
                0x0d => output.extend_from_slice(&[u16::from(b'\\'), u16::from(b'r')]),
                0x2028 => output.extend_from_slice(&[
                    u16::from(b'\\'),
                    u16::from(b'u'),
                    u16::from(b'2'),
                    u16::from(b'0'),
                    u16::from(b'2'),
                    u16::from(b'8'),
                ]),
                0x2029 => output.extend_from_slice(&[
                    u16::from(b'\\'),
                    u16::from(b'u'),
                    u16::from(b'2'),
                    u16::from(b'0'),
                    u16::from(b'2'),
                    u16::from(b'9'),
                ]),
                unit if unit == u16::from(b'/') && preceding_backslashes.is_multiple_of(2) => {
                    output.extend_from_slice(&[u16::from(b'\\'), unit]);
                }
                unit => output.push(unit),
            }
            preceding_backslashes = if unit == u16::from(b'\\') {
                preceding_backslashes.saturating_add(1)
            } else {
                0
            };
        }
        self.allocate_runtime_string(
            JsString::try_from_owned_code_units(output)
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
    pub(crate) fn regexp_string_argument(
        &mut self,
        value: Option<Value>,
    ) -> Result<Value, ExecutionError> {
        let value = value.unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.is_string_wrapper(value) {
            return self.string_primitive_value(value);
        }
        if self.is_object_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        self.primitive_string_value(Some(value))
    }

    /// Converts one primitive RegExp argument while retaining a prior managed edge.
    fn regexp_string_argument_retaining(
        &mut self,
        value: Option<Value>,
        retained: Value,
    ) -> Result<(Value, Value), ExecutionError> {
        let value = value.unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.is_string_wrapper(value) {
            return self
                .string_primitive_value(value)
                .map(|string| (string, retained));
        }
        if self.is_object_value(value) {
            return Err(ExecutionError::UnsupportedPrimitiveStringConversion(value));
        }
        self.primitive_string_value_retaining(Some(value), retained)
    }

    /// Copies exact UTF-16 code units so backend offsets remain ECMAScript-visible positions.
    pub(crate) fn regexp_string_units(&mut self, value: Value) -> Result<Vec<u16>, ExecutionError> {
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
    pub(crate) fn regexp_flags(&mut self, value: Value) -> Result<RegExpFlags, ExecutionError> {
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

/// Expands the replacement grammar that does not require invoking user code.
fn append_regexp_replacement(
    output: &mut Vec<u16>,
    replacement: &[u16],
    input: &[u16],
    matched: &RegExpMatch,
) {
    let mut index = 0;
    while index < replacement.len() {
        if replacement[index] != u16::from(b'$') || index + 1 >= replacement.len() {
            output.push(replacement[index]);
            index += 1;
            continue;
        }
        match replacement[index + 1] {
            36 => output.push(u16::from(b'$')),
            96 => output.extend_from_slice(&input[..matched.start]),
            39 => output.extend_from_slice(&input[matched.end..]),
            38 => output.extend_from_slice(&input[matched.start..matched.end]),
            digit @ 48..=57 => {
                let first = usize::from(digit - 48);
                let second = replacement
                    .get(index + 2)
                    .copied()
                    .filter(|unit| (48..=57).contains(unit))
                    .map(|unit| usize::from(unit - 48));
                let (capture, consumed) =
                    regexp_replacement_capture(first, second, matched.captures.len());
                let Some(capture) = capture else {
                    output.push(u16::from(b'$'));
                    index += 1;
                    continue;
                };
                if let Some(range) = matched.captures[capture - 1].as_ref() {
                    output.extend_from_slice(&input[range.clone()]);
                }
                index += consumed + 1;
                continue;
            }
            60 if !matched.named_captures.is_empty() => {
                let Some(relative_end) = replacement[index + 2..]
                    .iter()
                    .position(|unit| *unit == u16::from(b'>'))
                else {
                    output.push(u16::from(b'$'));
                    index += 1;
                    continue;
                };
                let name_end = index + 2 + relative_end;
                let name = &replacement[index + 2..name_end];
                if let Some(range) = regexp_named_capture(matched, name) {
                    output.extend_from_slice(&input[range]);
                }
                index = name_end + 1;
                continue;
            }
            _ => {
                output.push(u16::from(b'$'));
                index += 1;
                continue;
            }
        }
        index += 2;
    }
}

/// Chooses the longest valid one- or two-digit replacement capture reference.
#[inline]
fn regexp_replacement_capture(
    first: usize,
    second: Option<usize>,
    capture_count: usize,
) -> (Option<usize>, usize) {
    if let Some(second) = second {
        let two_digit = first * 10 + second;
        if (1..=capture_count).contains(&two_digit) {
            return (Some(two_digit), 2);
        }
    }
    if (1..=capture_count).contains(&first) {
        (Some(first), 1)
    } else {
        (None, 0)
    }
}

/// Resolves a UTF-16 replacement name without allocating a temporary Rust string.
fn regexp_named_capture(
    matched: &RegExpMatch,
    requested: &[u16],
) -> Option<core::ops::Range<usize>> {
    matched
        .named_captures
        .iter()
        .find(|(name, _)| name.encode_utf16().eq(requested.iter().copied()))
        .and_then(|(_, range)| range.clone())
}

/// Computes the exact output capacity using the same code-point boundaries as emission.
fn regexp_escape_output_length(units: &[u16]) -> Result<usize, ExecutionError> {
    let mut length = 0_usize;
    let mut index = 0_usize;
    while index < units.len() {
        let unit = units[index];
        let paired = (0xd800..=0xdbff).contains(&unit)
            && units
                .get(index + 1)
                .is_some_and(|second| (0xdc00..=0xdfff).contains(second));
        let encoded = if paired {
            index += 2;
            2
        } else {
            index += 1;
            regexp_escape_unit_length(unit, length == 0)
        };
        length = length
            .checked_add(encoded)
            .ok_or(ExecutionError::InvalidStringLength)?;
    }
    Ok(length)
}

/// Counts the exact extra UTF-16 units needed by `EscapeRegExpPattern`'s visible cases.
fn regexp_source_escape_extra(units: &[u16]) -> usize {
    let mut extra = 0_usize;
    let mut preceding_backslashes = 0_usize;
    for &unit in units {
        extra = extra.saturating_add(match unit {
            0x0a | 0x0d => 1,
            0x2028 | 0x2029 => 5,
            unit if unit == u16::from(b'/') && preceding_backslashes.is_multiple_of(2) => 1,
            _ => 0,
        });
        preceding_backslashes = if unit == u16::from(b'\\') {
            preceding_backslashes.saturating_add(1)
        } else {
            0
        };
    }
    extra
}

#[inline(always)]
const fn regexp_escape_unit_length(unit: u16, first: bool) -> usize {
    if (first && is_ascii_alphanumeric_unit(unit))
        || ((is_regexp_other_punctuator(unit) || is_regexp_escape_whitespace(unit)) && unit <= 0xff)
    {
        4
    } else if is_surrogate(unit)
        || is_regexp_other_punctuator(unit)
        || is_regexp_escape_whitespace(unit)
    {
        6
    } else if is_regexp_syntax_unit(unit) || regexp_control_escape(unit).is_some() {
        2
    } else {
        1
    }
}

/// Appends the canonical escape for one non-paired UTF-16 code unit.
fn append_regexp_escape_unit(output: &mut Vec<u16>, unit: u16, first: bool) {
    if first && is_ascii_alphanumeric_unit(unit) {
        append_hex_escape(output, b'x', unit, 2);
        return;
    }
    if is_regexp_syntax_unit(unit) {
        output.extend_from_slice(&[u16::from(b'\\'), unit]);
        return;
    }
    if let Some(control) = regexp_control_escape(unit) {
        output.extend_from_slice(&[u16::from(b'\\'), control]);
        return;
    }
    if is_regexp_other_punctuator(unit) || is_regexp_escape_whitespace(unit) || is_surrogate(unit) {
        if unit <= 0xff {
            append_hex_escape(output, b'x', unit, 2);
        } else {
            append_hex_escape(output, b'u', unit, 4);
        }
        return;
    }
    output.push(unit);
}

#[inline(always)]
fn append_hex_escape(output: &mut Vec<u16>, marker: u8, unit: u16, digits: u32) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.extend_from_slice(&[u16::from(b'\\'), u16::from(marker)]);
    for shift in (0..digits).rev() {
        output.push(u16::from(HEX[usize::from((unit >> (shift * 4)) & 0xf)]));
    }
}

#[inline(always)]
const fn is_regexp_syntax_unit(unit: u16) -> bool {
    matches!(
        unit,
        94 | 36 | 92 | 46 | 42 | 43 | 63 | 40 | 41 | 91 | 93 | 123 | 125 | 124 | 47
    )
}

#[inline(always)]
const fn is_regexp_other_punctuator(unit: u16) -> bool {
    matches!(
        unit,
        44 | 45 | 61 | 60 | 62 | 35 | 38 | 33 | 37 | 58 | 59 | 64 | 126 | 39 | 96 | 34
    )
}

#[inline(always)]
const fn regexp_control_escape(unit: u16) -> Option<u16> {
    match unit {
        0x0009 => Some(b't' as u16),
        0x000a => Some(b'n' as u16),
        0x000b => Some(b'v' as u16),
        0x000c => Some(b'f' as u16),
        0x000d => Some(b'r' as u16),
        _ => None,
    }
}

#[inline(always)]
const fn is_regexp_escape_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x0020 | 0x00a0 | 0x1680 | 0x2000
            ..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

#[inline(always)]
const fn is_surrogate(unit: u16) -> bool {
    matches!(unit, 0xd800..=0xdfff)
}

#[inline(always)]
const fn is_ascii_alphanumeric_unit(unit: u16) -> bool {
    matches!(unit, 0x30..=0x39 | 0x41..=0x5a | 0x61..=0x7a)
}

pub(crate) struct RegExpFlags {
    value: Value,
    pub(crate) global: bool,
    sticky: bool,
    indices: bool,
    ignore_case: bool,
    multiline: bool,
    dot_all: bool,
    pub(crate) unicode: bool,
    pub(crate) unicode_sets: bool,
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

/// Advances one UTF-16 position, preserving surrogate pairs only in Unicode matching mode.
#[inline(always)]
pub(crate) fn advance_regexp_split_index(input: &[u16], index: usize, unicode: bool) -> usize {
    if unicode
        && input
            .get(index)
            .is_some_and(|unit| (0xd800..=0xdbff).contains(unit))
        && input
            .get(index + 1)
            .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
    {
        index + 2
    } else {
        index + 1
    }
}
