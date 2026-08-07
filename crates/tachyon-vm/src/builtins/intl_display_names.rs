//! Provider-backed `Intl.DisplayNames` construction and code lookup.

use super::super::*;
use crate::runtime::fiber::IntlDisplayNamesStage;

mod state;

pub(crate) use state::PendingIntlDisplayNames;

const UNDEFINED: Value = Value::from_immediate(Immediate::Undefined);

impl Isolate {
    /// Starts construction after enforcing the new-only DisplayNames contract.
    pub(crate) fn begin_intl_display_names_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(site.new_target) {
            return Err(ExecutionError::NonConstructor(site.callee));
        }
        self.begin_intl_display_names_options(site, site.new_target, false)
    }

    /// Starts locale filtering while observing only `localeMatcher`.
    pub(crate) fn begin_intl_display_names_supported_locales_of(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_intl_display_names_options(site, UNDEFINED, true)
    }

    /// Allocates traced state and nests CanonicalizeLocaleList below its continuation.
    fn begin_intl_display_names_options(
        &mut self,
        site: &CallSite,
        new_target: Value,
        supported_locales: bool,
    ) -> Result<(), ExecutionError> {
        let locales = self.call_argument(site, 0)?.unwrap_or(UNDEFINED);
        let options = self.call_argument(site, 1)?.unwrap_or(UNDEFINED);
        let state = self.allocate_pending_intl_display_names(PendingIntlDisplayNames::new(
            new_target,
            options,
            supported_locales,
        ))?;
        self.set_pending_intl_display_names_value(
            state,
            |pending| &mut pending.requested_locales,
            locales,
        )?;
        if !supported_locales {
            return self.dispatch_intl_display_names_prototype_get(site, state, locales);
        }
        self.dispatch_intl_display_names_locales(site, state, locales)
    }

    /// Observes `newTarget.prototype` before locales and options, as required by construction.
    fn dispatch_intl_display_names_prototype_get(
        &mut self,
        site: &CallSite,
        state: GcRef<PendingIntlDisplayNames>,
        locales: Value,
    ) -> Result<(), ExecutionError> {
        self.update_pending_intl_display_names(state, |pending| {
            pending.stage = IntlDisplayNamesStage::Prototype;
        })?;
        let snapshot = self.pending_intl_display_names_snapshot(state)?;
        let key = self.prototype_atom()?.into();
        let continuation = NativeContinuation::intl_display_names(
            Self::native_site(site),
            IntlDisplayNamesStage::Prototype,
            Value::from_heap_ref(state.raw()),
            locales,
        );
        match self.resolve_property_read_until_proxy(snapshot.new_target, key)? {
            PropertyReadResolution::Read(PropertyRead::Missing) => self.resume_intl_display_names(
                continuation,
                IntlDisplayNamesStage::Prototype,
                UNDEFINED,
            ),
            PropertyReadResolution::Read(PropertyRead::Data(value)) => self
                .resume_intl_display_names(continuation, IntlDisplayNamesStage::Prototype, value),
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) if getter == UNDEFINED => {
                self.resume_intl_display_names(
                    continuation,
                    IntlDisplayNamesStage::Prototype,
                    UNDEFINED,
                )
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => self
                .dispatch_property_callback(
                    NativeContinuation::intl_display_names_property_get(
                        Self::native_site(site),
                        Value::from_heap_ref(state.raw()),
                        snapshot.new_target,
                    ),
                    getter,
                )
                .map(|_| ()),
            PropertyReadResolution::Proxy(_) => {
                self.dispatch_intl_display_names_nested(continuation, |isolate| {
                    isolate
                        .dispatch_proxy_aware_property_read(
                            Self::native_site(site),
                            snapshot.new_target,
                            snapshot.new_target,
                            key,
                        )
                        .map(|_| ())
                })
            }
        }
    }

    /// Starts the shared canonical locale-list state machine beneath DisplayNames state.
    fn dispatch_intl_display_names_locales(
        &mut self,
        site: &CallSite,
        state: GcRef<PendingIntlDisplayNames>,
        locales: Value,
    ) -> Result<(), ExecutionError> {
        self.update_pending_intl_display_names(state, |pending| {
            pending.stage = IntlDisplayNamesStage::Locales;
        })?;
        self.dispatch_intl_display_names_nested(
            NativeContinuation::intl_display_names(
                Self::native_site(site),
                IntlDisplayNamesStage::Locales,
                Value::from_heap_ref(state.raw()),
                locales,
            ),
            |isolate| isolate.begin_intl_get_canonical_locales(site),
        )
    }

    /// Resumes locale canonicalization or one observable option conversion.
    pub(crate) fn resume_intl_display_names(
        &mut self,
        continuation: NativeContinuation,
        stage: IntlDisplayNamesStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.pending_intl_display_names_reference(continuation.first())?;
        if stage == IntlDisplayNamesStage::Prototype {
            self.set_pending_intl_display_names_value(
                state,
                |pending| &mut pending.prototype,
                value,
            )?;
            let site = continuation.site();
            let locales = self
                .pending_intl_display_names_snapshot(state)?
                .requested_locales;
            self.update_pending_intl_display_names(state, |pending| {
                pending.stage = IntlDisplayNamesStage::Locales;
            })?;
            return self.dispatch_intl_display_names_nested(
                NativeContinuation::intl_display_names(
                    site,
                    IntlDisplayNamesStage::Locales,
                    Value::from_heap_ref(state.raw()),
                    locales,
                ),
                |isolate| isolate.begin_intl_get_canonical_locales_value(site, locales),
            );
        }
        if stage == IntlDisplayNamesStage::Locales {
            return self.resume_intl_display_names_locales(continuation.site(), state, value);
        }
        if stage == IntlDisplayNamesStage::Of {
            return self.resume_intl_display_names_code(continuation.site(), state, value);
        }
        if value == UNDEFINED {
            return self.advance_intl_display_names_option(continuation.site(), state, stage);
        }
        self.update_pending_intl_display_names(state, |pending| pending.stage = stage)?;
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlDisplayNamesStringOption,
                continuation.site().caller_base,
                continuation.site().destination,
                Value::from_heap_ref(state.raw()),
                value,
                continuation.site().call_site,
            );
        }
        self.resume_intl_display_names_option_primitive(continuation.site(), state, value)
    }

    /// Stores canonical locales and enters the exact DisplayNames option order.
    fn resume_intl_display_names_locales(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlDisplayNames>,
        locales: Value,
    ) -> Result<(), ExecutionError> {
        self.set_pending_intl_display_names_value(state, |pending| &mut pending.locales, locales)?;
        let snapshot = self.pending_intl_display_names_snapshot(state)?;
        let options = if snapshot.supported_locales && snapshot.options == UNDEFINED {
            UNDEFINED
        } else if self.is_object_value(snapshot.options) {
            snapshot.options
        } else {
            return Err(ExecutionError::NotObject(snapshot.options));
        };
        if options == UNDEFINED {
            return self.finish_intl_display_names_supported_locales(site, state);
        }
        self.set_pending_intl_display_names_value(state, |pending| &mut pending.options, options)?;
        self.dispatch_intl_display_names_option_get(
            site,
            state,
            IntlDisplayNamesStage::LocaleMatcher,
        )
    }

    /// Converts and parses one string-valued option before advancing observable Gets.
    pub(crate) fn resume_intl_display_names_option_primitive(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlDisplayNames>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let string = self.primitive_to_string_value(primitive)?;
        let text = self
            .string_value_to_ascii(string)
            .map_err(|_| ExecutionError::InvalidIntlDisplayNamesOption)?;
        let stage = self.pending_intl_display_names_stage(state)?;
        self.update_pending_intl_display_names(state, |pending| match stage {
            IntlDisplayNamesStage::LocaleMatcher => {
                pending.locale_matcher = match text.as_str() {
                    "lookup" => IntlLocaleMatcher::Lookup,
                    "best fit" => IntlLocaleMatcher::BestFit,
                    _ => pending.locale_matcher,
                };
            }
            IntlDisplayNamesStage::Style => {
                pending.style = match text.as_str() {
                    "long" => IntlDisplayNamesStyle::Long,
                    "short" => IntlDisplayNamesStyle::Short,
                    "narrow" => IntlDisplayNamesStyle::Narrow,
                    _ => pending.style,
                };
            }
            IntlDisplayNamesStage::Type => {
                pending.display_type = match text.as_str() {
                    "language" => Some(IntlDisplayNamesType::Language),
                    "region" => Some(IntlDisplayNamesType::Region),
                    "script" => Some(IntlDisplayNamesType::Script),
                    "currency" => Some(IntlDisplayNamesType::Currency),
                    "calendar" => Some(IntlDisplayNamesType::Calendar),
                    "dateTimeField" => Some(IntlDisplayNamesType::DateTimeField),
                    _ => pending.display_type,
                };
            }
            IntlDisplayNamesStage::Fallback => {
                pending.fallback = match text.as_str() {
                    "code" => IntlDisplayNamesFallback::Code,
                    "none" => IntlDisplayNamesFallback::None,
                    _ => pending.fallback,
                };
            }
            IntlDisplayNamesStage::LanguageDisplay => {
                pending.language_display = match text.as_str() {
                    "dialect" => IntlDisplayNamesLanguageDisplay::Dialect,
                    "standard" => IntlDisplayNamesLanguageDisplay::Standard,
                    _ => pending.language_display,
                };
            }
            _ => {}
        })?;
        if !display_names_option_is_valid(stage, &text) {
            return Err(ExecutionError::InvalidIntlDisplayNamesOption);
        }
        self.advance_intl_display_names_option(site, state, stage)
    }

    /// Advances the ordered option pipeline and closes locale filtering early.
    fn advance_intl_display_names_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlDisplayNames>,
        stage: IntlDisplayNamesStage,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_display_names_snapshot(state)?;
        let next = match stage {
            IntlDisplayNamesStage::LocaleMatcher if snapshot.supported_locales => None,
            IntlDisplayNamesStage::LocaleMatcher => Some(IntlDisplayNamesStage::Style),
            IntlDisplayNamesStage::Style => Some(IntlDisplayNamesStage::Type),
            IntlDisplayNamesStage::Type if snapshot.display_type.is_none() => {
                return Err(ExecutionError::MissingIntlDisplayNamesType);
            }
            IntlDisplayNamesStage::Type => Some(IntlDisplayNamesStage::Fallback),
            IntlDisplayNamesStage::Fallback => Some(IntlDisplayNamesStage::LanguageDisplay),
            IntlDisplayNamesStage::LanguageDisplay => None,
            _ => return Err(ExecutionError::MissingNativeContinuation),
        };
        let Some(next) = next else {
            return if snapshot.supported_locales {
                self.finish_intl_display_names_supported_locales(site, state)
            } else {
                self.finish_intl_display_names_construction(site, state)
            };
        };
        self.dispatch_intl_display_names_option_get(site, state, next)
    }

    /// Performs one Proxy/accessor-aware option Get under a typed continuation.
    fn dispatch_intl_display_names_option_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlDisplayNames>,
        stage: IntlDisplayNamesStage,
    ) -> Result<(), ExecutionError> {
        self.update_pending_intl_display_names(state, |pending| pending.stage = stage)?;
        let snapshot = self.pending_intl_display_names_snapshot(state)?;
        let key = self
            .intern_intrinsic_name(display_names_option_name(stage)?)?
            .into();
        let continuation = NativeContinuation::intl_display_names(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            snapshot.options,
        );
        match self.resolve_property_read_until_proxy(snapshot.options, key)? {
            PropertyReadResolution::Read(PropertyRead::Missing) => {
                self.resume_intl_display_names(continuation, stage, UNDEFINED)
            }
            PropertyReadResolution::Read(PropertyRead::Data(value)) => {
                self.resume_intl_display_names(continuation, stage, value)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) if getter == UNDEFINED => {
                self.resume_intl_display_names(continuation, stage, UNDEFINED)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => self
                .dispatch_property_callback(
                    NativeContinuation::intl_display_names_property_get(
                        site,
                        Value::from_heap_ref(state.raw()),
                        snapshot.options,
                    ),
                    getter,
                )
                .map(|_| ()),
            PropertyReadResolution::Proxy(_) => {
                self.dispatch_intl_display_names_nested(continuation, |isolate| {
                    isolate
                        .dispatch_proxy_aware_property_read(
                            site,
                            snapshot.options,
                            snapshot.options,
                            key,
                        )
                        .map(|_| ())
                })
            }
        }
    }

    /// Creates the provider backend and branded object with newTarget-Realm fallback semantics.
    fn finish_intl_display_names_construction(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlDisplayNames>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_display_names_snapshot(state)?;
        let request = IntlDisplayNamesRequest {
            locales: self.intl_display_names_locale_strings(snapshot.locales)?,
            locale_matcher: snapshot.locale_matcher,
            style: snapshot.style,
            display_type: snapshot
                .display_type
                .ok_or(ExecutionError::MissingIntlDisplayNamesType)?,
            fallback: snapshot.fallback,
            language_display: snapshot.language_display,
        };
        let creation = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .create_display_names(request)
            .map_err(ExecutionError::IntlProvider)?;
        let default_prototype = self
            .realm
            .intl_display_names_prototype
            .expect("Intl.DisplayNames prototype initializes before construction");
        let prototype = self
            .is_object_value(snapshot.prototype)
            .then_some(snapshot.prototype)
            .or_else(|| {
                self.realm_for_callable(snapshot.new_target)
                    .ok()
                    .and_then(|realm| {
                        self.realm_intrinsic_prototype(
                            realm,
                            IntrinsicPrototypeKind::IntlDisplayNames,
                        )
                    })
            })
            .unwrap_or(default_prototype);
        let object =
            self.allocate_intl_display_names_object(creation, prototype, AllocationSpace::Young)?;
        self.write(site.caller_base, site.destination, object)
    }

    /// Filters canonical requested locales and publishes a fresh intrinsic Array.
    fn finish_intl_display_names_supported_locales(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlDisplayNames>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_display_names_snapshot(state)?;
        let locales = self.intl_display_names_locale_strings(snapshot.locales)?;
        let supported = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .display_names_supported_locales(&locales, snapshot.locale_matcher)
            .map_err(ExecutionError::IntlProvider)?;
        self.materialize_intl_display_names_locales(site, supported)
    }

    /// Brands the receiver before starting the observable ToString(code) conversion.
    pub(crate) fn begin_intl_display_names_of(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.intl_display_names_reference(site.this_value)?;
        let code = self.call_argument(site, 0)?.unwrap_or(UNDEFINED);
        let mut pending = PendingIntlDisplayNames::new(UNDEFINED, UNDEFINED, false);
        pending.receiver = site.this_value;
        pending.code = code;
        pending.stage = IntlDisplayNamesStage::Of;
        let state = self.allocate_pending_intl_display_names(pending)?;
        if self.is_object_value(code) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlDisplayNamesCode,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                code,
                site.call_site,
            );
        }
        self.resume_intl_display_names_code(Self::native_site(site), state, code)
    }

    /// Canonicalizes one code according to the object's type and applies provider fallback.
    pub(crate) fn resume_intl_display_names_code(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlDisplayNames>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let string = self.primitive_to_string_value(primitive)?;
        self.set_pending_intl_display_names_value(state, |pending| &mut pending.code, string)?;
        let snapshot = self.pending_intl_display_names_snapshot(state)?;
        let object = self.intl_display_names_reference(snapshot.receiver)?;
        let payload = self.intl_display_names_snapshot(object)?.payload;
        let display_type = self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(payload, self.types.intl_display_names_payload)
                    .map(|payload| payload.resolved.display_type)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let text = self
            .string_value_to_ascii(string)
            .map_err(|_| ExecutionError::InvalidIntlDisplayNamesOption)?;
        let canonical = self.canonicalize_intl_display_names_code(display_type, &text)?;
        let (localized, fallback) = self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let payload = no_gc
                    .borrow(payload, self.types.intl_display_names_payload)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let localized = payload
                    .backend
                    .display_name(&canonical)
                    .map_err(ExecutionError::IntlProvider)?;
                Ok::<_, ExecutionError>((localized, payload.resolved.fallback))
            })
        })?;
        let Some(units) = localized.or_else(|| {
            (fallback == IntlDisplayNamesFallback::Code).then(|| {
                canonical
                    .encode_utf16()
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            })
        }) else {
            return self.write(site.caller_base, site.destination, UNDEFINED);
        };
        let result = self.allocate_runtime_string(
            JsString::try_from_utf16(&units).map_err(ExecutionError::PropertyKeyString)?,
        )?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Returns locale/style/type/fallback/languageDisplay in specification order.
    pub(crate) fn call_intl_display_names_resolved_options(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let object = self.intl_display_names_reference(site.this_value)?;
        let payload = self.intl_display_names_snapshot(object)?.payload;
        let resolved = self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(payload, self.types.intl_display_names_payload)
                    .map(|payload| payload.resolved.clone())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let result = self.create_ordinary_object()?;
        self.write(site.caller_base, site.destination, result)?;
        for (name, value) in [
            (b"locale".as_slice(), resolved.locale.as_bytes()),
            (
                b"style".as_slice(),
                display_names_style_name(resolved.style),
            ),
            (
                b"type".as_slice(),
                display_names_type_name(resolved.display_type),
            ),
            (
                b"fallback".as_slice(),
                display_names_fallback_name(resolved.fallback),
            ),
        ] {
            self.set_intl_display_names_resolved_string(result, name, value)?;
        }
        if resolved.display_type == IntlDisplayNamesType::Language {
            self.set_intl_display_names_resolved_string(
                result,
                b"languageDisplay",
                display_names_language_display_name(resolved.language_display),
            )?;
        }
        Ok(())
    }

    /// Validates and canonicalizes the namespace-specific DisplayNames code grammar.
    fn canonicalize_intl_display_names_code(
        &mut self,
        display_type: IntlDisplayNamesType,
        code: &str,
    ) -> Result<Box<str>, ExecutionError> {
        match display_type {
            IntlDisplayNamesType::Language => canonicalize_unicode_language_id(code)
                .ok_or(ExecutionError::InvalidIntlDisplayNamesOption),
            IntlDisplayNamesType::Region if valid_ascii_alpha(code, 2) => {
                Ok(code.to_ascii_uppercase().into_boxed_str())
            }
            IntlDisplayNamesType::Region
                if code.len() == 3 && code.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                Ok(code.into())
            }
            IntlDisplayNamesType::Script if valid_ascii_alpha(code, 4) => {
                let mut canonical = code.to_ascii_lowercase().into_bytes();
                canonical[0].make_ascii_uppercase();
                String::from_utf8(canonical)
                    .map(String::into_boxed_str)
                    .map_err(|_| ExecutionError::InvalidIntlDisplayNamesOption)
            }
            IntlDisplayNamesType::Currency if valid_ascii_alpha(code, 3) => {
                Ok(code.to_ascii_uppercase().into_boxed_str())
            }
            IntlDisplayNamesType::Calendar if is_unicode_locale_type(code) => {
                Ok(code.to_ascii_lowercase().into_boxed_str())
            }
            IntlDisplayNamesType::DateTimeField if is_date_time_field(code) => Ok(code.into()),
            _ => Err(ExecutionError::InvalidIntlDisplayNamesOption),
        }
    }

    fn intl_display_names_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<IntlDisplayNamesObject>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::IncompatibleIntlDisplayNamesReceiver(value))?;
        self.heap
            .checked_reference(raw, self.types.intl_display_names_object)
            .map_err(|_| ExecutionError::IncompatibleIntlDisplayNamesReceiver(value))
    }

    fn intl_display_names_snapshot(
        &mut self,
        object: GcRef<IntlDisplayNamesObject>,
    ) -> Result<IntlDisplayNamesObject, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(object, self.types.intl_display_names_object)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn intl_display_names_locale_strings(
        &mut self,
        locales: Value,
    ) -> Result<Box<[Box<str>]>, ExecutionError> {
        let values = self.copy_packed_intl_array(locales)?;
        let mut strings = Vec::new();
        strings
            .try_reserve_exact(values.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for value in values {
            strings.push(self.string_value_to_ascii(value)?.into_boxed_str());
        }
        Ok(strings.into_boxed_slice())
    }

    /// Publishes provider-filtered locales without observing Array prototype methods.
    fn materialize_intl_display_names_locales(
        &mut self,
        site: NativeContinuationSite,
        locales: Box<[Box<str>]>,
    ) -> Result<(), ExecutionError> {
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.DisplayNames"),
        )?;
        self.write(site.caller_base, site.destination, result)?;
        for (index, locale) in locales.into_vec().into_iter().enumerate() {
            let result = self.read(site.caller_base, site.destination)?;
            let (locale, result) = self.allocate_runtime_string_retaining(
                JsString::try_from_str(&locale).map_err(ExecutionError::PropertyKeyString)?,
                result,
            )?;
            self.write(site.caller_base, site.destination, result)?;
            let key = self.property_key_atom(safe_integer_value(
                u64::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?,
            ))?;
            self.set_own_data_property(result, key, locale)?;
        }
        Ok(())
    }

    fn set_intl_display_names_resolved_string(
        &mut self,
        result: Value,
        name: &[u8],
        value: &[u8],
    ) -> Result<(), ExecutionError> {
        let (value, result) = self.allocate_runtime_string_retaining(
            JsString::try_from_latin1(value).map_err(ExecutionError::PropertyKeyString)?,
            result,
        )?;
        let key = self.intern_intrinsic_name(name)?;
        self.set_own_data_property(result, key, value)
    }

    /// Drains synchronous nested work while preserving the typed parent continuation.
    fn dispatch_intl_display_names_nested(
        &mut self,
        continuation: NativeContinuation,
        operation: impl FnOnce(&mut Self) -> Result<(), ExecutionError>,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = operation(self) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(
            continuation.site().caller_base,
            continuation.site().destination,
        )?;
        let NativeContinuationKind::IntlDisplayNames(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_intl_display_names(continuation, stage, value)
    }
}

