//! Provider-backed Intl locale-list canonicalization.

use super::super::*;

const LOCALE_RECEIVER: usize = 0;
const LOCALE_RESULT: usize = 1;
const LOCALE_LENGTH: usize = 2;
const LOCALE_NEXT_INDEX: usize = 3;
const LOCALE_RESULT_COUNT: usize = 4;

struct IntlLocaleListRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for IntlLocaleListRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Starts `Intl.supportedValuesOf`, preserving observable ToString for object keys.
    pub(crate) fn begin_intl_supported_values_of(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let key = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.is_object_value(key) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlSupportedValuesKey,
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::Undefined),
                key,
                site.call_site,
            );
        }
        let key = self.primitive_to_string_value(key)?;
        self.finish_intl_supported_values_of(continuation_site, key)
    }

    /// Materializes one provider collection into a freshly allocated intrinsic Array.
    pub(crate) fn finish_intl_supported_values_of(
        &mut self,
        site: NativeContinuationSite,
        key: Value,
    ) -> Result<(), ExecutionError> {
        let key = self.intl_supported_values_key(key)?;
        let mut values = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .supported_values(key)
            .map_err(ExecutionError::IntlProvider)?
            .into_vec();
        values.sort_unstable();
        values.dedup();
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl"),
        )?;
        self.write(site.caller_base, site.destination, result)?;
        for (index, value) in values.into_iter().enumerate() {
            let result = self.read(site.caller_base, site.destination)?;
            let (value, result) = self.allocate_runtime_string_retaining(
                JsString::try_from_str(&value).map_err(ExecutionError::PropertyKeyString)?,
                result,
            )?;
            self.write(site.caller_base, site.destination, result)?;
            let index = u32::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
            let property = self.property_key_atom(safe_integer_value(u64::from(index)))?;
            let result = self.read(site.caller_base, site.destination)?;
            self.set_own_data_property(result, property, value)?;
        }
        Ok(())
    }

    /// Recognizes the six case-sensitive ECMA-402 enumeration keys without host parsing.
    fn intl_supported_values_key(
        &mut self,
        key: Value,
    ) -> Result<IntlSupportedValuesKey, ExecutionError> {
        let units = self.string_value_to_utf16(key)?;
        for (name, key) in [
            (b"calendar".as_slice(), IntlSupportedValuesKey::Calendar),
            (b"collation".as_slice(), IntlSupportedValuesKey::Collation),
            (b"currency".as_slice(), IntlSupportedValuesKey::Currency),
            (
                b"numberingSystem".as_slice(),
                IntlSupportedValuesKey::NumberingSystem,
            ),
            (b"timeZone".as_slice(), IntlSupportedValuesKey::TimeZone),
            (b"unit".as_slice(), IntlSupportedValuesKey::Unit),
        ] {
            if intl_utf16_equals_ascii(&units, name) {
                return Ok(key);
            }
        }
        Err(ExecutionError::InvalidIntlSupportedValuesKey)
    }

    /// Constructs the initial `Intl.Locale` internal-slot surface used by locale-list consumers.
    pub(crate) fn create_intl_locale_from_site(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(site.new_target) {
            return Err(ExecutionError::NonConstructor(site.callee));
        }
        let tag = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let tag = if let Ok(locale) = self.intl_locale_reference(tag) {
            self.intl_locale_tag(locale)?
        } else if self.is_string_value(tag) {
            tag
        } else if self.is_object_value(tag) {
            return Err(ExecutionError::InvalidLocaleListElement(tag));
        } else {
            self.primitive_to_string_value(tag)?
        };
        let mut canonical = self.canonicalize_intl_locale_text(tag)?;
        let options = self
            .call_argument(site, 1)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if options.as_immediate() != Some(Immediate::Undefined) {
            let options = self.coerce_to_object(options)?;
            let mut extensions = Vec::new();
            extensions
                .try_reserve_exact(3)
                .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
            for (property, key) in [
                (b"calendar".as_slice(), "ca"),
                (b"collation".as_slice(), "co"),
                (b"numberingSystem".as_slice(), "nu"),
            ] {
                if let Some(value) = self.intl_locale_option(options, property)? {
                    extensions.push((key, value));
                }
            }
            if !extensions.is_empty() {
                let extension_len = extensions
                    .iter()
                    .map(|(key, value)| 3 + key.len() + value.len())
                    .sum::<usize>();
                let mut combined = String::new();
                combined
                    .try_reserve_exact(canonical.len() + 2 + extension_len)
                    .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                combined.push_str(&canonical);
                combined.push_str("-u");
                for (key, value) in extensions {
                    combined.push('-');
                    combined.push_str(key);
                    combined.push('-');
                    combined.push_str(&value);
                }
                canonical = self.canonicalize_intl_ascii_tag(&combined)?;
            }
        }
        let value = self.allocate_runtime_string(
            JsString::try_from_str(&canonical).map_err(ExecutionError::PropertyKeyString)?,
        )?;
        let prototype = self
            .realm
            .intl_locale_prototype
            .expect("Intl.Locale prototype initializes before construction");
        let locale = self.allocate_intl_locale_object(value, prototype, AllocationSpace::Young)?;
        self.write(site.caller_base, site.destination, locale)
    }

    /// Returns the canonical tag retained by the current minimal `[[InitializedLocale]]` carrier.
    pub(crate) fn intl_locale_to_string(
        &mut self,
        receiver: Value,
    ) -> Result<Value, ExecutionError> {
        let locale = self.intl_locale_reference(receiver)?;
        self.intl_locale_tag(locale)
    }

    fn intl_locale_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<IntlLocaleObject>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::InvalidLocaleListElement(value))?;
        self.heap
            .checked_reference(raw, self.types.intl_locale_object)
            .map_err(|_| ExecutionError::InvalidLocaleListElement(value))
    }

    fn intl_locale_tag(
        &mut self,
        locale: GcRef<IntlLocaleObject>,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let locale = scope.root(locale).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(locale, self.types.intl_locale_object)
                    .map(|locale| locale.locale)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Returns one canonical Unicode extension value from the temporary Locale tag carrier.
    pub(crate) fn intl_locale_extension_value(
        &mut self,
        receiver: Value,
        native: NativeFunction,
    ) -> Result<Value, ExecutionError> {
        let tag = self.intl_locale_to_string(receiver)?;
        let tag = self.string_value_to_ascii(tag)?;
        let key = match native {
            NativeFunction::IntlLocaleCalendar => "ca",
            NativeFunction::IntlLocaleCollation => "co",
            NativeFunction::IntlLocaleHourCycle => "hc",
            NativeFunction::IntlLocaleCaseFirst => "kf",
            NativeFunction::IntlLocaleNumberingSystem => "nu",
            _ => unreachable!("only Intl.Locale extension getters enter this path"),
        };
        let Some(value) = intl_unicode_extension_value(&tag, key)? else {
            return Ok(Value::from_immediate(Immediate::Undefined));
        };
        self.allocate_runtime_string(
            JsString::try_from_str(&value).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Returns one base-name component from the canonical internal locale tag.
    pub(crate) fn intl_locale_base_component(
        &mut self,
        receiver: Value,
        native: NativeFunction,
    ) -> Result<Value, ExecutionError> {
        let tag = self.intl_locale_to_string(receiver)?;
        let tag = self.string_value_to_ascii(tag)?;
        let components = LocaleBaseComponents::parse(&tag)
            .ok_or(ExecutionError::InvalidLocaleListElement(receiver))?;
        let component = match native {
            NativeFunction::IntlLocaleBaseName => Some(components.base),
            NativeFunction::IntlLocaleLanguage => Some(components.language),
            NativeFunction::IntlLocaleScript => components.script,
            NativeFunction::IntlLocaleRegion => components.region,
            NativeFunction::IntlLocaleVariants => components.variants,
            _ => unreachable!("only Intl.Locale base getters enter this path"),
        };
        let Some(component) = component else {
            return Ok(Value::from_immediate(Immediate::Undefined));
        };
        self.allocate_runtime_string(
            JsString::try_from_str(component).map_err(ExecutionError::PropertyKeyString)?,
        )
    }

    /// Interprets the Unicode `kn` keyword as the Locale numeric boolean slot.
    pub(crate) fn intl_locale_numeric(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        let tag = self.intl_locale_to_string(receiver)?;
        let tag = self.string_value_to_ascii(tag)?;
        let numeric = match intl_unicode_extension_value(&tag, "kn")? {
            None => false,
            Some(value) => value != "false",
        };
        Ok(boolean_value(numeric))
    }

    /// Applies provider likely-subtag data and returns a fresh branded Locale object.
    pub(crate) fn call_intl_locale_transform(
        &mut self,
        site: &CallSite,
        maximize: bool,
    ) -> Result<(), ExecutionError> {
        let tag = self.intl_locale_to_string(site.this_value)?;
        let tag = self.string_value_to_ascii(tag)?;
        let transformed = {
            let provider = self
                .host_providers
                .intl_mut()
                .ok_or(ExecutionError::MissingIntlProvider)?;
            if maximize {
                provider.maximize_locale(&tag)
            } else {
                provider.minimize_locale(&tag)
            }
            .map_err(ExecutionError::IntlProvider)?
        };
        let tag = self.allocate_runtime_string(
            JsString::try_from_str(&transformed).map_err(ExecutionError::PropertyKeyString)?,
        )?;
        let prototype = self
            .realm
            .intl_locale_prototype
            .expect("Intl.Locale prototype initializes before likely-subtag methods");
        let locale = self.allocate_intl_locale_object(tag, prototype, AllocationSpace::Young)?;
        self.write(site.caller_base, site.destination, locale)
    }

    /// Reads one initial Locale option and converts the current primitive-only carrier input.
    fn intl_locale_option(
        &mut self,
        options: Value,
        property: &[u8],
    ) -> Result<Option<String>, ExecutionError> {
        let property = self.intern_intrinsic_name(property)?;
        let value = self
            .get_data_property(options, property)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if value.as_immediate() == Some(Immediate::Undefined) {
            return Ok(None);
        }
        let value = if self.is_string_value(value) {
            value
        } else if self.is_object_value(value) {
            return Err(ExecutionError::InvalidLocaleListElement(value));
        } else {
            self.primitive_to_string_value(value)?
        };
        self.string_value_to_ascii(value).map(Some)
    }

    /// Starts `Intl.getCanonicalLocales`, preserving every observable array-like access.
    pub(crate) fn begin_intl_get_canonical_locales(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let locales = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl"),
        )?;
        if locales.as_immediate() == Some(Immediate::Undefined) {
            return self.write(site.caller_base, site.destination, result);
        }
        let site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.is_string_value(locales) {
            let state = self.allocate_intl_locale_list_state(locales, result)?;
            self.write(
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
            )?;
            return self.resume_intl_locale_list_element(site, state, locales);
        }
        let receiver = self.coerce_to_object(locales)?;
        let state = self.allocate_intl_locale_list_state(receiver, result)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let length = self.length_atom()?;
        let observed = self.dispatch_intl_locale_get(
            site,
            state,
            IntlLocaleListStage::Length,
            receiver,
            length.into(),
        )?;
        if let Some(observed) = observed {
            self.resume_intl_locale_list(site, state, IntlLocaleListStage::Length, observed)?;
        }
        Ok(())
    }

    /// Resumes one observable locale-list length, membership, or element read.
    pub(crate) fn resume_intl_locale_list(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: IntlLocaleListStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match stage {
            IntlLocaleListStage::Length => {
                if self.is_object_value(value) {
                    return self.dispatch_object_primitive_conversion(
                        ConversionConsumer::IntlLocaleListLength,
                        site.caller_base,
                        site.destination,
                        Value::from_heap_ref(state.raw()),
                        value,
                        site.call_site,
                    );
                }
                self.resume_intl_locale_list_length(site, state, value)
            }
            IntlLocaleListStage::Has => {
                if self.is_truthy_value(value)? {
                    let snapshot = self.native_call_state_snapshot(state)?;
                    let index =
                        intl_exact_nonnegative_integer(snapshot.values[LOCALE_NEXT_INDEX])? - 1;
                    let key = self.property_key_atom(safe_integer_value(index))?;
                    let observed = self.dispatch_intl_locale_get(
                        site,
                        state,
                        IntlLocaleListStage::Get,
                        snapshot.values[LOCALE_RECEIVER],
                        key.into(),
                    )?;
                    if let Some(observed) = observed {
                        self.resume_intl_locale_list(
                            site,
                            state,
                            IntlLocaleListStage::Get,
                            observed,
                        )?;
                    }
                    Ok(())
                } else {
                    self.advance_intl_locale_list(site, state)
                }
            }
            IntlLocaleListStage::Get => self.consume_intl_locale_list_element(site, state, value),
        }
    }

    /// Applies ToLength after the observable `length` read and starts indexed traversal.
    pub(crate) fn resume_intl_locale_list_length(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = self.convert_to_number(value)?;
        let number =
            numeric_value(number).ok_or(ExecutionError::UnsupportedNumberConversion(number))?;
        let length = if number.is_nan() || number <= 0.0 {
            0
        } else if !number.is_finite() || number >= MAX_SAFE_INTEGER as f64 {
            MAX_SAFE_INTEGER
        } else {
            number.floor() as u64
        };
        self.update_native_call_state_value(state, LOCALE_LENGTH, safe_integer_value(length))?;
        self.advance_intl_locale_list(site, state)
    }

    /// Converts one permitted String/Object element and resumes canonicalization.
    fn consume_intl_locale_list_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_string_value(value) {
            return self.resume_intl_locale_list_element(site, state, value);
        }
        if !self.is_object_value(value) {
            return Err(ExecutionError::InvalidLocaleListElement(value));
        }
        self.dispatch_object_primitive_conversion(
            ConversionConsumer::IntlLocaleListElement,
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
            value,
            site.call_site,
        )
    }

    /// Calls the provider, removes duplicates by canonical String value, and advances the list.
    pub(crate) fn resume_intl_locale_list_element(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.canonicalize_and_append_locale(site, state, value)?;
        self.advance_intl_locale_list(site, state)
    }

    /// Canonicalizes one already-string element and appends it only when not already present.
    fn canonicalize_and_append_locale(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let canonical = self.canonicalize_intl_locale_text(value)?;
        let canonical = self.allocate_runtime_string(
            JsString::try_from_str(&canonical).map_err(ExecutionError::PropertyKeyString)?,
        )?;
        let snapshot = self.native_call_state_snapshot(state)?;
        let count = intl_exact_nonnegative_integer(snapshot.values[LOCALE_RESULT_COUNT])?;
        for index in 0..count {
            let key = self.property_key_atom(safe_integer_value(index))?;
            let existing = self
                .get_data_property(snapshot.values[LOCALE_RESULT], key)?
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            if self.strict_equal_values(existing, canonical)? {
                return Ok(());
            }
        }
        let key = self.property_key_atom(safe_integer_value(count))?;
        self.set_own_data_property(snapshot.values[LOCALE_RESULT], key, canonical)?;
        self.update_native_call_state_value(
            state,
            LOCALE_RESULT_COUNT,
            safe_integer_value(count + 1),
        )
    }

    /// Converts a managed ECMAScript String into an ASCII BCP 47 input and calls the provider.
    pub(crate) fn canonicalize_intl_locale_text(
        &mut self,
        value: Value,
    ) -> Result<Box<str>, ExecutionError> {
        let tag = self.string_value_to_ascii(value)?;
        self.canonicalize_intl_ascii_tag(&tag)
    }

    fn string_value_to_ascii(&mut self, value: Value) -> Result<String, ExecutionError> {
        let units = self.string_value_to_utf16(value)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(units.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for unit in units {
            let byte = u8::try_from(unit).map_err(|_| ExecutionError::InvalidLanguageTag)?;
            if !byte.is_ascii() {
                return Err(ExecutionError::InvalidLanguageTag);
            }
            bytes.push(byte);
        }
        String::from_utf8(bytes).map_err(|_| ExecutionError::InvalidLanguageTag)
    }

    pub(crate) fn canonicalize_intl_ascii_tag(
        &mut self,
        tag: &str,
    ) -> Result<Box<str>, ExecutionError> {
        self.host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .canonicalize_locale(tag)
            .map_err(ExecutionError::IntlProvider)?
            .ok_or(ExecutionError::InvalidLanguageTag)
    }

    /// Iterates synchronously until a Proxy/accessor suspends or every index has been visited.
    fn advance_intl_locale_list(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        loop {
            self.write(
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
            )?;
            let snapshot = self.native_call_state_snapshot(state)?;
            let length = intl_exact_nonnegative_integer(snapshot.values[LOCALE_LENGTH])?;
            let index = intl_exact_nonnegative_integer(snapshot.values[LOCALE_NEXT_INDEX])?;
            if index >= length {
                return self.write(
                    site.caller_base,
                    site.destination,
                    snapshot.values[LOCALE_RESULT],
                );
            }
            self.update_native_call_state_value(
                state,
                LOCALE_NEXT_INDEX,
                safe_integer_value(index + 1),
            )?;
            let key = safe_integer_value(index);
            let observed =
                self.dispatch_intl_locale_has(site, state, snapshot.values[LOCALE_RECEIVER], key)?;
            let Some(observed) = observed else {
                return Ok(());
            };
            if !self.is_truthy_value(observed)? {
                continue;
            }
            let key = self.property_key_atom(key)?;
            let observed = self.dispatch_intl_locale_get(
                site,
                state,
                IntlLocaleListStage::Get,
                snapshot.values[LOCALE_RECEIVER],
                key.into(),
            )?;
            let Some(observed) = observed else {
                return Ok(());
            };
            if self.is_string_value(observed) {
                self.canonicalize_and_append_locale(site, state, observed)?;
                continue;
            }
            if self.is_object_value(observed) {
                self.dispatch_object_primitive_conversion(
                    ConversionConsumer::IntlLocaleListElement,
                    site.caller_base,
                    site.destination,
                    Value::from_heap_ref(state.raw()),
                    observed,
                    site.call_site,
                )?;
                return Ok(());
            }
            return Err(ExecutionError::InvalidLocaleListElement(observed));
        }
    }

    /// Dispatches one Proxy-aware Get while retaining the locale-list state as its parent.
    fn dispatch_intl_locale_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: IntlLocaleListStage,
        receiver: Value,
        key: PropertyKey,
    ) -> Result<Option<Value>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_intl_locale_parent(site, state, stage, receiver)?;
        let outcome = self.dispatch_proxy_aware_property_read(site, receiver, receiver, key);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() == completion_depth
        {
            return Ok(None);
        }
        self.pop_native_continuation()?;
        self.read(site.caller_base, site.destination).map(Some)
    }

    /// Dispatches one Proxy-aware HasProperty while retaining the locale-list state as parent.
    fn dispatch_intl_locale_has(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        receiver: Value,
        key: Value,
    ) -> Result<Option<Value>, ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        let frame_depth = self.fiber.frames.len();
        self.push_intl_locale_parent(site, state, IntlLocaleListStage::Has, key)?;
        let outcome = self.dispatch_has_property(site, receiver, key);
        if let Err(error) = outcome {
            if self.fiber.completions.len() > completion_depth {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth
            || self.fiber.completions.len() == completion_depth
        {
            return Ok(None);
        }
        self.pop_native_continuation()?;
        self.read(site.caller_base, site.destination).map(Some)
    }

    fn push_intl_locale_parent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        stage: IntlLocaleListStage,
        retained: Value,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(NativeContinuation::intl_locale_list(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                retained,
            ))
            .map_err(Isolate::completion_stack_error)
    }

    /// Allocates fixed traced state without retaining a Rust collection across callbacks.
    fn allocate_intl_locale_list_state(
        &mut self,
        receiver: Value,
        result: Value,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let pending = NativeCallState {
            values: [
                receiver,
                result,
                Value::from_i32(0),
                Value::from_i32(0),
                Value::from_i32(0),
            ],
            count: 5,
        };
        let mut roots = IntlLocaleListRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                inactive_realms: &mut self.inactive_realms,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            pending,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }
}

