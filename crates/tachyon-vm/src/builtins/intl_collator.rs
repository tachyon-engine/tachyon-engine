//! Provider-backed `Intl.Collator` construction, branding, and comparison.

use super::super::*;

/// GC-managed constructor state retained across locale and option callbacks.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingIntlCollator {
    pub(crate) new_target: Value,
    pub(crate) options: Value,
    pub(crate) locales: Value,
    pub(crate) collation: Value,
    pub(crate) usage: IntlCollatorUsage,
    pub(crate) locale_matcher: IntlLocaleMatcher,
    pub(crate) numeric: Option<bool>,
    pub(crate) case_first: Option<IntlCollatorCaseFirst>,
    pub(crate) sensitivity: Option<IntlCollatorSensitivity>,
    pub(crate) ignore_punctuation: Option<bool>,
    pub(crate) stage: IntlCollatorStage,
    pub(crate) supported_locales: bool,
}

impl Trace for PendingIntlCollator {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.new_target.trace(tracer);
        self.options.trace(tracer);
        self.locales.trace(tracer);
        self.collation.trace(tracer);
    }
}

struct PendingIntlCollatorRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingIntlCollator,
}

impl Trace for PendingIntlCollatorRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

struct IntlCollatorBoundCompareRoots<'a> {
    vm: VmRoots<'a>,
    target: Value,
    collator: Value,
    name: Value,
    data: Option<GcRef<BoundFunctionData>>,
}

impl Trace for IntlCollatorBoundCompareRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.target.trace(tracer);
        self.collator.trace(tracer);
        self.name.trace(tracer);
        self.data.trace(tracer);
    }
}

struct IntlCollatorCompareRoots<'a> {
    vm: VmRoots<'a>,
    state: NativeCallState,
}

impl Trace for IntlCollatorCompareRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.state.trace(tracer);
    }
}

impl Isolate {
    /// Converts both comparison arguments with string hint before entering the cached backend.
    pub(crate) fn begin_intl_collator_compare(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.intl_collator_reference(site.this_value)?;
        let undefined = Value::from_immediate(Immediate::Undefined);
        let left = self.call_argument(site, 0)?.unwrap_or(undefined);
        let right = self.call_argument(site, 1)?.unwrap_or(undefined);
        let state = self.allocate_intl_collator_compare_state(NativeCallState {
            values: [site.this_value, left, right, undefined, undefined],
            count: 0,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.is_object_value(left) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlCollatorCompareLeft,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                left,
                site.call_site,
            );
        }
        let left = self.primitive_to_string_value(left)?;
        self.resume_intl_collator_compare_conversion(continuation_site, state, true, left)
    }