fn display_names_option_name(
    stage: IntlDisplayNamesStage,
) -> Result<&'static [u8], ExecutionError> {
    match stage {
        IntlDisplayNamesStage::LocaleMatcher => Ok(b"localeMatcher"),
        IntlDisplayNamesStage::Style => Ok(b"style"),
        IntlDisplayNamesStage::Type => Ok(b"type"),
        IntlDisplayNamesStage::Fallback => Ok(b"fallback"),
        IntlDisplayNamesStage::LanguageDisplay => Ok(b"languageDisplay"),
        _ => Err(ExecutionError::MissingNativeContinuation),
    }
}

#[inline(always)]
fn display_names_option_is_valid(stage: IntlDisplayNamesStage, value: &str) -> bool {
    match stage {
        IntlDisplayNamesStage::LocaleMatcher => matches!(value, "lookup" | "best fit"),
        IntlDisplayNamesStage::Style => matches!(value, "long" | "short" | "narrow"),
        IntlDisplayNamesStage::Type => matches!(
            value,
            "language" | "region" | "script" | "currency" | "calendar" | "dateTimeField"
        ),
        IntlDisplayNamesStage::Fallback => matches!(value, "code" | "none"),
        IntlDisplayNamesStage::LanguageDisplay => matches!(value, "dialect" | "standard"),
        _ => false,
    }
}