/// Extracts a multi-subtag type from one canonical `-u-` extension without allocating a token Vec.
fn intl_unicode_extension_value(
    tag: &str,
    expected: &str,
) -> Result<Option<String>, ExecutionError> {
    let mut tokens = tag.split('-');
    while tokens.next().is_some_and(|token| token != "u") {}
    let mut current_key = None;
    let mut output = String::new();
    for token in tokens {
        if token.len() == 1 {
            break;
        }
        if token.len() == 2 {
            if current_key == Some(expected) {
                return Ok(Some(output));
            }
            current_key = Some(token);
            output.clear();
            continue;
        }
        if current_key == Some(expected) {
            let separator = !output.is_empty();
            output
                .try_reserve_exact(token.len() + usize::from(separator))
                .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
            if separator {
                output.push('-');
            }
            output.push_str(token);
        }
    }
    Ok((current_key == Some(expected)).then_some(output))
}

struct LocaleBaseComponents<'a> {
    base: &'a str,
    language: &'a str,
    script: Option<&'a str>,
    region: Option<&'a str>,
    variants: Option<&'a str>,
}

impl<'a> LocaleBaseComponents<'a> {
    /// Splits a canonical Unicode locale ID without allocating component strings.
    fn parse(tag: &'a str) -> Option<Self> {
        let mut consumed = 0;
        let mut base_end = tag.len();
        for segment in tag.split('-') {
            if consumed != 0 && segment.len() == 1 {
                base_end = consumed - 1;
                break;
            }
            consumed = consumed.checked_add(segment.len() + 1)?;
        }
        let base = tag.get(..base_end)?;
        let mut segments = base.split('-');
        let language = segments.next()?;
        let mut offset = language.len();
        let mut script = None;
        let mut region = None;
        let mut variants_start = None;
        for segment in segments {
            offset = offset.checked_add(1)?;
            let start = offset;
            offset = offset.checked_add(segment.len())?;
            if script.is_none()
                && segment.len() == 4
                && segment.bytes().all(|byte| byte.is_ascii_alphabetic())
            {
                script = Some(segment);
            } else if region.is_none()
                && ((segment.len() == 2 && segment.bytes().all(|byte| byte.is_ascii_alphabetic()))
                    || (segment.len() == 3 && segment.bytes().all(|byte| byte.is_ascii_digit())))
            {
                region = Some(segment);
            } else if variants_start.is_none() {
                variants_start = Some(start);
            }
        }
        Some(Self {
            base,
            language,
            script,
            region,
            variants: variants_start.and_then(|start| base.get(start..)),
        })
    }
}

#[inline(always)]
fn intl_utf16_equals_ascii(units: &[u16], ascii: &[u8]) -> bool {
    units.len() == ascii.len()
        && units
            .iter()
            .zip(ascii)
            .all(|(unit, byte)| *unit == u16::from(*byte))
}

#[inline(always)]
fn intl_exact_nonnegative_integer(value: Value) -> Result<u64, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
    if number < 0.0 || number.fract() != 0.0 || number > MAX_SAFE_INTEGER as f64 {
        return Err(ExecutionError::UnsupportedNumberConversion(value));
    }
    Ok(number as u64)
}
