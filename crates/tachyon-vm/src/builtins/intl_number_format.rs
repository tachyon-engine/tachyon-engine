//! Provider-backed `Intl.NumberFormat` construction and default decimal formatting substrate.

use super::super::*;

struct IntlNumberFormatValueRoots<'a> {
    vm: VmRoots<'a>,
    state: NativeCallState,
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
        let number_format = self.intl_number_format_reference(site.this_value)?;
        let snapshot = self.intl_number_format_snapshot(number_format)?;
        if snapshot.cached_bound_format.as_immediate() != Some(Immediate::Undefined) {
            return self.write(
                site.caller_base,
                site.destination,
                snapshot.cached_bound_format,
            );
        }
        let format = self.allocate_intl_number_format_bound_format(site.this_value)?;
        self.set_intl_number_format_bound_format(number_format, format)?;
        self.write(site.caller_base, site.destination, format)
    }

    /// Converts one primitive mathematical value and formats it through the cached backend.
    pub(crate) fn begin_intl_number_format_format(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        self.intl_number_format_reference(site.this_value)?;
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if self.is_object_value(value) {
            let state = self.allocate_intl_number_format_value_state(NativeCallState {
                values: [
                    site.this_value,
                    Value::from_immediate(Immediate::Undefined),
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
        )
    }

    /// Continues ToIntlMathematicalValue after an object argument produced its primitive.
    pub(crate) fn resume_intl_number_format_value_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let number_format = self.native_call_state_snapshot(state)?.values[0];
        self.finish_intl_number_format_format(site, number_format, primitive)
    }

    /// Formats one primitive value through the immutable provider backend.
    fn finish_intl_number_format_format(
        &mut self,
        site: NativeContinuationSite,
        number_format: Value,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number_format = self.intl_number_format_reference(number_format)?;
        let input = self.intl_mathematical_value(value)?;
        let payload = self.intl_number_format_snapshot(number_format)?.payload;
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
            JsString::try_from_utf16(&formatted).map_err(ExecutionError::PropertyKeyString)?,
        )?;
        self.write(site.caller_base, site.destination, result)
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
        let number_format = self.intl_number_format_reference(site.this_value)?;
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