#[inline(always)]
fn valid_ascii_alpha(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_alphabetic())
}

#[inline(always)]
fn is_unicode_locale_type(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|subtag| {
            (3..=8).contains(&subtag.len())
                && subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

#[inline(always)]
fn is_date_time_field(value: &str) -> bool {
    matches!(
        value,
        "era"
            | "year"
            | "quarter"
            | "month"
            | "weekOfYear"
            | "weekday"
            | "day"
            | "dayPeriod"
            | "hour"
            | "minute"
            | "second"
            | "timeZoneName"
    )
}

/// Parses the extension-free Unicode language-id grammar and applies canonical casing.
fn canonicalize_unicode_language_id(value: &str) -> Option<Box<str>> {
    if value.eq_ignore_ascii_case("root") || value.contains('_') {
        return None;
    }
    let subtags = value.split('-').collect::<Vec<_>>();
    let language = *subtags.first()?;
    if !matches!(language.len(), 2 | 3 | 5..=8)
        || !language.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        return None;
    }
    let mut index = 1;
    let script = subtags.get(index).copied().filter(|subtag| {
        subtag.len() == 4 && subtag.bytes().all(|byte| byte.is_ascii_alphabetic())
    });
    index += usize::from(script.is_some());
    let region = subtags.get(index).copied().filter(|subtag| {
        valid_ascii_alpha(subtag, 2)
            || (subtag.len() == 3 && subtag.bytes().all(|byte| byte.is_ascii_digit()))
    });
    index += usize::from(region.is_some());
    let variants = subtags.get(index..)?;
    let mut seen = std::collections::HashSet::with_capacity(variants.len());
    for variant in variants {
        let valid = ((5..=8).contains(&variant.len())
            && variant.bytes().all(|byte| byte.is_ascii_alphanumeric()))
            || (variant.len() == 4
                && variant.as_bytes()[0].is_ascii_digit()
                && variant.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        if !valid || !seen.insert(variant.to_ascii_lowercase()) {
            return None;
        }
    }
    let mut canonical = Vec::with_capacity(subtags.len());
    canonical.push(language.to_ascii_lowercase());
    if let Some(script) = script {
        let mut script = script.to_ascii_lowercase().into_bytes();
        script[0].make_ascii_uppercase();
        canonical.push(String::from_utf8(script).ok()?);
    }
    if let Some(region) = region {
        canonical.push(region.to_ascii_uppercase());
    }
    canonical.extend(variants.iter().map(|variant| variant.to_ascii_lowercase()));
    Some(canonical.join("-").into_boxed_str())
}

#[inline(always)]
const fn display_names_style_name(style: IntlDisplayNamesStyle) -> &'static [u8] {
    match style {
        IntlDisplayNamesStyle::Long => b"long",
        IntlDisplayNamesStyle::Short => b"short",
        IntlDisplayNamesStyle::Narrow => b"narrow",
    }
}

#[inline(always)]
const fn display_names_type_name(display_type: IntlDisplayNamesType) -> &'static [u8] {
    match display_type {
        IntlDisplayNamesType::Language => b"language",
        IntlDisplayNamesType::Region => b"region",
        IntlDisplayNamesType::Script => b"script",
        IntlDisplayNamesType::Currency => b"currency",
        IntlDisplayNamesType::Calendar => b"calendar",
        IntlDisplayNamesType::DateTimeField => b"dateTimeField",
    }
}

#[inline(always)]
const fn display_names_fallback_name(fallback: IntlDisplayNamesFallback) -> &'static [u8] {
    match fallback {
        IntlDisplayNamesFallback::Code => b"code",
        IntlDisplayNamesFallback::None => b"none",
    }
}

#[inline(always)]
const fn display_names_language_display_name(
    language_display: IntlDisplayNamesLanguageDisplay,
) -> &'static [u8] {
    match language_display {
        IntlDisplayNamesLanguageDisplay::Dialect => b"dialect",
        IntlDisplayNamesLanguageDisplay::Standard => b"standard",
    }
}
