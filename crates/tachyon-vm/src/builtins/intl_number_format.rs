//! Provider-backed `Intl.NumberFormat` construction and default decimal formatting substrate.

use super::super::*;

const NUMBER_FORMAT_NEW_TARGET: usize = 0;
const NUMBER_FORMAT_OPTIONS: usize = 1;
const NUMBER_FORMAT_LOCALES: usize = 2;

struct IntlNumberFormatStateRoots<'a> {
    vm: VmRoots<'a>,
    state: NativeCallState,
}

impl Trace for IntlNumberFormatStateRoots<'_> {
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
    /// Canonicalizes locales before provider construction; option expansion follows this substrate.
    pub(crate) fn begin_intl_number_format_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let locales = self.call_argument(site, 0)?.unwrap_or(undefined);
        let options = self.call_argument(site, 1)?.unwrap_or(undefined);
        let new_target = if self.is_object_value(site.new_target) {
            site.new_target
        } else {
            site.callee
        };
        let state = self.allocate_intl_number_format_state(NativeCallState {
            values: [new_target, options, undefined, undefined, undefined],
            count: 1,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.dispatch_intl_number_format_nested(
            NativeContinuation::intl_number_format(
                continuation_site,
                IntlNumberFormatStage::Locales,
                Value::from_heap_ref(state.raw()),
                locales,
            ),
            |isolate| isolate.begin_intl_get_canonical_locales(site),
        )
    }

    /// Canonicalizes requested locales before provider capability filtering.
    pub(crate) fn begin_intl_number_format_supported_locales_of(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let locales = self.call_argument(site, 0)?.unwrap_or(undefined);
        let options = self.call_argument(site, 1)?.unwrap_or(undefined);
        let state = self.allocate_intl_number_format_state(NativeCallState {
            values: [undefined, options, undefined, undefined, undefined],
            count: 0,
        })?;
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        self.dispatch_intl_number_format_nested(
            NativeContinuation::intl_number_format(
                continuation_site,
                IntlNumberFormatStage::Locales,
                Value::from_heap_ref(state.raw()),
                locales,
            ),
            |isolate| isolate.begin_intl_get_canonical_locales(site),
        )
    }

    /// Completes the locale stage for construction or supportedLocalesOf.
    pub(crate) fn resume_intl_number_format(
        &mut self,
        continuation: NativeContinuation,
        stage: IntlNumberFormatStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if stage != IntlNumberFormatStage::Locales {
            return Err(ExecutionError::MissingNativeContinuation);
        }
        let state = self.native_call_state_reference(continuation.first())?;
        self.update_native_call_state_value(state, NUMBER_FORMAT_LOCALES, value)?;
        if self.native_call_state_snapshot(state)?.count == 0 {
            self.finish_intl_number_format_supported_locales(continuation.site(), state)
        } else {
            self.finish_intl_number_format_construction(continuation.site(), state)
        }
    }

    /// Constructs the first default-decimal NumberFormat object through the provider ABI.
    fn finish_intl_number_format_construction(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let options = snapshot.values[NUMBER_FORMAT_OPTIONS];
        if options.as_immediate() != Some(Immediate::Undefined) {
            self.coerce_to_object(options)?;
        }
        let locales = self.intl_locale_strings(snapshot.values[NUMBER_FORMAT_LOCALES])?;
        let creation = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .create_number_format(IntlNumberFormatRequest {
                locales,
                options: IntlNumberFormatOptions::default(),
                ..Default::default()
            })
            .map_err(ExecutionError::IntlProvider)?;
        let prototype_atom = self.prototype_atom()?;
        let default_prototype = self
            .realm
            .intl_number_format_prototype
            .expect("Intl.NumberFormat prototype initializes before construction");
        let new_target = snapshot.values[NUMBER_FORMAT_NEW_TARGET];
        let prototype = self
            .constructor_prototype_value(new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .or_else(|| {
                self.realm_for_callable(new_target).ok().and_then(|realm| {
                    self.realm_intrinsic_prototype(realm, IntrinsicPrototypeKind::IntlNumberFormat)
                })
            })
            .unwrap_or(default_prototype);
        let number_format =
            self.allocate_intl_number_format_object(creation, prototype, AllocationSpace::Young)?;
        self.write(site.caller_base, site.destination, number_format)
    }

    /// Filters provider-supported locales and materializes a fresh intrinsic Array.
    fn finish_intl_number_format_supported_locales(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.native_call_state_snapshot(state)?;
        let options = snapshot.values[NUMBER_FORMAT_OPTIONS];
        if options.as_immediate() != Some(Immediate::Undefined) {
            self.coerce_to_object(options)?;
        }
        let locales = self.intl_locale_strings(snapshot.values[NUMBER_FORMAT_LOCALES])?;
        let supported = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .number_format_supported_locales(&locales, IntlLocaleMatcher::BestFit)
            .map_err(ExecutionError::IntlProvider)?;
        let result = self.create_array_object_with_prototype(
            self.realm
                .array_prototype
                .expect("Array prototype initializes before Intl.NumberFormat"),
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
            self.set_own_data_property(result, key, locale)?;
        }
        Ok(())
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
        let number_format = self.intl_number_format_reference(site.this_value)?;
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
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
        self.set_intl_number_format_string(result, b"style", b"decimal")?;
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
        self.set_intl_number_format_string(result, b"useGrouping", b"auto")?;
        self.set_intl_number_format_string(result, b"notation", b"standard")?;
        self.set_intl_number_format_string(result, b"signDisplay", b"auto")?;
        self.set_intl_number_format_number(
            result,
            b"roundingIncrement",
            u32::from(resolved.options.rounding_increment),
        )?;
        self.set_intl_number_format_string(result, b"roundingMode", b"halfExpand")?;
        self.set_intl_number_format_string(result, b"roundingPriority", b"auto")?;
        self.set_intl_number_format_string(result, b"trailingZeroDisplay", b"auto")
    }

    /// Drains a synchronous nested locale operation or leaves its typed continuation pending.
    fn dispatch_intl_number_format_nested(
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
        let NativeContinuationKind::IntlNumberFormat(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_intl_number_format(continuation, stage, value)
    }

    fn allocate_intl_number_format_state(
        &mut self,
        state: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = IntlNumberFormatStateRoots {
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

    fn intl_locale_strings(&mut self, value: Value) -> Result<Box<[Box<str>]>, ExecutionError> {
        let values = self.copy_packed_intl_array(value)?;
        let mut locales = Vec::new();
        locales
            .try_reserve_exact(values.len())
            .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
        for locale in values {
            locales.push(self.intl_ascii_string(locale)?);
        }
        Ok(locales.into_boxed_slice())
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
