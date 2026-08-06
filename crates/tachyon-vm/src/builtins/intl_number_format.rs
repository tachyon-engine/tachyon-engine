//! Provider-backed `Intl.NumberFormat` construction and default decimal formatting substrate.

use super::super::*;

struct IntlNumberFormatValueRoots<'a> {
    vm: VmRoots<'a>,
    state: NativeCallState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntlNumberFormatOutput {
    String,
    Parts,
}

impl Trace for IntlNumberFormatValueRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.state.trace(tracer);
    }
}

struct IntlNumberFormatBoundRoots<'a> {
    vm: VmRoots<'a>,
    target: Value,
    number_format: Value,
    name: Value,
    data: Option<GcRef<BoundFunctionData>>,
}

impl Trace for IntlNumberFormatBoundRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.target.trace(tracer);
        self.number_format.trace(tracer);
        self.name.trace(tracer);
        self.data.trace(tracer);
    }
}

impl Isolate {
    /// Starts locale canonicalization before the resumable NumberFormat option pipeline.
    pub(crate) fn begin_intl_number_format_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.start_intl_number_format_constructor(site)
    }

    /// Canonicalizes requested locales before provider capability filtering.
    pub(crate) fn begin_intl_number_format_supported_locales_of(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.start_intl_number_format_supported_locales_of(site)
    }

    /// Completes the locale stage for construction or supportedLocalesOf.
    pub(crate) fn resume_intl_number_format(
        &mut self,
        continuation: NativeContinuation,
        stage: IntlNumberFormatStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.resume_pending_intl_number_format(continuation, stage, value)
    }

    /// Returns one cached anonymous bound formatter after enforcing the receiver brand.
    pub(crate) fn call_intl_number_format_format_getter(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        if self
            .intl_number_format_reference_if_branded(site.this_value)
            .is_some()
        {
            return self
                .finish_intl_number_format_format_getter(Self::native_site(site), site.this_value);
        }
        self.begin_intl_number_format_unwrap(
            Self::native_site(site),
            IntlNumberFormatLegacyStage::FormatHasInstance,
            site.this_value,
        )
    }

    /// Returns or creates the cached bound formatter for an already unwrapped receiver.
    fn finish_intl_number_format_format_getter(
        &mut self,
        site: NativeContinuationSite,
        number_format_value: Value,
    ) -> Result<(), ExecutionError> {
        let number_format = self.intl_number_format_reference(number_format_value)?;
        let snapshot = self.intl_number_format_snapshot(number_format)?;
        if snapshot.cached_bound_format.as_immediate() != Some(Immediate::Undefined) {
            return self.write(
                site.caller_base,
                site.destination,
                snapshot.cached_bound_format,
            );
        }
        let format = self.allocate_intl_number_format_bound_format(number_format_value)?;
        self.set_intl_number_format_bound_format(number_format, format)?;
        self.write(site.caller_base, site.destination, format)
    }

    /// Converts one primitive mathematical value and formats it through the cached backend.
    pub(crate) fn begin_intl_number_format_format(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_intl_number_format_value(site, IntlNumberFormatOutput::String)
    }

    /// Starts the same resumable ToIntlMathematicalValue path for structured output.
    pub(crate) fn begin_intl_number_format_format_to_parts(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.begin_intl_number_format_value(site, IntlNumberFormatOutput::Parts)
    }

    /// Brands the receiver before any argument coercion and preserves the selected output mode.
    fn begin_intl_number_format_value(
        &mut self,
        site: &CallSite,
        output: IntlNumberFormatOutput,
    ) -> Result<(), ExecutionError> {
        self.intl_number_format_reference(site.this_value)?;
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.is_object_value(value) {
            let state = self.allocate_intl_number_format_value_state(NativeCallState {
                values: [
                    site.this_value,
                    boolean_value(output == IntlNumberFormatOutput::Parts),
                    Value::from_immediate(Immediate::Undefined),
                    Value::from_immediate(Immediate::Undefined),
                    Value::from_immediate(Immediate::Undefined),
                ],
                count: 0,
            })?;
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlNumberFormatValue,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        self.finish_intl_number_format_format(
            NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            },
            site.this_value,
            value,
            output,
        )
    }

    /// Continues ToIntlMathematicalValue after an object argument produced its primitive.
    pub(crate) fn resume_intl_number_format_value_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let output = if snapshot.values[1].as_immediate() == Some(Immediate::True) {
            IntlNumberFormatOutput::Parts
        } else {
            IntlNumberFormatOutput::String
        };
        self.finish_intl_number_format_format(site, snapshot.values[0], primitive, output)
    }

    /// Formats one primitive value through the immutable provider backend.
    fn finish_intl_number_format_format(
        &mut self,
        site: NativeContinuationSite,
        number_format: Value,
        value: Value,
        output: IntlNumberFormatOutput,
    ) -> Result<(), ExecutionError> {
        let number_format = self.intl_number_format_reference(number_format)?;
        let input = self.intl_mathematical_value(value)?;
        let payload = self.intl_number_format_snapshot(number_format)?.payload;
        match output {
            IntlNumberFormatOutput::String => {
                let formatted = self.heap.with_running_scope(|scope| {
                    let payload = scope.root(payload).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow(payload, self.types.intl_number_format_payload)
                            .map_err(ExecutionError::NoGcBorrow)?
                            .backend
                            .format(&input)
                            .map_err(ExecutionError::IntlProvider)
                    })
                })?;
                let result = self.allocate_runtime_string(
                    JsString::try_from_utf16(&formatted)
                        .map_err(ExecutionError::PropertyKeyString)?,
                )?;
                self.write(site.caller_base, site.destination, result)
            }
            IntlNumberFormatOutput::Parts => {
                let parts = self.heap.with_running_scope(|scope| {
                    let payload = scope.root(payload).map_err(ExecutionError::Root)?;
                    scope.with_no_gc_scope(|no_gc| {
                        no_gc
                            .borrow(payload, self.types.intl_number_format_payload)
                            .map_err(ExecutionError::NoGcBorrow)?
                            .backend
                            .format_to_parts(&input)
                            .map_err(ExecutionError::IntlProvider)
                    })
                })?;
                self.materialize_intl_number_format_parts(site, parts)
            }
        }
    }

    /// Formats the already extracted Number receiver after toLocaleString option processing.
    pub(crate) fn finish_intl_number_format_value_to_string(
        &mut self,
        site: NativeContinuationSite,
        number_format: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.finish_intl_number_format_format(
            site,
            number_format,
            value,
            IntlNumberFormatOutput::String,
        )
    }

    /// Creates the intrinsic Array and fresh `{ type, value }` records without observable push.
    fn materialize_intl_number_format_parts(
        &mut self,
        site: NativeContinuationSite,
        parts: IntlFormattedNumberParts,
    ) -> Result<(), ExecutionError> {
        self.validate_intl_number_format_parts(&parts)?;
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.NumberFormat"),
        )?;
        self.write(site.caller_base, site.destination, result)?;
        let type_key = self.intern_intrinsic_name(b"type")?;
        let value_key = self.intern_intrinsic_name(b"value")?;
        for (index, span) in parts.spans.iter().copied().enumerate() {
            let result = self.read(site.caller_base, site.destination)?;
            let part = self.create_ordinary_object()?;
            let index = u32::try_from(index).map_err(|_| ExecutionError::ArrayLengthOverflow)?;
            let property = self.property_key_atom(safe_integer_value(u64::from(index)))?;
            self.set_own_data_property(result, property, part)?;

            let (part_type, part) = self.allocate_runtime_string_retaining(
                JsString::try_from_latin1(intl_number_format_part_name(span.kind))
                    .map_err(ExecutionError::PropertyKeyString)?,
                part,
            )?;
            self.set_own_data_property(part, type_key, part_type)?;

            let result = self.read(site.caller_base, site.destination)?;
            let part = self
                .get_data_property(result, property)?
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            let start = usize::try_from(span.start)
                .map_err(|_| ExecutionError::NumberFormatInvalidDigit)?;
            let end =
                usize::try_from(span.end).map_err(|_| ExecutionError::NumberFormatInvalidDigit)?;
            let units = parts
                .formatted
                .get(start..end)
                .ok_or(ExecutionError::NumberFormatInvalidDigit)?;
            let (part_value, part) = self.allocate_runtime_string_retaining(
                JsString::try_from_utf16(units).map_err(ExecutionError::PropertyKeyString)?,
                part,
            )?;
            self.set_own_data_property(part, value_key, part_value)?;
        }
        Ok(())
    }

    /// Rejects malformed provider data before any partially populated result becomes observable.
    fn validate_intl_number_format_parts(
        &self,
        parts: &IntlFormattedNumberParts,
    ) -> Result<(), ExecutionError> {
        let mut cursor = 0_u32;
        for span in &parts.spans {
            if span.start != cursor || span.end <= span.start {
                return Err(ExecutionError::IntlProvider(HostProviderError::Failure(3)));
            }
            cursor = span.end;
        }
        let length = u32::try_from(parts.formatted.len())
            .map_err(|_| ExecutionError::IntlProvider(HostProviderError::Failure(3)))?;
        if cursor != length || (length != 0 && parts.spans.is_empty()) {
            return Err(ExecutionError::IntlProvider(HostProviderError::Failure(3)));
        }
        Ok(())
    }

    /// Allocates the fixed receiver state before a formatting argument can invoke JavaScript.
    fn allocate_intl_number_format_value_state(
        &mut self,
        state: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = IntlNumberFormatValueRoots {
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

    /// Publishes a fresh resolved-options object in ECMA-402 property order.
    pub(crate) fn call_intl_number_format_resolved_options(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        if self
            .intl_number_format_reference_if_branded(site.this_value)
            .is_some()
        {
            return self.finish_intl_number_format_resolved_options(
                Self::native_site(site),
                site.this_value,
            );
        }
        self.begin_intl_number_format_unwrap(
            Self::native_site(site),
            IntlNumberFormatLegacyStage::ResolvedOptionsHasInstance,
            site.this_value,
        )
    }

    /// Materializes resolved options for an already unwrapped NumberFormat object.
    fn finish_intl_number_format_resolved_options(
        &mut self,
        site: NativeContinuationSite,
        number_format_value: Value,
    ) -> Result<(), ExecutionError> {
        let number_format = self.intl_number_format_reference(number_format_value)?;
        let resolved = self.intl_number_format_resolved(number_format)?;
        let result = self.create_ordinary_object()?;
        self.write(site.caller_base, site.destination, result)?;
        self.set_intl_number_format_string(result, b"locale", resolved.locale.as_bytes())?;
        self.set_intl_number_format_string(
            result,
            b"numberingSystem",
            resolved.numbering_system.as_bytes(),
        )?;
        let style = match resolved.options.style {
            IntlNumberFormatStyle::Decimal => b"decimal".as_slice(),
            IntlNumberFormatStyle::Percent => b"percent".as_slice(),
            IntlNumberFormatStyle::Currency => b"currency".as_slice(),
            IntlNumberFormatStyle::Unit => b"unit".as_slice(),
        };
        self.set_intl_number_format_string(result, b"style", style)?;
        if let Some(currency) = resolved.options.currency.as_deref() {
            self.set_intl_number_format_string(result, b"currency", currency.as_bytes())?;
            let display = match resolved.options.currency_display {
                IntlNumberFormatCurrencyDisplay::Code => b"code".as_slice(),
                IntlNumberFormatCurrencyDisplay::Symbol => b"symbol".as_slice(),
                IntlNumberFormatCurrencyDisplay::NarrowSymbol => b"narrowSymbol".as_slice(),
                IntlNumberFormatCurrencyDisplay::Name => b"name".as_slice(),
            };
            self.set_intl_number_format_string(result, b"currencyDisplay", display)?;
            let sign = match resolved.options.currency_sign {
                IntlNumberFormatCurrencySign::Standard => b"standard".as_slice(),
                IntlNumberFormatCurrencySign::Accounting => b"accounting".as_slice(),
            };
            self.set_intl_number_format_string(result, b"currencySign", sign)?;
        }
        if let Some(unit) = resolved.options.unit.as_deref() {
            self.set_intl_number_format_string(result, b"unit", unit.as_bytes())?;
            let display = match resolved.options.unit_display {
                IntlNumberFormatUnitDisplay::Short => b"short".as_slice(),
                IntlNumberFormatUnitDisplay::Narrow => b"narrow".as_slice(),
                IntlNumberFormatUnitDisplay::Long => b"long".as_slice(),
            };
            self.set_intl_number_format_string(result, b"unitDisplay", display)?;
        }
        self.set_intl_number_format_number(
            result,
            b"minimumIntegerDigits",
            u32::from(resolved.options.minimum_integer_digits),
        )?;
        if let Some(value) = resolved.options.minimum_fraction_digits {
            self.set_intl_number_format_number(result, b"minimumFractionDigits", u32::from(value))?;
        }
        if let Some(value) = resolved.options.maximum_fraction_digits {
            self.set_intl_number_format_number(result, b"maximumFractionDigits", u32::from(value))?;
        }
        if let Some(value) = resolved.options.minimum_significant_digits {
            self.set_intl_number_format_number(
                result,
                b"minimumSignificantDigits",
                u32::from(value),
            )?;
        }
        if let Some(value) = resolved.options.maximum_significant_digits {
            self.set_intl_number_format_number(
                result,
                b"maximumSignificantDigits",
                u32::from(value),
            )?;
        }
        match resolved.options.use_grouping {
            IntlNumberFormatUseGrouping::Never => {
                let key = self.intern_intrinsic_name(b"useGrouping")?;
                self.set_own_data_property(result, key, boolean_value(false))?;
            }
            IntlNumberFormatUseGrouping::Min2 => {
                self.set_intl_number_format_string(result, b"useGrouping", b"min2")?;
            }
            IntlNumberFormatUseGrouping::Auto => {
                self.set_intl_number_format_string(result, b"useGrouping", b"auto")?;
            }
            IntlNumberFormatUseGrouping::Always => {
                self.set_intl_number_format_string(result, b"useGrouping", b"always")?;
            }
        }
        let notation = match resolved.options.notation {
            IntlNumberFormatNotation::Standard => b"standard".as_slice(),
            IntlNumberFormatNotation::Scientific => b"scientific".as_slice(),
            IntlNumberFormatNotation::Engineering => b"engineering".as_slice(),
            IntlNumberFormatNotation::Compact => b"compact".as_slice(),
        };
        self.set_intl_number_format_string(result, b"notation", notation)?;
        if resolved.options.notation == IntlNumberFormatNotation::Compact {
            let compact = match resolved.options.compact_display {
                IntlNumberFormatCompactDisplay::Short => b"short".as_slice(),
                IntlNumberFormatCompactDisplay::Long => b"long".as_slice(),
            };
            self.set_intl_number_format_string(result, b"compactDisplay", compact)?;
        }
        let sign_display = match resolved.options.sign_display {
            IntlNumberFormatSignDisplay::Auto => b"auto".as_slice(),
            IntlNumberFormatSignDisplay::Never => b"never".as_slice(),
            IntlNumberFormatSignDisplay::Always => b"always".as_slice(),
            IntlNumberFormatSignDisplay::ExceptZero => b"exceptZero".as_slice(),
            IntlNumberFormatSignDisplay::Negative => b"negative".as_slice(),
        };
        self.set_intl_number_format_string(result, b"signDisplay", sign_display)?;
        self.set_intl_number_format_number(
            result,
            b"roundingIncrement",
            u32::from(resolved.options.rounding_increment),
        )?;
        let rounding_mode = match resolved.options.rounding_mode {
            IntlNumberFormatRoundingMode::Ceil => b"ceil".as_slice(),
            IntlNumberFormatRoundingMode::Floor => b"floor".as_slice(),
            IntlNumberFormatRoundingMode::Expand => b"expand".as_slice(),
            IntlNumberFormatRoundingMode::Trunc => b"trunc".as_slice(),
            IntlNumberFormatRoundingMode::HalfCeil => b"halfCeil".as_slice(),
            IntlNumberFormatRoundingMode::HalfFloor => b"halfFloor".as_slice(),
            IntlNumberFormatRoundingMode::HalfExpand => b"halfExpand".as_slice(),
            IntlNumberFormatRoundingMode::HalfTrunc => b"halfTrunc".as_slice(),
            IntlNumberFormatRoundingMode::HalfEven => b"halfEven".as_slice(),
        };
        self.set_intl_number_format_string(result, b"roundingMode", rounding_mode)?;
        let priority = match resolved.options.rounding_priority {
            IntlNumberFormatRoundingPriority::Auto => b"auto".as_slice(),
            IntlNumberFormatRoundingPriority::MorePrecision => b"morePrecision".as_slice(),
            IntlNumberFormatRoundingPriority::LessPrecision => b"lessPrecision".as_slice(),
        };
        self.set_intl_number_format_string(result, b"roundingPriority", priority)?;
        let trailing = match resolved.options.trailing_zero_display {
            IntlNumberFormatTrailingZeroDisplay::Auto => b"auto".as_slice(),
            IntlNumberFormatTrailingZeroDisplay::StripIfInteger => b"stripIfInteger".as_slice(),
        };
        self.set_intl_number_format_string(result, b"trailingZeroDisplay", trailing)
    }

    /// Applies the normative-optional ChainNumberFormat operation after initialization.
    pub(crate) fn begin_intl_number_format_chain(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        number_format: Value,
    ) -> Result<(), ExecutionError> {
        let constructor = self
            .realm
            .intl_number_format_constructor
            .expect("Intl.NumberFormat constructor initializes before legacy chaining");
        self.dispatch_intl_number_format_legacy(
            NativeContinuation::intl_number_format_legacy(
                site,
                IntlNumberFormatLegacyStage::ChainHasInstance,
                receiver,
                number_format,
            ),
            |isolate| isolate.begin_ordinary_has_instance(site, constructor, receiver),
        )
    }

    /// Starts OrdinaryHasInstance for one of the two UnwrapNumberFormat consumers.
    fn begin_intl_number_format_unwrap(
        &mut self,
        site: NativeContinuationSite,
        stage: IntlNumberFormatLegacyStage,
        receiver: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(receiver) {
            return Err(ExecutionError::IncompatibleIntlNumberFormatReceiver(
                receiver,
            ));
        }
        let constructor = self
            .realm
            .intl_number_format_constructor
            .expect("Intl.NumberFormat constructor initializes before unwrap");
        self.dispatch_intl_number_format_legacy(
            NativeContinuation::intl_number_format_legacy(
                site,
                stage,
                receiver,
                Value::from_immediate(Immediate::Undefined),
            ),
            |isolate| isolate.begin_ordinary_has_instance(site, constructor, receiver),
        )
    }

    /// Resumes ChainNumberFormat or UnwrapNumberFormat after an observable nested operation.
    pub(crate) fn resume_intl_number_format_legacy(
        &mut self,
        continuation: NativeContinuation,
        stage: IntlNumberFormatLegacyStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        let receiver = continuation.first();
        match stage {
            IntlNumberFormatLegacyStage::ChainHasInstance => {
                let number_format = continuation.second();
                if !self.is_truthy_value(value)? {
                    return self.write(site.caller_base, site.destination, number_format);
                }
                self.define_intl_number_format_legacy_fallback(site, receiver, number_format)
            }
            IntlNumberFormatLegacyStage::ChainDefine => {
                self.write(site.caller_base, site.destination, receiver)
            }
            IntlNumberFormatLegacyStage::FormatHasInstance
            | IntlNumberFormatLegacyStage::ResolvedOptionsHasInstance => {
                if !self.is_truthy_value(value)? {
                    return Err(ExecutionError::IncompatibleIntlNumberFormatReceiver(
                        receiver,
                    ));
                }
                let get_stage = if stage == IntlNumberFormatLegacyStage::FormatHasInstance {
                    IntlNumberFormatLegacyStage::FormatFallbackGet
                } else {
                    IntlNumberFormatLegacyStage::ResolvedOptionsFallbackGet
                };
                self.dispatch_intl_number_format_fallback_get(site, get_stage, receiver)
            }
            IntlNumberFormatLegacyStage::FormatFallbackGet => {
                self.finish_intl_number_format_format_getter(site, value)
            }
            IntlNumberFormatLegacyStage::ResolvedOptionsFallbackGet => {
                self.finish_intl_number_format_resolved_options(site, value)
            }
        }
    }

    /// Defines the hidden fallback edge through ordinary or Proxy [[DefineOwnProperty]].
    fn define_intl_number_format_legacy_fallback(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        number_format: Value,
    ) -> Result<(), ExecutionError> {
        let key = self.intl_number_format_legacy_key()?;
        let descriptor = PropertyDescriptor::Data(DataPropertyDescriptor {
            value: Some(number_format),
            writable: Some(false),
            enumerable: Some(false),
            configurable: Some(false),
        });
        if !self.is_proxy_value(receiver) {
            self.define_property(receiver, key, descriptor)?;
            return self.write(site.caller_base, site.destination, receiver);
        }
        self.dispatch_intl_number_format_legacy(
            NativeContinuation::intl_number_format_legacy(
                site,
                IntlNumberFormatLegacyStage::ChainDefine,
                receiver,
                number_format,
            ),
            |isolate| {
                isolate.dispatch_proxy_define(
                    site,
                    receiver,
                    key,
                    descriptor,
                    ProxyDefineMode::Object,
                )
            },
        )
    }

    /// Performs the observable fallback-symbol Get required by UnwrapNumberFormat.
    fn dispatch_intl_number_format_fallback_get(
        &mut self,
        site: NativeContinuationSite,
        stage: IntlNumberFormatLegacyStage,
        receiver: Value,
    ) -> Result<(), ExecutionError> {
        let key = self.intl_number_format_legacy_key()?;
        self.dispatch_intl_number_format_legacy(
            NativeContinuation::intl_number_format_legacy(
                site,
                stage,
                receiver,
                Value::from_immediate(Immediate::Undefined),
            ),
            |isolate| isolate.dispatch_proxy_aware_property_read(site, receiver, receiver, key),
        )
    }

    /// Drains a synchronous nested MOP or leaves the typed parent below a JavaScript frame.
    fn dispatch_intl_number_format_legacy(
        &mut self,
        continuation: NativeContinuation,
        operation: impl FnOnce(&mut Self) -> Result<Option<RunOutcome>, ExecutionError>,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        let outcome = match operation(self) {
            Ok(outcome) => outcome,
            Err(error) => {
                if self.fiber.completions.len() > completion_depth {
                    self.pop_native_continuation()?;
                }
                return Err(error);
            }
        };
        if self.fiber.completions.len() == completion_depth
            || self.fiber.frames.len() != frame_depth
        {
            return Ok(());
        }
        debug_assert!(outcome.is_none());
        let continuation = self.pop_native_continuation()?;
        let value = self.read(
            continuation.site().caller_base,
            continuation.site().destination,
        )?;
        let NativeContinuationKind::IntlNumberFormatLegacy(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_intl_number_format_legacy(continuation, stage, value)
    }

    /// Returns the per-Realm hidden fallback Symbol as a property key.
    fn intl_number_format_legacy_key(&mut self) -> Result<PropertyKey, ExecutionError> {
        let symbol = self
            .realm
            .intl_legacy_constructed_symbol
            .expect("Intl fallback symbol initializes before NumberFormat use");
        self.property_key(symbol)
    }

    /// Converts supported primitive Number/BigInt inputs without retaining managed borrows.
    fn intl_mathematical_value(
        &mut self,
        value: Value,
    ) -> Result<IntlMathematicalValue, ExecutionError> {
        if self.is_bigint_value(value) {
            let bytes = self.bigint_decimal_bytes(value)?;
            let value = String::from_utf8(bytes)
                .map(String::into_boxed_str)
                .map_err(|_| ExecutionError::NumberFormatInvalidDigit)?;
            return Ok(IntlMathematicalValue::Finite(value));
        }
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        if number.is_nan() {
            return Ok(IntlMathematicalValue::NaN);
        }
        if number == 0.0 && number.is_sign_negative() {
            return Ok(IntlMathematicalValue::NegativeZero);
        }
        if number == f64::INFINITY {
            return Ok(IntlMathematicalValue::PositiveInfinity);
        }
        if number == f64::NEG_INFINITY {
            return Ok(IntlMathematicalValue::NegativeInfinity);
        }
        let string = self.number_to_string(Value::from_f64(number), None)?;
        let units = self.string_value_to_utf16(string)?;
        let value = String::from_utf16(&units)
            .map(String::into_boxed_str)
            .map_err(|_| ExecutionError::NumberFormatInvalidDigit)?;
        Ok(IntlMathematicalValue::Finite(value))
    }

    fn intl_number_format_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<IntlNumberFormatObject>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::IncompatibleIntlNumberFormatReceiver(value))?;
        self.heap
            .checked_reference(raw, self.types.intl_number_format_object)
            .map_err(|_| ExecutionError::IncompatibleIntlNumberFormatReceiver(value))
    }

    #[inline(always)]
    fn intl_number_format_reference_if_branded(
        &self,
        value: Value,
    ) -> Option<GcRef<IntlNumberFormatObject>> {
        let raw = value.as_heap_ref()?;
        self.heap
            .checked_reference(raw, self.types.intl_number_format_object)
            .ok()
    }

    fn intl_number_format_snapshot(
        &mut self,
        number_format: GcRef<IntlNumberFormatObject>,
    ) -> Result<IntlNumberFormatObject, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let number_format = scope.root(number_format).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(number_format, self.types.intl_number_format_object)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn intl_number_format_resolved(
        &mut self,
        number_format: GcRef<IntlNumberFormatObject>,
    ) -> Result<IntlNumberFormatResolved, ExecutionError> {
        let payload = self.intl_number_format_snapshot(number_format)?.payload;
        self.heap.with_running_scope(|scope| {
            let payload = scope.root(payload).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(payload, self.types.intl_number_format_payload)
                    .map(|payload| payload.resolved.clone())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    fn set_intl_number_format_bound_format(
        &mut self,
        number_format: GcRef<IntlNumberFormatObject>,
        format: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let number_format = scope.root(number_format).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(number_format, self.types.intl_number_format_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .cached_bound_format = format;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(number_format, format)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Allocates an anonymous bound function with length one and no bound arguments.
    fn allocate_intl_number_format_bound_format(
        &mut self,
        number_format: Value,
    ) -> Result<Value, ExecutionError> {
        let target = self
            .realm
            .intl_number_format_format
            .expect("Intl.NumberFormat format target initializes before access");
        let name = self.allocate_runtime_string(
            JsString::try_from_latin1(b"").map_err(ExecutionError::PropertyKeyString)?,
        )?;
        let realm = self.realm_for_callable(target)?;
        let prototype = self.resolve_function_object(target)?.ordinary.prototype;
        let mut roots = IntlNumberFormatBoundRoots {
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
            number_format,
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
                    bound_this: number_format,
                    arguments: Box::new([]),
                    length: Value::from_i32(1),
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

    fn set_intl_number_format_string(
        &mut self,
        result: Value,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), ExecutionError> {
        let (value, result) = self.allocate_runtime_string_retaining(
            JsString::try_from_latin1(value).map_err(ExecutionError::PropertyKeyString)?,
            result,
        )?;
        let key = self.intern_intrinsic_name(key)?;
        self.set_own_data_property(result, key, value)
    }

    fn set_intl_number_format_number(
        &mut self,
        result: Value,
        key: &[u8],
        value: u32,
    ) -> Result<(), ExecutionError> {
        let key = self.intern_intrinsic_name(key)?;
        self.set_own_data_property(result, key, safe_integer_value(u64::from(value)))
    }
}

#[inline(always)]
const fn intl_number_format_part_name(kind: IntlNumberFormatPartType) -> &'static [u8] {
    match kind {
        IntlNumberFormatPartType::Literal => b"literal",
        IntlNumberFormatPartType::Nan => b"nan",
        IntlNumberFormatPartType::Infinity => b"infinity",
        IntlNumberFormatPartType::Integer => b"integer",
        IntlNumberFormatPartType::Group => b"group",
        IntlNumberFormatPartType::Decimal => b"decimal",
        IntlNumberFormatPartType::Fraction => b"fraction",
        IntlNumberFormatPartType::PlusSign => b"plusSign",
        IntlNumberFormatPartType::MinusSign => b"minusSign",
        IntlNumberFormatPartType::PercentSign => b"percentSign",
        IntlNumberFormatPartType::Currency => b"currency",
        IntlNumberFormatPartType::Unit => b"unit",
        IntlNumberFormatPartType::ExponentSeparator => b"exponentSeparator",
        IntlNumberFormatPartType::ExponentMinusSign => b"exponentMinusSign",
        IntlNumberFormatPartType::ExponentInteger => b"exponentInteger",
        IntlNumberFormatPartType::Compact => b"compact",
        IntlNumberFormatPartType::ApproximatelySign => b"approximatelySign",
    }
}