    /// Stores one converted operand and either converts the right side or compares immediately.
    pub(crate) fn resume_intl_collator_compare_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        left: bool,
        string: Value,
    ) -> Result<(), ExecutionError> {
        self.update_native_call_state_value(state, if left { 3 } else { 4 }, string)?;
        if left {
            let right = self.native_call_state_snapshot(state)?.values[2];
            if self.is_object_value(right) {
                return self.dispatch_object_primitive_conversion(
                    ConversionConsumer::IntlCollatorCompareRight,
                    site.caller_base,
                    site.destination,
                    Value::from_heap_ref(state.raw()),
                    right,
                    site.call_site,
                );
            }
            let right = self.primitive_to_string_value(right)?;
            self.update_native_call_state_value(state, 4, right)?;
        }
        self.finish_intl_collator_compare(site, state)
    }

    /// Borrows the immutable backend in a no-GC scope and writes the normalized comparison sign.
    fn finish_intl_collator_compare(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let collator = self.intl_collator_reference(snapshot.values[0])?;
        let backend = self.intl_collator_snapshot(collator)?.backend;
        let left = self.string_value_to_utf16(snapshot.values[3])?;
        let right = self.string_value_to_utf16(snapshot.values[4])?;
        let ordering = self.heap.with_running_scope(|scope| {
            let backend = scope.root(backend).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(backend, self.types.intl_collator_backend)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .backend
                    .compare_utf16(&left, &right)
                    .map_err(ExecutionError::IntlProvider)
            })
        })?;
        let result = match ordering {
            core::cmp::Ordering::Less => -1,
            core::cmp::Ordering::Equal => 0,
            core::cmp::Ordering::Greater => 1,
        };
        self.write(site.caller_base, site.destination, Value::from_i32(result))
    }

    /// Returns the cached anonymous bound comparator after enforcing the Collator brand.
    pub(crate) fn call_intl_collator_compare_getter(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let collator = self.intl_collator_reference(site.this_value)?;
        let snapshot = self.intl_collator_snapshot(collator)?;
        if snapshot.cached_bound_compare.as_immediate() != Some(Immediate::Undefined) {
            return self.write(
                site.caller_base,
                site.destination,
                snapshot.cached_bound_compare,
            );
        }
        let compare = self.allocate_intl_collator_bound_compare(site.this_value)?;
        self.set_intl_collator_bound_compare(collator, compare)?;
        self.write(site.caller_base, site.destination, compare)
    }

    /// Materializes a fresh ordinary resolved-options record in required property order.
    pub(crate) fn call_intl_collator_resolved_options(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let collator = self.intl_collator_reference(site.this_value)?;
        let snapshot = self.intl_collator_snapshot(collator)?;
        let result = self.create_ordinary_object()?;
        self.write(site.caller_base, site.destination, result)?;
        let locale_atom = self.intern_intrinsic_name(b"locale")?;
        self.set_own_data_property(result, locale_atom, snapshot.locale)?;
        let usage = match snapshot.usage {
            IntlCollatorUsage::Sort => b"sort".as_slice(),
            IntlCollatorUsage::Search => b"search".as_slice(),
        };
        self.set_intl_collator_resolved_string(site, result, b"usage", usage)?;
        let sensitivity = match snapshot.sensitivity {
            IntlCollatorSensitivity::Base => b"base".as_slice(),
            IntlCollatorSensitivity::Accent => b"accent".as_slice(),
            IntlCollatorSensitivity::Case => b"case".as_slice(),
            IntlCollatorSensitivity::Variant => b"variant".as_slice(),
        };
        self.set_intl_collator_resolved_string(site, result, b"sensitivity", sensitivity)?;
        let ignore_atom = self.intern_intrinsic_name(b"ignorePunctuation")?;
        self.set_own_data_property(
            result,
            ignore_atom,
            boolean_value(snapshot.ignore_punctuation),
        )?;
        let collation_atom = self.intern_intrinsic_name(b"collation")?;
        self.set_own_data_property(result, collation_atom, snapshot.collation)?;
        let numeric_atom = self.intern_intrinsic_name(b"numeric")?;
        self.set_own_data_property(result, numeric_atom, boolean_value(snapshot.numeric))?;
        let case_first = match snapshot.case_first {
            IntlCollatorCaseFirst::Upper => b"upper".as_slice(),
            IntlCollatorCaseFirst::Lower => b"lower".as_slice(),
            IntlCollatorCaseFirst::False => b"false".as_slice(),
        };
        self.set_intl_collator_resolved_string(site, result, b"caseFirst", case_first)
    }

    /// Canonicalizes the requested list before applying the provider's locale capability filter.
    pub(crate) fn begin_intl_collator_supported_locales_of(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let locales = self.call_argument(site, 0)?.unwrap_or(undefined);
        let options = self.call_argument(site, 1)?.unwrap_or(undefined);
        let pending = self.allocate_pending_intl_collator(PendingIntlCollator {
            new_target: undefined,
            options,
            locales: undefined,
            collation: undefined,
            usage: IntlCollatorUsage::Sort,
            locale_matcher: IntlLocaleMatcher::BestFit,
            numeric: None,
            case_first: None,
            sensitivity: None,
            ignore_punctuation: None,
            stage: IntlCollatorStage::Locales,
            supported_locales: true,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.dispatch_intl_collator_nested(
            NativeContinuation::intl_collator(
                continuation_site,
                IntlCollatorStage::Locales,
                Value::from_heap_ref(pending.raw()),
                locales,
            ),
            |isolate| isolate.begin_intl_get_canonical_locales(site),
        )
    }

    /// Starts locale canonicalization before reading Collator options in specification order.
    pub(crate) fn begin_intl_collator_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let new_target = if self.is_object_value(site.new_target) {
            site.new_target
        } else {
            site.callee
        };
        let locales = self.call_argument(site, 0)?.unwrap_or(undefined);
        let options = self.call_argument(site, 1)?.unwrap_or(undefined);
        let pending = self.allocate_pending_intl_collator(PendingIntlCollator {
            new_target,
            options,
            locales: undefined,
            collation: undefined,
            usage: IntlCollatorUsage::Sort,
            locale_matcher: IntlLocaleMatcher::BestFit,
            numeric: None,
            case_first: None,
            sensitivity: None,
            ignore_punctuation: None,
            stage: IntlCollatorStage::Locales,
            supported_locales: false,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.dispatch_intl_collator_nested(
            NativeContinuation::intl_collator(
                continuation_site,
                IntlCollatorStage::Locales,
                Value::from_heap_ref(pending.raw()),
                locales,
            ),
            |isolate| isolate.begin_intl_get_canonical_locales(site),
        )
    }

    /// Resumes locale canonicalization or one observable options property read.
    pub(crate) fn resume_intl_collator(
        &mut self,
        continuation: NativeContinuation,
        stage: IntlCollatorStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.pending_intl_collator_reference(continuation.first())?;
        if stage == IntlCollatorStage::Locales {
            self.set_pending_intl_collator_value(state, |pending| &mut pending.locales, value)?;
            let snapshot = self.pending_intl_collator_snapshot(state)?;
            let options = snapshot.options;
            if options.as_immediate() == Some(Immediate::Undefined) {
                if snapshot.supported_locales {
                    return self.finish_intl_collator_supported_locales(continuation.site(), state);
                }
                return self.finish_intl_collator_construction(continuation.site(), state);
            }
            let options = self.coerce_to_object(options)?;
            self.set_pending_intl_collator_value(state, |pending| &mut pending.options, options)?;
            return self.dispatch_intl_collator_option_get(
                continuation.site(),
                state,
                if snapshot.supported_locales {
                    IntlCollatorStage::LocaleMatcher
                } else {
                    IntlCollatorStage::Usage
                },
            );
        }
        if value.as_immediate() == Some(Immediate::Undefined) {
            return self.advance_intl_collator_option(continuation.site(), state, stage);
        }
        match stage {
            IntlCollatorStage::Numeric | IntlCollatorStage::IgnorePunctuation => {
                let boolean = self.is_truthy_value(value)?;
                self.update_pending_intl_collator(state, |pending| match stage {
                    IntlCollatorStage::Numeric => pending.numeric = Some(boolean),
                    IntlCollatorStage::IgnorePunctuation => {
                        pending.ignore_punctuation = Some(boolean);
                    }
                    _ => unreachable!("boolean Collator option stage"),
                })?;
                self.advance_intl_collator_option(continuation.site(), state, stage)
            }
            IntlCollatorStage::Usage
            | IntlCollatorStage::LocaleMatcher
            | IntlCollatorStage::Collation
            | IntlCollatorStage::CaseFirst
            | IntlCollatorStage::Sensitivity => {
                self.set_pending_intl_collator_stage(state, stage)?;
                if self.is_object_value(value) {
                    return self.dispatch_object_primitive_conversion(
                        ConversionConsumer::IntlCollatorOption,
                        continuation.site().caller_base,
                        continuation.site().destination,
                        Value::from_heap_ref(state.raw()),
                        value,
                        continuation.site().call_site,
                    );
                }
                let string = self.primitive_to_string_value(value)?;
                self.resume_intl_collator_option_string(continuation.site(), state, string)
            }
            IntlCollatorStage::Locales => {
                unreachable!("Collator stage handled before string conversion")
            }
        }
    }

    /// Validates one converted string option and advances to the next observable property.
    pub(crate) fn resume_intl_collator_option_string(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlCollator>,
        string: Value,
    ) -> Result<(), ExecutionError> {
        let stage = self.pending_intl_collator_snapshot(state)?.stage;
        let text = self.intl_ascii_string(string)?;
        match stage {
            IntlCollatorStage::Usage => {
                let usage = match text.as_ref() {
                    "sort" => IntlCollatorUsage::Sort,
                    "search" => IntlCollatorUsage::Search,
                    _ => return Err(ExecutionError::InvalidIntlCollatorOption),
                };
                self.update_pending_intl_collator(state, |pending| pending.usage = usage)?;
            }
            IntlCollatorStage::LocaleMatcher => {
                let matcher = match text.as_ref() {
                    "lookup" => IntlLocaleMatcher::Lookup,
                    "best fit" => IntlLocaleMatcher::BestFit,
                    _ => return Err(ExecutionError::InvalidIntlCollatorOption),
                };
                self.update_pending_intl_collator(state, |pending| {
                    pending.locale_matcher = matcher;
                })?;
            }
            IntlCollatorStage::Collation => {
                if !is_unicode_locale_type(&text) {
                    return Err(ExecutionError::InvalidIntlCollatorOption);
                }
                self.set_pending_intl_collator_value(
                    state,
                    |pending| &mut pending.collation,
                    string,
                )?;
            }
            IntlCollatorStage::CaseFirst => {
                let case_first = match text.as_ref() {
                    "upper" => IntlCollatorCaseFirst::Upper,
                    "lower" => IntlCollatorCaseFirst::Lower,
                    "false" => IntlCollatorCaseFirst::False,
                    _ => return Err(ExecutionError::InvalidIntlCollatorOption),
                };
                self.update_pending_intl_collator(state, |pending| {
                    pending.case_first = Some(case_first);
                })?;
            }
            IntlCollatorStage::Sensitivity => {
                let sensitivity = match text.as_ref() {
                    "base" => IntlCollatorSensitivity::Base,
                    "accent" => IntlCollatorSensitivity::Accent,
                    "case" => IntlCollatorSensitivity::Case,
                    "variant" => IntlCollatorSensitivity::Variant,
                    _ => return Err(ExecutionError::InvalidIntlCollatorOption),
                };
                self.update_pending_intl_collator(state, |pending| {
                    pending.sensitivity = Some(sensitivity);
                })?;
            }
            _ => return Err(ExecutionError::MissingNativeContinuation),
        }
        self.advance_intl_collator_option(site, state, stage)
    }

    /// Creates the provider request, allocates resolved strings, and publishes the branded object.
    fn finish_intl_collator_construction(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlCollator>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_collator_snapshot(state)?;
        let locale_values = self.copy_packed_intl_array(snapshot.locales)?;
        let mut locales = Vec::new();
        locales
            .try_reserve_exact(locale_values.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for locale in locale_values {
            locales.push(self.intl_ascii_string(locale)?);
        }
        let collation = (snapshot.collation.as_immediate() != Some(Immediate::Undefined))
            .then(|| self.intl_ascii_string(snapshot.collation))
            .transpose()?;
        let request = IntlCollatorRequest {
            locales: locales.into_boxed_slice(),
            locale_matcher: snapshot.locale_matcher,
            usage: snapshot.usage,
            collation,
            numeric: snapshot.numeric,
            case_first: snapshot.case_first,
            sensitivity: snapshot.sensitivity,
            ignore_punctuation: snapshot.ignore_punctuation,
        };
        let creation = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .create_collator(request)
            .map_err(ExecutionError::IntlProvider)?;
        let locale = JsString::try_from_str(&creation.resolved.locale)
            .map_err(ExecutionError::PropertyKeyString)?;
        let collation = JsString::try_from_str(&creation.resolved.collation)
            .map_err(ExecutionError::PropertyKeyString)?;
        let locale = self.allocate_runtime_string(locale)?;
        let (collation, locale) = self.allocate_runtime_string_retaining(collation, locale)?;
        let prototype_atom = self.prototype_atom()?;
        let default_prototype = self
            .realm
            .intl_collator_prototype
            .expect("Intl.Collator prototype initializes before construction");
        let prototype = self
            .constructor_prototype_value(snapshot.new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .or_else(|| {
                self.realm_for_callable(snapshot.new_target)
                    .ok()
                    .and_then(|realm| {
                        self.realm_intrinsic_prototype(realm, IntrinsicPrototypeKind::IntlCollator)
                    })
            })
            .unwrap_or(default_prototype);
        let resolved = creation.resolved;
        let collator = self.allocate_intl_collator_object(
            creation.backend,
            locale,
            collation,
            IntlCollatorResolvedOptions {
                usage: resolved.usage,
                sensitivity: resolved.sensitivity,
                case_first: resolved.case_first,
                ignore_punctuation: resolved.ignore_punctuation,
                numeric: resolved.numeric,
            },
            prototype,
            AllocationSpace::Young,
        )?;
        self.write(site.caller_base, site.destination, collator)
    }

    /// Dispatches the next option Get or finalizes after the last option.
    fn advance_intl_collator_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlCollator>,
        stage: IntlCollatorStage,
    ) -> Result<(), ExecutionError> {
        if stage == IntlCollatorStage::LocaleMatcher
            && self
                .pending_intl_collator_snapshot(state)?
                .supported_locales
        {
            return self.finish_intl_collator_supported_locales(site, state);
        }
        let next = match stage {
            IntlCollatorStage::Usage => IntlCollatorStage::LocaleMatcher,
            IntlCollatorStage::LocaleMatcher => IntlCollatorStage::Collation,
            IntlCollatorStage::Collation => IntlCollatorStage::Numeric,
            IntlCollatorStage::Numeric => IntlCollatorStage::CaseFirst,
            IntlCollatorStage::CaseFirst => IntlCollatorStage::Sensitivity,
            IntlCollatorStage::Sensitivity => IntlCollatorStage::IgnorePunctuation,
            IntlCollatorStage::IgnorePunctuation => {
                return self.finish_intl_collator_construction(site, state);
            }
            IntlCollatorStage::Locales => return Err(ExecutionError::MissingNativeContinuation),
        };
        self.dispatch_intl_collator_option_get(site, state, next)
    }

    /// Filters canonical locales and materializes a fresh intrinsic Array without observable push.
    fn finish_intl_collator_supported_locales(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlCollator>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_collator_snapshot(state)?;
        let locale_values = self.copy_packed_intl_array(snapshot.locales)?;
        let mut locales = Vec::new();
        locales
            .try_reserve_exact(locale_values.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for locale in locale_values {
            locales.push(self.intl_ascii_string(locale)?);
        }
        let supported = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .collator_supported_locales(&locales, snapshot.locale_matcher)
            .map_err(ExecutionError::IntlProvider)?;
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.Collator"),
        )?;
        self.write(site.caller_base, site.destination, result)?;
        for (index, locale) in supported.into_vec().into_iter().enumerate() {
            let result = self.read(site.caller_base, site.destination)?;
            let (locale, result) = self.allocate_runtime_string_retaining(
                JsString::try_from_str(&locale).map_err(ExecutionError::PropertyKeyString)?,
                result,
            )?;
            self.write(site.caller_base, site.destination, result)?;
            let index = u32::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
            let key = self.property_key_atom(safe_integer_value(u64::from(index)))?;
            let result = self.read(site.caller_base, site.destination)?;
            self.set_own_data_property(result, key, locale)?;
        }
        Ok(())
    }

    /// Performs one Proxy/accessor-aware options Get under a typed Collator continuation.
    fn dispatch_intl_collator_option_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlCollator>,
        stage: IntlCollatorStage,
    ) -> Result<(), ExecutionError> {
        self.set_pending_intl_collator_stage(state, stage)?;
        let snapshot = self.pending_intl_collator_snapshot(state)?;
        let name = match stage {
            IntlCollatorStage::Usage => b"usage".as_slice(),
            IntlCollatorStage::LocaleMatcher => b"localeMatcher".as_slice(),
            IntlCollatorStage::Collation => b"collation".as_slice(),
            IntlCollatorStage::Numeric => b"numeric".as_slice(),
            IntlCollatorStage::CaseFirst => b"caseFirst".as_slice(),
            IntlCollatorStage::Sensitivity => b"sensitivity".as_slice(),
            IntlCollatorStage::IgnorePunctuation => b"ignorePunctuation".as_slice(),
            IntlCollatorStage::Locales => return Err(ExecutionError::MissingNativeContinuation),
        };
        let key = self.intern_intrinsic_name(name)?.into();
        match self.resolve_property_read_until_proxy(snapshot.options, key)? {
            PropertyReadResolution::Read(PropertyRead::Missing) => {
                return self.resume_intl_collator(
                    NativeContinuation::intl_collator(
                        site,
                        stage,
                        Value::from_heap_ref(state.raw()),
                        snapshot.options,
                    ),
                    stage,
                    Value::from_immediate(Immediate::Undefined),
                );
            }
            PropertyReadResolution::Read(PropertyRead::Data(value)) => {
                return self.resume_intl_collator(
                    NativeContinuation::intl_collator(
                        site,
                        stage,
                        Value::from_heap_ref(state.raw()),
                        snapshot.options,
                    ),
                    stage,
                    value,
                );
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter))
                if getter.as_immediate() == Some(Immediate::Undefined) =>
            {
                return self.resume_intl_collator(
                    NativeContinuation::intl_collator(
                        site,
                        stage,
                        Value::from_heap_ref(state.raw()),
                        snapshot.options,
                    ),
                    stage,
                    Value::from_immediate(Immediate::Undefined),
                );
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => {
                return self
                    .dispatch_property_callback(
                        NativeContinuation::intl_collator_property_get(
                            site,
                            Value::from_heap_ref(state.raw()),
                            snapshot.options,
                        ),
                        getter,
                    )
                    .map(|_| ());
            }
            PropertyReadResolution::Proxy(_) => {}
        }
        self.dispatch_intl_collator_nested(
            NativeContinuation::intl_collator(
                site,
                stage,
                Value::from_heap_ref(state.raw()),
                snapshot.options,
            ),
            |isolate| {
                isolate
                    .dispatch_proxy_aware_property_read(
                        site,
                        snapshot.options,
                        snapshot.options,
                        key,
                    )
                    .map(|_| ())
            },
        )
    }

    /// Drains a synchronous nested operation or leaves its parent below the new JS frame.
    fn dispatch_intl_collator_nested(
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
        let NativeContinuationKind::IntlCollator(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_intl_collator(continuation, stage, value)
    }

    /// Allocates the compact constructor record under a root set containing all pending Values.
    fn allocate_pending_intl_collator(
        &mut self,
        pending: PendingIntlCollator,
    ) -> Result<GcRef<PendingIntlCollator>, ExecutionError> {
        let mut roots = PendingIntlCollatorRoots {
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
                self.types.pending_intl_collator,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Allocates the fixed five-Value compare state before either ToString callback can run.
    fn allocate_intl_collator_compare_state(
        &mut self,
        state: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = IntlCollatorCompareRoots {
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
            state,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.native_call_state,
                0,
                0,
                roots.state,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Recovers a checked pending Collator reference from a traced continuation Value.
    pub(crate) fn pending_intl_collator_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingIntlCollator>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_intl_collator)
            .map_err(ExecutionError::HeapReference)
    }

    /// Copies scalar constructor state without retaining a no-GC borrow across VM work.
    fn pending_intl_collator_snapshot(
        &mut self,
        state: GcRef<PendingIntlCollator>,
    ) -> Result<PendingIntlCollator, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_intl_collator)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Returns the option stage encoded in the pending record for a property callback resume.
    pub(crate) fn pending_intl_collator_stage(
        &mut self,
        state: GcRef<PendingIntlCollator>,
    ) -> Result<IntlCollatorStage, ExecutionError> {
        self.pending_intl_collator_snapshot(state)
            .map(|pending| pending.stage)
    }

    /// Updates non-Value state fields without retaining references across callbacks.
    fn update_pending_intl_collator(
        &mut self,
        state: GcRef<PendingIntlCollator>,
        update: impl FnOnce(&mut PendingIntlCollator),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_intl_collator)
                    .map_err(ExecutionError::NoGcBorrow)?;
                update(pending);
                Ok(())
            })
        })
    }

    /// Stores the current string-option stage for a resumable ToString callback.
    fn set_pending_intl_collator_stage(
        &mut self,
        state: GcRef<PendingIntlCollator>,
        stage: IntlCollatorStage,
    ) -> Result<(), ExecutionError> {
        self.update_pending_intl_collator(state, |pending| pending.stage = stage)
    }

    /// Replaces one traced pending Value and publishes its generational edge.
    fn set_pending_intl_collator_value(
        &mut self,
        state: GcRef<PendingIntlCollator>,
        field: impl FnOnce(&mut PendingIntlCollator) -> &mut Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_intl_collator)
                    .map_err(ExecutionError::NoGcBorrow)?;
                *field(pending) = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Copies the packed intrinsic Array emitted by CanonicalizeLocaleList.
    pub(crate) fn copy_packed_intl_array(
        &mut self,
        value: Value,
    ) -> Result<Vec<Value>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        let array = self
            .heap
            .checked_reference(raw, self.types.array)
            .map_err(ExecutionError::HeapReference)?;
        self.heap.with_running_scope(|scope| {
            let array = scope.root(array).map_err(ExecutionError::Root)?;
            let elements = scope.with_no_gc_scope(|no_gc| {
                let array = no_gc
                    .borrow(array, self.types.array)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok::<_, ExecutionError>(array.elements)
            })?;
            let Some(elements) = elements else {
                return Ok(Vec::new());
            };
            let elements = scope.root(elements).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(elements, self.types.array_elements)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .copy_packed_values()
                    .map_err(|()| ExecutionError::StringBufferAllocationFailed)?
                    .ok_or(ExecutionError::MissingNativeContinuation)
            })
        })
    }

    /// Copies an ECMAScript String whose Collator domain is restricted to ASCII identifiers.
    pub(crate) fn intl_ascii_string(&mut self, value: Value) -> Result<Box<str>, ExecutionError> {
        let units = self.string_value_to_utf16(value)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(units.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for unit in units {
            bytes.push(u8::try_from(unit).map_err(|_| ExecutionError::InvalidIntlCollatorOption)?);
        }
        String::from_utf8(bytes)
            .map(String::into_boxed_str)
            .map_err(|_| ExecutionError::InvalidIntlCollatorOption)
    }

    /// Recovers the unforgeable Collator payload or reports the branded receiver TypeError.
    fn intl_collator_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<IntlCollatorObject>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::IncompatibleIntlCollatorReceiver(value))?;
        self.heap
            .checked_reference(raw, self.types.intl_collator_object)
            .map_err(|_| ExecutionError::IncompatibleIntlCollatorReceiver(value))
    }

    /// Copies Collator slots without retaining a no-GC borrow across allocations or callbacks.
    fn intl_collator_snapshot(
        &mut self,
        collator: GcRef<IntlCollatorObject>,
    ) -> Result<IntlCollatorObject, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let collator = scope.root(collator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(collator, self.types.intl_collator_object)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Publishes the lazily created bound function through the Collator's traced cache edge.
    fn set_intl_collator_bound_compare(
        &mut self,
        collator: GcRef<IntlCollatorObject>,
        compare: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let collator = scope.root(collator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(collator, self.types.intl_collator_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .cached_bound_compare = compare;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(collator, compare)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Allocates the spec-shaped anonymous bound function with length two and no arguments.
    fn allocate_intl_collator_bound_compare(
        &mut self,
        collator: Value,
    ) -> Result<Value, ExecutionError> {
        let target = self
            .realm
            .intl_collator_compare
            .expect("Intl.Collator compare target initializes before access");
        let name = self.allocate_runtime_string(
            JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?,
        )?;
        let realm = self.realm_for_callable(target)?;
        let prototype = self.resolve_function_object(target)?.ordinary.prototype;
        let mut roots = IntlCollatorBoundCompareRoots {
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
            target,
            collator,
            name,
            data: None,
        };
        let data = self
            .heap
            .try_allocate_external_with_gc(
                self.types.bound_function,
                0,
                BoundFunctionData {
                    bound_target: target,
                    call_target: target,
                    bound_this: collator,
                    arguments: Box::new([]),
                    length: Value::from_i32(2),
                    name,
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        roots.data = Some(data);
        self.heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    executable: FunctionExecutable::Bound(data),
                    realm,
                    prototype_or_home_object: FunctionAuxiliaryEdge::NONE,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        extensible: true,
                        storage: None,
                        prototype,
                    },
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map(|function| Value::from_heap_ref(function.raw()))
            .map_err(ExecutionError::HeapAllocation)
    }

    /// Adds one resolved String while retaining the result across its allocation safepoint.
    fn set_intl_collator_resolved_string(
        &mut self,
        site: &CallSite,
        result: Value,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), ExecutionError> {
        let (value, result) = self.allocate_runtime_string_retaining(
            JsString::try_from_latin1(value).map_err(ExecutionError::PropertyKeyString)?,
            result,
        )?;
        self.write(site.caller_base, site.destination, result)?;
        let key = self.intern_intrinsic_name(key)?;
        self.set_own_data_property(result, key, value)
    }
}

/// Validates a Unicode locale type as alphanumeric subtags of length three through eight.
fn is_unicode_locale_type(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|subtag| {
            (3..=8).contains(&subtag.len())
                && subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}
