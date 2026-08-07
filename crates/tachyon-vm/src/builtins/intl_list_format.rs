//! Provider-backed `Intl.ListFormat` construction and resolved scalar surface.

use super::super::*;
use crate::runtime::fiber::IntlListFormatStage;

const UNDEFINED: Value = Value::from_immediate(Immediate::Undefined);

/// GC-managed constructor state retained across locale and option callbacks.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingIntlListFormat {
    new_target: Value,
    options: Value,
    locales: Value,
    locale_matcher: IntlLocaleMatcher,
    list_type: IntlListFormatType,
    style: IntlListFormatStyle,
    stage: IntlListFormatStage,
    supported_locales: bool,
}

impl Trace for PendingIntlListFormat {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.new_target.trace(tracer);
        self.options.trace(tracer);
        self.locales.trace(tracer);
    }
}

struct PendingIntlListFormatRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingIntlListFormat,
}

impl Trace for PendingIntlListFormatRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl PendingIntlListFormat {
    #[inline]
    fn new(new_target: Value, options: Value, supported_locales: bool) -> Self {
        Self {
            new_target,
            options,
            locales: UNDEFINED,
            locale_matcher: IntlLocaleMatcher::BestFit,
            list_type: IntlListFormatType::Conjunction,
            style: IntlListFormatStyle::Long,
            stage: IntlListFormatStage::Locales,
            supported_locales,
        }
    }
}

impl Isolate {
    /// Starts locale canonicalization before reading ListFormat options.
    pub(crate) fn begin_intl_list_format_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(site.new_target) {
            return Err(ExecutionError::NonConstructor(site.callee));
        }
        self.begin_intl_list_format_options(site, site.new_target, false)
    }

    /// Starts locale filtering for the static supportedLocalesOf method.
    pub(crate) fn begin_intl_list_format_supported_locales_of(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_intl_list_format_options(site, UNDEFINED, true)
    }

    /// Allocates pending state and nests the shared CanonicalizeLocaleList machine beneath it.
    fn begin_intl_list_format_options(
        &mut self,
        site: &CallSite,
        new_target: Value,
        supported_locales: bool,
    ) -> Result<(), ExecutionError> {
        let locales = self.call_argument(site, 0)?.unwrap_or(UNDEFINED);
        let options = self.call_argument(site, 1)?.unwrap_or(UNDEFINED);
        let pending = self.allocate_pending_intl_list_format(PendingIntlListFormat::new(
            new_target,
            options,
            supported_locales,
        ))?;
        let continuation_site = Self::native_site(site);
        self.dispatch_intl_list_format_nested(
            NativeContinuation::intl_list_format(
                continuation_site,
                IntlListFormatStage::Locales,
                Value::from_heap_ref(pending.raw()),
                locales,
            ),
            |isolate| isolate.begin_intl_get_canonical_locales(site),
        )
    }

    /// Resumes locale canonicalization or one observable string option.
    pub(crate) fn resume_intl_list_format(
        &mut self,
        continuation: NativeContinuation,
        stage: IntlListFormatStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if matches!(
            stage,
            IntlListFormatStage::Format | IntlListFormatStage::FormatToParts
        ) {
            return self.finish_intl_list_format_output(
                continuation.site(),
                continuation.first(),
                value,
                stage == IntlListFormatStage::FormatToParts,
            );
        }
        let state = self.pending_intl_list_format_reference(continuation.first())?;
        if stage == IntlListFormatStage::Locales {
            self.set_pending_intl_list_format_value(
                state,
                IntlListFormatValueSlot::Locales,
                value,
            )?;
            let snapshot = self.pending_intl_list_format_snapshot(state)?;
            if snapshot.options == UNDEFINED {
                return if snapshot.supported_locales {
                    self.finish_intl_list_format_supported_locales(continuation.site(), state)
                } else {
                    self.finish_intl_list_format_construction(continuation.site(), state)
                };
            }
            self.write(
                continuation.site().caller_base,
                continuation.site().destination,
                Value::from_heap_ref(state.raw()),
            )?;
            let options = if snapshot.supported_locales {
                self.coerce_to_object(snapshot.options)?
            } else {
                if !self.is_object_value(snapshot.options) {
                    return Err(ExecutionError::NotObject(snapshot.options));
                }
                snapshot.options
            };
            let state = self.pending_intl_list_format_reference(self.read(
                continuation.site().caller_base,
                continuation.site().destination,
            )?)?;
            self.set_pending_intl_list_format_value(
                state,
                IntlListFormatValueSlot::Options,
                options,
            )?;
            return self.dispatch_intl_list_format_option_get(
                continuation.site(),
                state,
                IntlListFormatStage::LocaleMatcher,
            );
        }
        if value == UNDEFINED {
            return self.advance_intl_list_format_option(continuation.site(), state, stage);
        }
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlListFormatOption,
                continuation.site().caller_base,
                continuation.site().destination,
                Value::from_heap_ref(state.raw()),
                value,
                continuation.site().call_site,
            );
        }
        let string = self.primitive_to_string_value(value)?;
        self.resume_intl_list_format_option_string(continuation.site(), state, string)
    }

    /// Parses one converted string option and advances to the next observable Get.
    pub(crate) fn resume_intl_list_format_option_string(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlListFormat>,
        string: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_list_format_snapshot(state)?;
        let text = self
            .string_value_to_ascii(string)
            .map_err(|_| ExecutionError::InvalidIntlListFormatOption)?;
        match snapshot.stage {
            IntlListFormatStage::LocaleMatcher => {
                let matcher = match text.as_str() {
                    "lookup" => IntlLocaleMatcher::Lookup,
                    "best fit" => IntlLocaleMatcher::BestFit,
                    _ => return Err(ExecutionError::InvalidIntlListFormatOption),
                };
                self.update_pending_intl_list_format(state, |pending| {
                    pending.locale_matcher = matcher;
                })?;
            }
            IntlListFormatStage::Type => {
                let list_type = match text.as_str() {
                    "conjunction" => IntlListFormatType::Conjunction,
                    "disjunction" => IntlListFormatType::Disjunction,
                    "unit" => IntlListFormatType::Unit,
                    _ => return Err(ExecutionError::InvalidIntlListFormatOption),
                };
                self.update_pending_intl_list_format(state, |pending| {
                    pending.list_type = list_type;
                })?;
            }
            IntlListFormatStage::Style => {
                let style = match text.as_str() {
                    "long" => IntlListFormatStyle::Long,
                    "short" => IntlListFormatStyle::Short,
                    "narrow" => IntlListFormatStyle::Narrow,
                    _ => return Err(ExecutionError::InvalidIntlListFormatOption),
                };
                self.update_pending_intl_list_format(state, |pending| pending.style = style)?;
            }
            IntlListFormatStage::Locales => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
            IntlListFormatStage::Format | IntlListFormatStage::FormatToParts => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
        }
        self.advance_intl_list_format_option(site, state, snapshot.stage)
    }

    fn advance_intl_list_format_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlListFormat>,
        stage: IntlListFormatStage,
    ) -> Result<(), ExecutionError> {
        let supported = self
            .pending_intl_list_format_snapshot(state)?
            .supported_locales;
        let next = match stage {
            IntlListFormatStage::LocaleMatcher if !supported => Some(IntlListFormatStage::Type),
            IntlListFormatStage::Type => Some(IntlListFormatStage::Style),
            IntlListFormatStage::LocaleMatcher | IntlListFormatStage::Style => None,
            IntlListFormatStage::Locales
            | IntlListFormatStage::Format
            | IntlListFormatStage::FormatToParts => {
                return Err(ExecutionError::MissingNativeContinuation);
            }
        };
        let Some(next) = next else {
            return if supported {
                self.finish_intl_list_format_supported_locales(site, state)
            } else {
                self.finish_intl_list_format_construction(site, state)
            };
        };
        self.dispatch_intl_list_format_option_get(site, state, next)
    }

    /// Performs one Proxy/accessor-aware Get with a ListFormat-specific continuation.
    fn dispatch_intl_list_format_option_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlListFormat>,
        stage: IntlListFormatStage,
    ) -> Result<(), ExecutionError> {
        self.update_pending_intl_list_format(state, |pending| pending.stage = stage)?;
        let snapshot = self.pending_intl_list_format_snapshot(state)?;
        let key = self
            .intern_intrinsic_name(intl_list_format_option_name(stage))?
            .into();
        let continuation = NativeContinuation::intl_list_format(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            snapshot.options,
        );
        match self.resolve_property_read_until_proxy(snapshot.options, key)? {
            PropertyReadResolution::Read(PropertyRead::Missing) => {
                self.resume_intl_list_format(continuation, stage, UNDEFINED)
            }
            PropertyReadResolution::Read(PropertyRead::Data(value)) => {
                self.resume_intl_list_format(continuation, stage, value)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) if getter == UNDEFINED => {
                self.resume_intl_list_format(continuation, stage, UNDEFINED)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => self
                .dispatch_property_callback(
                    NativeContinuation::intl_list_format_property_get(
                        site,
                        Value::from_heap_ref(state.raw()),
                        snapshot.options,
                    ),
                    getter,
                )
                .map(|_| ()),
            PropertyReadResolution::Proxy(_) => {
                self.dispatch_intl_list_format_nested(continuation, |isolate| {
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

    /// Creates the branded object after provider locale resolution has completed.
    fn finish_intl_list_format_construction(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlListFormat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_list_format_snapshot(state)?;
        let request = IntlListFormatRequest {
            locales: self.intl_list_format_locale_strings(snapshot.locales)?,
            locale_matcher: snapshot.locale_matcher,
            list_type: snapshot.list_type,
            style: snapshot.style,
        };
        let resolved = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .create_list_format(request)
            .map_err(ExecutionError::IntlProvider)?;
        let (locale, retained) = self.allocate_runtime_string_retaining(
            JsString::try_from_str(&resolved.locale).map_err(ExecutionError::PropertyKeyString)?,
            Value::from_heap_ref(state.raw()),
        )?;
        let state = self.pending_intl_list_format_reference(retained)?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        let prototype_atom = self.prototype_atom()?;
        let state = self
            .pending_intl_list_format_reference(self.read(site.caller_base, site.destination)?)?;
        let new_target = self.pending_intl_list_format_snapshot(state)?.new_target;
        let candidate = self.constructor_prototype_value(new_target, prototype_atom)?;
        let state = self
            .pending_intl_list_format_reference(self.read(site.caller_base, site.destination)?)?;
        let new_target = self.pending_intl_list_format_snapshot(state)?.new_target;
        let prototype = if candidate.is_some_and(|value| self.is_object_value(value)) {
            candidate.expect("object candidate remains present")
        } else {
            self.realm_for_callable(new_target)
                .ok()
                .and_then(|realm| {
                    self.realm_intrinsic_prototype(realm, IntrinsicPrototypeKind::IntlListFormat)
                })
                .unwrap_or_else(|| {
                    self.realm
                        .intl_list_format_prototype
                        .expect("Intl.ListFormat prototype initializes before construction")
                })
        };
        let list_format = self.allocate_intl_list_format_object(
            locale,
            resolved.list_type,
            resolved.style,
            prototype,
            AllocationSpace::Young,
        )?;
        self.write(site.caller_base, site.destination, list_format)
    }

    /// Filters canonical requests through provider list-pattern capability and returns a fresh Array.
    fn finish_intl_list_format_supported_locales(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlListFormat>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_list_format_snapshot(state)?;
        let locales = self.intl_list_format_locale_strings(snapshot.locales)?;
        let supported = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .list_format_supported_locales(&locales, snapshot.locale_matcher)
            .map_err(ExecutionError::IntlProvider)?;
        self.materialize_intl_list_format_locales(site, supported)
    }

    /// Returns a fresh ordinary object with locale/type/style in specification order.
    pub(crate) fn intl_list_format_resolved_options(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.intl_list_format_reference(site.this_value)?;
        self.write(site.caller_base, site.destination, site.this_value)?;
        let result = self.create_ordinary_object_with_prototype(
            self.realm
                .object_prototype
                .expect("Object prototype initializes before Intl.ListFormat"),
        )?;
        let receiver = self.read(site.caller_base, site.destination)?;
        let snapshot = self.intl_list_format_snapshot(receiver)?;
        self.write(site.caller_base, site.destination, result)?;
        self.set_intl_list_format_resolved_string(result, b"locale", snapshot.locale)?;
        let list_type = match snapshot.list_type {
            IntlListFormatType::Conjunction => b"conjunction".as_slice(),
            IntlListFormatType::Disjunction => b"disjunction".as_slice(),
            IntlListFormatType::Unit => b"unit".as_slice(),
        };
        self.set_intl_list_format_resolved_ascii(result, b"type", list_type)?;
        let style = match snapshot.style {
            IntlListFormatStyle::Long => b"long".as_slice(),
            IntlListFormatStyle::Short => b"short".as_slice(),
            IntlListFormatStyle::Narrow => b"narrow".as_slice(),
        };
        self.set_intl_list_format_resolved_ascii(result, b"style", style)
    }

    /// Validates the receiver before collecting the iterable for string output.
    pub(crate) fn begin_intl_list_format_format(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_intl_list_format_output(site, IntlListFormatStage::Format)
    }

    /// Validates the receiver before collecting the iterable for structured output.
    pub(crate) fn begin_intl_list_format_format_to_parts(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_intl_list_format_output(site, IntlListFormatStage::FormatToParts)
    }

    /// Starts StringListFromIterable or directly formats the default empty list.
    fn begin_intl_list_format_output(
        &mut self,
        site: &CallSite,
        stage: IntlListFormatStage,
    ) -> Result<(), ExecutionError> {
        self.intl_list_format_reference(site.this_value)?;
        let list = self.call_argument(site, 0)?.unwrap_or(UNDEFINED);
        let native_site = Self::native_site(site);
        if list == UNDEFINED {
            return self.finish_intl_list_format_elements(
                native_site,
                site.this_value,
                Box::new([]),
                stage == IntlListFormatStage::FormatToParts,
            );
        }
        self.dispatch_intl_list_format_nested(
            NativeContinuation::intl_list_format(native_site, stage, site.this_value, list),
            |isolate| isolate.begin_intl_string_list(native_site, list),
        )
    }

    /// Extracts the validated intrinsic staging Array into provider-owned UTF-16 elements.
    fn finish_intl_list_format_output(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        list: Value,
        to_parts: bool,
    ) -> Result<(), ExecutionError> {
        let values = self.copy_packed_intl_array(list)?;
        let mut elements = Vec::new();
        elements
            .try_reserve_exact(values.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for value in values {
            if !self.is_string_value(value) {
                return Err(ExecutionError::InvalidIntlListFormatElement(value));
            }
            elements.push(self.string_value_to_utf16(value)?.into_boxed_slice());
        }
        self.finish_intl_list_format_elements(site, receiver, elements.into_boxed_slice(), to_parts)
    }

    /// Calls the provider with immutable resolved slots and materializes the selected result.
    fn finish_intl_list_format_elements(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        elements: Box<[Box<[u16]>]>,
        to_parts: bool,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.intl_list_format_snapshot(receiver)?;
        let resolved = IntlListFormatResolved {
            locale: self
                .string_value_to_ascii(snapshot.locale)?
                .into_boxed_str(),
            list_type: snapshot.list_type,
            style: snapshot.style,
        };
        let parts = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .format_list(&resolved, &elements)
            .map_err(ExecutionError::IntlProvider)?;
        if to_parts {
            self.materialize_intl_list_format_parts(site, parts)
        } else {
            let result = self.allocate_runtime_string(
                JsString::try_from_utf16(&parts.formatted)
                    .map_err(ExecutionError::PropertyKeyString)?,
            )?;
            self.write(site.caller_base, site.destination, result)
        }
    }

    /// Creates the intrinsic parts Array with fresh `{ type, value }` records.
    fn materialize_intl_list_format_parts(
        &mut self,
        site: NativeContinuationSite,
        parts: IntlFormattedListParts,
    ) -> Result<(), ExecutionError> {
        validate_intl_list_format_parts(&parts)?;
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.ListFormat"),
        )?;
        self.write(site.caller_base, site.destination, result)?;
        let type_key = self.intern_intrinsic_name(b"type")?;
        let value_key = self.intern_intrinsic_name(b"value")?;
        for (index, span) in parts.spans.iter().copied().enumerate() {
            let result = self.read(site.caller_base, site.destination)?;
            let part = self.create_ordinary_object()?;
            let property = self.property_key_atom(safe_integer_value(
                u64::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?,
            ))?;
            self.set_own_data_property(result, property, part)?;
            let name = match span.kind {
                IntlListFormatPartType::Element => b"element".as_slice(),
                IntlListFormatPartType::Literal => b"literal".as_slice(),
            };
            let (kind, part) = self.allocate_runtime_string_retaining(
                JsString::try_from_latin1(name).map_err(ExecutionError::PropertyKeyString)?,
                part,
            )?;
            self.set_own_data_property(part, type_key, kind)?;
            let result = self.read(site.caller_base, site.destination)?;
            let part = self
                .get_data_property(result, property)?
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            let start = usize::try_from(span.start)
                .map_err(|_| ExecutionError::IntlProvider(HostProviderError::Failure(6)))?;
            let end = usize::try_from(span.end)
                .map_err(|_| ExecutionError::IntlProvider(HostProviderError::Failure(6)))?;
            let units = parts
                .formatted
                .get(start..end)
                .ok_or(ExecutionError::IntlProvider(HostProviderError::Failure(6)))?;
            let (value, part) = self.allocate_runtime_string_retaining(
                JsString::try_from_utf16(units).map_err(ExecutionError::PropertyKeyString)?,
                part,
            )?;
            self.set_own_data_property(part, value_key, value)?;
        }
        Ok(())
    }

    fn intl_list_format_snapshot(
        &mut self,
        value: Value,
    ) -> Result<IntlListFormatObject, ExecutionError> {
        let object = self.intl_list_format_reference(value)?;
        self.heap.with_running_scope(|scope| {
            let object = scope.root(object).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(object, self.types.intl_list_format_object)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn intl_list_format_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<IntlListFormatObject>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::IncompatibleIntlListFormatReceiver(value))?;
        self.heap
            .checked_reference(raw, self.types.intl_list_format_object)
            .map_err(|_| ExecutionError::IncompatibleIntlListFormatReceiver(value))
    }

    fn intl_list_format_locale_strings(
        &mut self,
        locales: Value,
    ) -> Result<Box<[Box<str>]>, ExecutionError> {
        let values = self.copy_packed_intl_array(locales)?;
        let mut strings = Vec::new();
        strings
            .try_reserve_exact(values.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for value in values {
            strings.push(
                self.string_value_to_ascii(value)
                    .map(String::into_boxed_str)?,
            );
        }
        Ok(strings.into_boxed_slice())
    }

    /// Materializes provider-filtered locale names without observing Array prototype methods.
    fn materialize_intl_list_format_locales(
        &mut self,
        site: NativeContinuationSite,
        locales: Box<[Box<str>]>,
    ) -> Result<(), ExecutionError> {
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.ListFormat"),
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

    fn set_intl_list_format_resolved_string(
        &mut self,
        result: Value,
        name: &[u8],
        value: Value,
    ) -> Result<(), ExecutionError> {
        let key = self.intern_intrinsic_name(name)?;
        self.set_own_data_property(result, key, value)
    }

    fn set_intl_list_format_resolved_ascii(
        &mut self,
        result: Value,
        name: &[u8],
        value: &[u8],
    ) -> Result<(), ExecutionError> {
        let (value, result) = self.allocate_runtime_string_retaining(
            JsString::try_from_latin1(value).map_err(ExecutionError::PropertyKeyString)?,
            result,
        )?;
        self.set_intl_list_format_resolved_string(result, name, value)
    }

    fn allocate_pending_intl_list_format(
        &mut self,
        pending: PendingIntlListFormat,
    ) -> Result<GcRef<PendingIntlListFormat>, ExecutionError> {
        let mut roots = PendingIntlListFormatRoots {
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
                self.types.pending_intl_list_format,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_intl_list_format_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingIntlListFormat>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_intl_list_format)
            .map_err(ExecutionError::HeapReference)
    }

    fn pending_intl_list_format_snapshot(
        &mut self,
        state: GcRef<PendingIntlListFormat>,
    ) -> Result<PendingIntlListFormat, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_intl_list_format)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(crate) fn pending_intl_list_format_stage(
        &mut self,
        state: GcRef<PendingIntlListFormat>,
    ) -> Result<IntlListFormatStage, ExecutionError> {
        self.pending_intl_list_format_snapshot(state)
            .map(|pending| pending.stage)
    }

    fn update_pending_intl_list_format(
        &mut self,
        state: GcRef<PendingIntlListFormat>,
        update: impl FnOnce(&mut PendingIntlListFormat),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                update(
                    no_gc
                        .borrow_mut(state, self.types.pending_intl_list_format)
                        .map_err(ExecutionError::NoGcBorrow)?,
                );
                Ok(())
            })
        })
    }

    fn set_pending_intl_list_format_value(
        &mut self,
        state: GcRef<PendingIntlListFormat>,
        slot: IntlListFormatValueSlot,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_intl_list_format)
                    .map_err(ExecutionError::NoGcBorrow)?;
                match slot {
                    IntlListFormatValueSlot::Options => pending.options = value,
                    IntlListFormatValueSlot::Locales => pending.locales = value,
                }
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Drains synchronous Proxy reads while preserving the typed parent continuation.
    fn dispatch_intl_list_format_nested(
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
        let NativeContinuationKind::IntlListFormat(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_intl_list_format(continuation, stage, value)
    }
}

#[derive(Clone, Copy)]
enum IntlListFormatValueSlot {
    Options,
    Locales,
}

#[inline(always)]
const fn intl_list_format_option_name(stage: IntlListFormatStage) -> &'static [u8] {
    match stage {
        IntlListFormatStage::Locales => b"",
        IntlListFormatStage::LocaleMatcher => b"localeMatcher",
        IntlListFormatStage::Type => b"type",
        IntlListFormatStage::Style => b"style",
        IntlListFormatStage::Format | IntlListFormatStage::FormatToParts => b"",
    }
}

/// Rejects malformed provider spans before publishing any partially populated result.
fn validate_intl_list_format_parts(parts: &IntlFormattedListParts) -> Result<(), ExecutionError> {
    let mut cursor = 0_u32;
    for span in &parts.spans {
        if span.start != cursor || span.end < span.start {
            return Err(ExecutionError::IntlProvider(HostProviderError::Failure(6)));
        }
        cursor = span.end;
    }
    let length = u32::try_from(parts.formatted.len())
        .map_err(|_| ExecutionError::IntlProvider(HostProviderError::Failure(6)))?;
    if cursor != length || (length != 0 && parts.spans.is_empty()) {
        return Err(ExecutionError::IntlProvider(HostProviderError::Failure(6)));
    }
    Ok(())
}
