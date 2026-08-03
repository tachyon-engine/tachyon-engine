//! Resumable non-blocking `%Atomics%` operations over integer TypedArray views.

use std::sync::Arc;

use super::super::*;
use super::array_buffer::BufferBacking;
use super::data_view::{data_view_decode, data_view_encode};
use super::typed_array::TypedArraySnapshot;
use crate::object::{ContentType, TypedArrayKind};
use crate::runtime::callable::{AtomicsFunction, DataViewElement};

const ATOMICS_RECEIVER: usize = 0;
const ATOMICS_INDEX: usize = 1;
const ATOMICS_VALUE: usize = 2;
const ATOMICS_REPLACEMENT: usize = 3;
const ATOMICS_INITIAL_LENGTH: usize = 4;
const ATOMICS_STATE_SLOTS: u8 = 5;

struct AtomicsRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

#[derive(Clone, Copy)]
struct PrimitiveAtomicsInput {
    receiver: Value,
    initial_length: usize,
    index: Value,
    value: Value,
    replacement: Value,
}

impl Trace for AtomicsRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl Isolate {
    /// Validates the integer view before any observable argument conversion.
    pub(crate) fn begin_atomics(
        &mut self,
        site: &CallSite,
        function: AtomicsFunction,
    ) -> Result<(), ExecutionError> {
        if function == AtomicsFunction::IsLockFree {
            return self.begin_atomics_is_lock_free(site);
        }
        if function == AtomicsFunction::Notify {
            return self.begin_atomics_notify(site);
        }
        if function == AtomicsFunction::Wait {
            return self.begin_atomics_wait(site);
        }
        if function == AtomicsFunction::Pause {
            return self.write(
                site.caller_base,
                site.destination,
                Value::from_immediate(Immediate::Undefined),
            );
        }
        let undefined = Value::from_immediate(Immediate::Undefined);
        let receiver = self.call_argument(site, 0)?.unwrap_or(undefined);
        let snapshot = self.validate_atomic_typed_array(receiver)?;
        let index = self.call_argument(site, 1)?.unwrap_or(undefined);
        let value = self.call_argument(site, 2)?.unwrap_or(undefined);
        let replacement = self.call_argument(site, 3)?.unwrap_or(undefined);
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if !self.is_object_value(index)
            && (function == AtomicsFunction::Load || !self.is_object_value(value))
            && (function != AtomicsFunction::CompareExchange || !self.is_object_value(replacement))
        {
            return self.finish_primitive_atomics(
                continuation_site,
                function,
                PrimitiveAtomicsInput {
                    receiver,
                    initial_length: snapshot.length,
                    index,
                    value,
                    replacement,
                },
            );
        }
        let state = self.allocate_atomics_state(NativeCallState {
            values: [
                receiver,
                index,
                value,
                replacement,
                Value::from_f64(snapshot.length as f64),
            ],
            count: ATOMICS_STATE_SLOTS,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.convert_atomics_value(
            continuation_site,
            state,
            ConversionConsumer::AtomicsIndex(function),
            index,
        )
    }

    /// Continues index, operand, and replacement conversion in specification order.
    pub(crate) fn resume_atomics_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match consumer {
            ConversionConsumer::AtomicsIndex(function) => {
                let index = self.ecma_to_index(value)?;
                let pending = self.native_call_state_snapshot(state)?;
                if index >= atomic_usize(pending.values[ATOMICS_INITIAL_LENGTH])? {
                    return Err(ExecutionError::InvalidArrayLength);
                }
                self.set_atomics_value(state, ATOMICS_INDEX, Value::from_f64(index as f64))?;
                if function == AtomicsFunction::Load {
                    return self.finish_atomics_state(site, state, function);
                }
                self.convert_atomics_value(
                    site,
                    state,
                    ConversionConsumer::AtomicsValue(function),
                    pending.values[ATOMICS_VALUE],
                )
            }
            ConversionConsumer::AtomicsValue(function) => {
                let pending = self.native_call_state_snapshot(state)?;
                let kind = self
                    .validate_atomic_typed_array(pending.values[ATOMICS_RECEIVER])?
                    .kind;
                let converted = self.convert_atomic_operand(kind, value)?;
                self.set_atomics_value(state, ATOMICS_VALUE, converted)?;
                if function != AtomicsFunction::CompareExchange {
                    return self.finish_atomics_state(site, state, function);
                }
                self.convert_atomics_value(
                    site,
                    state,
                    ConversionConsumer::AtomicsReplacement(function),
                    pending.values[ATOMICS_REPLACEMENT],
                )
            }
            ConversionConsumer::AtomicsReplacement(function) => {
                let pending = self.native_call_state_snapshot(state)?;
                let kind = self
                    .validate_atomic_typed_array(pending.values[ATOMICS_RECEIVER])?
                    .kind;
                let converted = self.convert_atomic_operand(kind, value)?;
                self.set_atomics_value(state, ATOMICS_REPLACEMENT, converted)?;
                self.finish_atomics_state(site, state, function)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Validates the waitable view before converting index and count in order.
    fn begin_atomics_notify(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let receiver = self.call_argument(site, 0)?.unwrap_or(undefined);
        let snapshot = self.validate_waitable_typed_array(receiver)?;
        let shared = matches!(
            self.resolve_buffer_backing(snapshot.buffer)?,
            BufferBacking::Shared(_)
        );
        let index = self.call_argument(site, 1)?.unwrap_or(undefined);
        let count = self.call_argument(site, 2)?.unwrap_or(undefined);
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if !self.is_object_value(index) {
            let index = self.ecma_to_index(index)?;
            if index >= snapshot.length {
                return Err(ExecutionError::InvalidArrayLength);
            }
            if !self.is_object_value(count) {
                let count = self.convert_atomics_notify_count(count)?;
                return self.finish_atomics_notify(
                    continuation_site,
                    receiver,
                    index,
                    count,
                    shared,
                );
            }
            let state = self.allocate_atomics_notify_state(
                site,
                receiver,
                Value::from_f64(index as f64),
                count,
                snapshot.length,
                shared,
            )?;
            return self.convert_atomics_value(
                continuation_site,
                state,
                ConversionConsumer::AtomicsNotifyCount,
                count,
            );
        }
        let state = self.allocate_atomics_notify_state(
            site,
            receiver,
            index,
            count,
            snapshot.length,
            shared,
        )?;
        self.convert_atomics_value(
            continuation_site,
            state,
            ConversionConsumer::AtomicsNotifyIndex,
            index,
        )
    }

    /// Continues observable notify index/count conversion without entering a backing lock.
    pub(crate) fn resume_atomics_notify_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        match consumer {
            ConversionConsumer::AtomicsNotifyIndex => {
                let index = self.ecma_to_index(value)?;
                let pending = self.native_call_state_snapshot(state)?;
                if index >= atomic_usize(pending.values[ATOMICS_INITIAL_LENGTH])? {
                    return Err(ExecutionError::InvalidArrayLength);
                }
                self.set_atomics_value(state, ATOMICS_INDEX, Value::from_f64(index as f64))?;
                let count = pending.values[ATOMICS_VALUE];
                if !self.is_object_value(count) {
                    let count = self.convert_atomics_notify_count(count)?;
                    return self.finish_atomics_notify_state(site, state, count);
                }
                self.convert_atomics_value(
                    site,
                    state,
                    ConversionConsumer::AtomicsNotifyCount,
                    count,
                )
            }
            ConversionConsumer::AtomicsNotifyCount => {
                let count = self.convert_atomics_notify_count(value)?;
                self.set_atomics_value(state, ATOMICS_VALUE, Value::from_f64(count))?;
                self.finish_atomics_notify_state(site, state, count)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Revalidates a notify location and returns zero until a waiter provider owns registrations.
    fn finish_atomics_notify_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        count: f64,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        self.finish_atomics_notify(
            site,
            pending.values[ATOMICS_RECEIVER],
            atomic_usize(pending.values[ATOMICS_INDEX])?,
            count,
            pending.values[ATOMICS_REPLACEMENT].as_immediate() == Some(Immediate::True),
        )
    }

    /// Returns zero for ordinary backing and validates retained shared backing for future waiters.
    fn finish_atomics_notify(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        index: usize,
        count: f64,
        shared: bool,
    ) -> Result<(), ExecutionError> {
        if shared {
            let snapshot = self.validate_waitable_typed_array(receiver)?;
            if index >= snapshot.length {
                return Err(ExecutionError::InvalidArrayLength);
            }
            let width = snapshot.kind.byte_width();
            let byte_offset = snapshot
                .byte_offset
                .checked_add(
                    index
                        .checked_mul(width)
                        .ok_or(ExecutionError::InvalidArrayLength)?,
                )
                .ok_or(ExecutionError::InvalidArrayLength)?;
            let BufferBacking::Shared(backing) = self.resolve_buffer_backing(snapshot.buffer)?
            else {
                return Err(ExecutionError::DetachedArrayBuffer);
            };
            let location = AtomicsWaitLocation::new(
                SharedMemoryId::from_address(Arc::as_ptr(&backing) as usize),
                byte_offset,
            );
            if let Some(provider) = self.host_providers.atomics_waiter_mut() {
                let count = atomics_notify_count(count);
                let notified = provider
                    .notify(location, count)
                    .map_err(ExecutionError::AtomicsWaiterProvider)?;
                return self.write(
                    site.caller_base,
                    site.destination,
                    Value::from_f64(notified as f64),
                );
            }
        }
        debug_assert!(count >= 0.0 || count == f64::INFINITY);
        self.write(site.caller_base, site.destination, Value::from_i32(0))
    }

    /// Publishes the callback-spanning notify state in the destination root slot.
    fn allocate_atomics_notify_state(
        &mut self,
        site: &CallSite,
        receiver: Value,
        index: Value,
        count: Value,
        initial_length: usize,
        shared: bool,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let state = self.allocate_atomics_state(NativeCallState {
            values: [
                receiver,
                index,
                count,
                boolean_value(shared),
                Value::from_f64(initial_length as f64),
            ],
            count: ATOMICS_STATE_SLOTS,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        Ok(state)
    }

    /// Implements notify's undefined-to-infinity and non-negative count normalization.
    fn convert_atomics_notify_count(&mut self, value: Value) -> Result<f64, ExecutionError> {
        if value.as_immediate() == Some(Immediate::Undefined) {
            return Ok(f64::INFINITY);
        }
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        Ok(atomic_integer_or_infinity(number).max(0.0))
    }

    /// Validates shared waitable storage before converting index, expected value, and timeout.
    fn begin_atomics_wait(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let undefined = Value::from_immediate(Immediate::Undefined);
        let receiver = self.call_argument(site, 0)?.unwrap_or(undefined);
        let snapshot = self.validate_waitable_typed_array(receiver)?;
        if !matches!(
            self.resolve_buffer_backing(snapshot.buffer)?,
            BufferBacking::Shared(_)
        ) {
            return Err(ExecutionError::AtomicsWaitRequiresSharedArrayBuffer);
        }
        let index = self.call_argument(site, 1)?.unwrap_or(undefined);
        let expected = self.call_argument(site, 2)?.unwrap_or(undefined);
        let timeout = self.call_argument(site, 3)?.unwrap_or(undefined);
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if !self.is_object_value(index)
            && !self.is_object_value(expected)
            && !self.is_object_value(timeout)
        {
            let index = self.ecma_to_index(index)?;
            if index >= snapshot.length {
                return Err(ExecutionError::InvalidArrayLength);
            }
            let expected = self.convert_atomics_wait_expected(snapshot.kind, expected)?;
            let timeout = self.convert_atomics_wait_timeout(timeout)?;
            return self.finish_atomics_wait(continuation_site, receiver, index, expected, timeout);
        }
        let state = self.allocate_atomics_state(NativeCallState {
            values: [
                receiver,
                index,
                expected,
                timeout,
                Value::from_f64(snapshot.length as f64),
            ],
            count: ATOMICS_STATE_SLOTS,
        })?;
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )?;
        self.convert_atomics_value(
            continuation_site,
            state,
            ConversionConsumer::AtomicsWaitIndex,
            index,
        )
    }

    /// Continues the three observable wait conversions without retaining a backing lock.
    pub(crate) fn resume_atomics_wait_conversion(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        match consumer {
            ConversionConsumer::AtomicsWaitIndex => {
                let index = self.ecma_to_index(value)?;
                if index >= atomic_usize(pending.values[ATOMICS_INITIAL_LENGTH])? {
                    return Err(ExecutionError::InvalidArrayLength);
                }
                self.set_atomics_value(state, ATOMICS_INDEX, Value::from_f64(index as f64))?;
                self.convert_atomics_value(
                    site,
                    state,
                    ConversionConsumer::AtomicsWaitExpected,
                    pending.values[ATOMICS_VALUE],
                )
            }
            ConversionConsumer::AtomicsWaitExpected => {
                let kind = self
                    .typed_array_snapshot(pending.values[ATOMICS_RECEIVER])?
                    .kind;
                let expected = self.convert_atomics_wait_expected(kind, value)?;
                self.set_atomics_value(state, ATOMICS_VALUE, expected)?;
                self.convert_atomics_value(
                    site,
                    state,
                    ConversionConsumer::AtomicsWaitTimeout,
                    pending.values[ATOMICS_REPLACEMENT],
                )
            }
            ConversionConsumer::AtomicsWaitTimeout => {
                let timeout = self.convert_atomics_wait_timeout(value)?;
                self.set_atomics_value(state, ATOMICS_REPLACEMENT, Value::from_f64(timeout))?;
                self.finish_atomics_wait_state(site, state)
            }
            _ => Err(ExecutionError::MissingNativeContinuation),
        }
    }

    /// Restores the fully converted wait inputs from their traced state owner.
    fn finish_atomics_wait_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        self.finish_atomics_wait(
            site,
            pending.values[ATOMICS_RECEIVER],
            atomic_usize(pending.values[ATOMICS_INDEX])?,
            pending.values[ATOMICS_VALUE],
            numeric_value(pending.values[ATOMICS_REPLACEMENT])
                .ok_or(ExecutionError::InvalidArrayLength)?,
        )
    }

    /// Compares and parks through the host provider, then materializes the normative result string.
    fn finish_atomics_wait(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        index: usize,
        expected: Value,
        timeout_ms: f64,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.validate_waitable_typed_array(receiver)?;
        if index >= snapshot.length {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let width = snapshot.kind.byte_width();
        let byte_offset = snapshot
            .byte_offset
            .checked_add(
                index
                    .checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let BufferBacking::Shared(backing) = self.resolve_buffer_backing(snapshot.buffer)? else {
            return Err(ExecutionError::AtomicsWaitRequiresSharedArrayBuffer);
        };
        let expected = self.atomics_wait_expected_bits(snapshot.kind, expected)?;
        let location = AtomicsWaitLocation::new(
            SharedMemoryId::from_address(Arc::as_ptr(&backing) as usize),
            byte_offset,
        );
        let timeout = atomics_wait_timeout(timeout_ms);
        if !self.host_providers.agent_can_suspend() {
            return Err(ExecutionError::AtomicsWaitCannotSuspend);
        }
        let mut condition = || {
            let locked = backing
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let end = byte_offset
                .checked_add(width)
                .ok_or(HostProviderError::Failure(1))?;
            let bytes = locked
                .bytes
                .get(byte_offset..end)
                .filter(|_| end <= locked.byte_length)
                .ok_or(HostProviderError::Failure(1))?;
            Ok(atomic_read_bits(bytes) == expected)
        };
        let result = if let Some(provider) = self.host_providers.atomics_waiter_mut() {
            provider
                .wait(location, timeout, &mut condition)
                .map_err(ExecutionError::AtomicsWaiterProvider)?
        } else if !condition().map_err(ExecutionError::AtomicsWaiterProvider)? {
            AtomicsWaitResult::NotEqual
        } else if timeout == Some(core::time::Duration::ZERO) {
            AtomicsWaitResult::TimedOut
        } else {
            return Err(ExecutionError::MissingAtomicsWaiterProvider);
        };
        let text = match result {
            AtomicsWaitResult::Ok => b"ok".as_slice(),
            AtomicsWaitResult::NotEqual => b"not-equal".as_slice(),
            AtomicsWaitResult::TimedOut => b"timed-out".as_slice(),
        };
        let string = JsString::try_from_latin1(text).map_err(ExecutionError::PropertyKeyString)?;
        let result = self.allocate_runtime_string(string)?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Converts the wait comparison operand without narrowing BigInt through a Number.
    fn convert_atomics_wait_expected(
        &mut self,
        kind: TypedArrayKind,
        value: Value,
    ) -> Result<Value, ExecutionError> {
        if kind == TypedArrayKind::BigInt64 {
            return self.primitive_to_bigint(value);
        }
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        Ok(Value::from_i32(atomics_to_int32(number)))
    }

    /// Applies wait's NaN/infinity and non-negative millisecond timeout normalization.
    fn convert_atomics_wait_timeout(&mut self, value: Value) -> Result<f64, ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        if number.is_nan() || number == f64::INFINITY {
            return Ok(f64::INFINITY);
        }
        Ok(number.max(0.0))
    }

    /// Encodes the converted expected operand into the exact waitable element representation.
    fn atomics_wait_expected_bits(
        &mut self,
        kind: TypedArrayKind,
        expected: Value,
    ) -> Result<u64, ExecutionError> {
        if kind == TypedArrayKind::BigInt64 {
            return self.bigint_modulo_u64(expected);
        }
        let value = expected
            .as_i32()
            .ok_or(ExecutionError::UnsupportedNumberConversion(expected))?;
        Ok(u64::from(u32::from_le_bytes(value.to_le_bytes())))
    }

    /// Implements the allocation-free path after validating before conversion.
    fn finish_primitive_atomics(
        &mut self,
        site: NativeContinuationSite,
        function: AtomicsFunction,
        input: PrimitiveAtomicsInput,
    ) -> Result<(), ExecutionError> {
        let index = self.ecma_to_index(input.index)?;
        if index >= input.initial_length {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let kind = self.validate_atomic_typed_array(input.receiver)?.kind;
        let value = if function == AtomicsFunction::Load {
            Value::from_immediate(Immediate::Undefined)
        } else {
            self.convert_atomic_operand(kind, input.value)?
        };
        let replacement = if function == AtomicsFunction::CompareExchange {
            self.convert_atomic_operand(kind, input.replacement)?
        } else {
            Value::from_immediate(Immediate::Undefined)
        };
        let result =
            self.execute_atomic_operation(function, input.receiver, index, value, replacement)?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Revalidates the view after callbacks and performs one indivisible backing operation.
    fn finish_atomics_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        function: AtomicsFunction,
    ) -> Result<(), ExecutionError> {
        let pending = self.native_call_state_snapshot(state)?;
        let receiver = pending.values[ATOMICS_RECEIVER];
        let index = atomic_usize(pending.values[ATOMICS_INDEX])?;
        let result = self.execute_atomic_operation(
            function,
            receiver,
            index,
            pending.values[ATOMICS_VALUE],
            pending.values[ATOMICS_REPLACEMENT],
        )?;
        self.write(site.caller_base, site.destination, result)
    }

    /// Executes load/store/RMW while a shared backing lock or ordinary no-GC borrow is held.
    fn execute_atomic_operation(
        &mut self,
        function: AtomicsFunction,
        receiver: Value,
        index: usize,
        value: Value,
        replacement: Value,
    ) -> Result<Value, ExecutionError> {
        let snapshot = self.validate_atomic_typed_array(receiver)?;
        if index >= snapshot.length {
            return Err(ExecutionError::InvalidArrayLength);
        }
        let width = snapshot.kind.byte_width();
        let start = snapshot
            .byte_offset
            .checked_add(
                index
                    .checked_mul(width)
                    .ok_or(ExecutionError::InvalidArrayLength)?,
            )
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let end = start
            .checked_add(width)
            .ok_or(ExecutionError::InvalidArrayLength)?;
        let backing = self.resolve_buffer_backing(snapshot.buffer)?;
        if function == AtomicsFunction::Load {
            let old = self.atomic_load_bits(&backing, start, end)?;
            return self.atomic_bits_to_value(snapshot.kind, old);
        }
        let operand = self.atomic_value_bits(snapshot.kind, value)?;
        let replacement = if function == AtomicsFunction::CompareExchange {
            self.atomic_value_bits(snapshot.kind, replacement)?
        } else {
            0
        };
        let old = self.atomic_modify_bits(&backing, start, end, function, operand, replacement)?;
        if function == AtomicsFunction::Store {
            return Ok(value);
        }
        self.atomic_bits_to_value(snapshot.kind, old)
    }

    /// Reads one complete element under the backing synchronization boundary.
    fn atomic_load_bits(
        &mut self,
        backing: &BufferBacking,
        start: usize,
        end: usize,
    ) -> Result<u64, ExecutionError> {
        self.with_buffer_backing_bytes(backing, |bytes, visible| {
            let source = bytes
                .get(start..end)
                .filter(|_| end <= visible)
                .ok_or(ExecutionError::InvalidArrayLength)?;
            Ok(atomic_read_bits(source))
        })
    }

    /// Performs read-modify-write as one callback so no lock spans allocation or JavaScript.
    fn atomic_modify_bits(
        &mut self,
        backing: &BufferBacking,
        start: usize,
        end: usize,
        function: AtomicsFunction,
        operand: u64,
        replacement: u64,
    ) -> Result<u64, ExecutionError> {
        self.with_buffer_backing_bytes_mut(backing, |bytes, visible| {
            let target = bytes
                .get_mut(start..end)
                .filter(|_| end <= visible)
                .ok_or(ExecutionError::InvalidArrayLength)?;
            let old = atomic_read_bits(target);
            let mask = atomic_width_mask(target.len());
            let next = match function {
                AtomicsFunction::Add => old.wrapping_add(operand),
                AtomicsFunction::And => old & operand,
                AtomicsFunction::CompareExchange => {
                    if old == operand & mask {
                        replacement
                    } else {
                        old
                    }
                }
                AtomicsFunction::Exchange | AtomicsFunction::Store => operand,
                AtomicsFunction::Notify => {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                AtomicsFunction::Or => old | operand,
                AtomicsFunction::Pause => {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
                AtomicsFunction::Sub => old.wrapping_sub(operand),
                AtomicsFunction::Xor => old ^ operand,
                AtomicsFunction::IsLockFree | AtomicsFunction::Load | AtomicsFunction::Wait => {
                    return Err(ExecutionError::MissingNativeContinuation);
                }
            } & mask;
            target.copy_from_slice(&next.to_le_bytes()[..target.len()]);
            Ok(old)
        })
    }

    /// Converts one already-primitive operand according to the view content type.
    fn convert_atomic_operand(
        &mut self,
        kind: TypedArrayKind,
        value: Value,
    ) -> Result<Value, ExecutionError> {
        match kind.content_type() {
            ContentType::BigInt => self.primitive_to_bigint(value),
            ContentType::Number => {
                let number = numeric_value(self.convert_to_number(value)?)
                    .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
                Ok(Value::from_f64(atomic_integer_or_infinity(number)))
            }
        }
    }

    /// Rejects floating/clamped views while preserving ValidateTypedArray error ordering.
    fn validate_atomic_typed_array(
        &mut self,
        receiver: Value,
    ) -> Result<TypedArraySnapshot, ExecutionError> {
        let snapshot = self.validated_typed_array_snapshot(receiver)?;
        if matches!(
            snapshot.kind,
            TypedArrayKind::Uint8Clamped | TypedArrayKind::Float32 | TypedArrayKind::Float64
        ) {
            return Err(ExecutionError::TypedArrayContentTypeMismatch);
        }
        Ok(snapshot)
    }

    /// Restricts waiter-list operations to Int32Array and BigInt64Array views.
    fn validate_waitable_typed_array(
        &mut self,
        receiver: Value,
    ) -> Result<TypedArraySnapshot, ExecutionError> {
        let snapshot = self.validated_typed_array_snapshot(receiver)?;
        if !matches!(
            snapshot.kind,
            TypedArrayKind::Int32 | TypedArrayKind::BigInt64
        ) {
            return Err(ExecutionError::TypedArrayContentTypeMismatch);
        }
        Ok(snapshot)
    }

    /// Encodes the modulo element representation without retaining a backing borrow.
    fn atomic_value_bits(
        &mut self,
        kind: TypedArrayKind,
        value: Value,
    ) -> Result<u64, ExecutionError> {
        match kind.content_type() {
            ContentType::BigInt => self.bigint_modulo_u64(value),
            ContentType::Number => {
                let number = numeric_value(value)
                    .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
                let bytes = data_view_encode(atomic_data_view_kind(kind)?, number, true);
                Ok(u64::from_le_bytes(bytes))
            }
        }
    }

    /// Decodes the previous element only after releasing the shared backing lock.
    fn atomic_bits_to_value(
        &mut self,
        kind: TypedArrayKind,
        bits: u64,
    ) -> Result<Value, ExecutionError> {
        if kind.content_type() == ContentType::BigInt {
            return self.allocate_bigint_bits(bits, kind == TypedArrayKind::BigInt64);
        }
        Ok(data_view_decode(
            atomic_data_view_kind(kind)?,
            bits.to_le_bytes(),
            true,
        ))
    }

    /// Starts the standalone ToIntegerOrInfinity conversion used by isLockFree.
    fn begin_atomics_is_lock_free(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let value = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let continuation_site = NativeContinuationSite {
            caller_base: site.caller_base,
            destination: site.destination,
            call_site: site.call_site,
        };
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::AtomicsIsLockFree,
                site.caller_base,
                site.destination,
                value,
                value,
                site.call_site,
            );
        }
        self.resume_atomics_is_lock_free(continuation_site, value)
    }

    /// Completes isLockFree after any object-to-primitive callback.
    pub(crate) fn resume_atomics_is_lock_free(
        &mut self,
        site: NativeContinuationSite,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let number = numeric_value(self.convert_to_number(value)?)
            .ok_or(ExecutionError::UnsupportedNumberConversion(value))?;
        let integer = atomic_integer_or_infinity(number);
        let result =
            boolean_value(integer == 1.0 || integer == 2.0 || integer == 4.0 || integer == 8.0);
        self.write(site.caller_base, site.destination, result)
    }

    /// Dispatches object ToPrimitive while retaining the fixed five-slot operation state.
    fn convert_atomics_value(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<NativeCallState>,
        consumer: ConversionConsumer,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                consumer,
                site.caller_base,
                site.destination,
                Value::from_heap_ref(state.raw()),
                value,
                site.call_site,
            );
        }
        if matches!(
            consumer,
            ConversionConsumer::AtomicsNotifyIndex | ConversionConsumer::AtomicsNotifyCount
        ) {
            return self.resume_atomics_notify_conversion(site, state, consumer, value);
        }
        if matches!(
            consumer,
            ConversionConsumer::AtomicsWaitIndex
                | ConversionConsumer::AtomicsWaitExpected
                | ConversionConsumer::AtomicsWaitTimeout
        ) {
            return self.resume_atomics_wait_conversion(site, state, consumer, value);
        }
        self.resume_atomics_conversion(site, state, consumer, value)
    }

    /// Publishes a converted state value before the next callback or allocation boundary.
    fn set_atomics_value(
        &mut self,
        state: GcRef<NativeCallState>,
        slot: usize,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow_mut(state, self.types.native_call_state)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .values[slot] = value;
                Ok(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    /// Allocates exact fixed-capacity state under every isolate-owned root category.
    fn allocate_atomics_state(
        &mut self,
        pending: NativeCallState,
    ) -> Result<GcRef<NativeCallState>, ExecutionError> {
        let mut roots = AtomicsRoots {
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

#[inline(always)]
fn atomic_integer_or_infinity(number: f64) -> f64 {
    if number.is_nan() || number == 0.0 {
        0.0
    } else if number.is_infinite() {
        number
    } else {
        number.trunc()
    }
}

#[inline(always)]
fn atomics_notify_count(count: f64) -> u64 {
    if count == f64::INFINITY || count >= u64::MAX as f64 {
        u64::MAX
    } else {
        count as u64
    }
}

#[inline(always)]
fn atomics_wait_timeout(timeout_ms: f64) -> Option<core::time::Duration> {
    if timeout_ms == f64::INFINITY {
        return None;
    }
    let nanos = timeout_ms * 1_000_000.0;
    Some(core::time::Duration::from_nanos(
        nanos.min(u64::MAX as f64) as u64
    ))
}

#[inline(always)]
fn atomics_to_int32(number: f64) -> i32 {
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    let value = number.trunc().rem_euclid(4_294_967_296.0);
    if value >= 2_147_483_648.0 {
        (value - 4_294_967_296.0) as i32
    } else {
        value as i32
    }
}

#[inline(always)]
fn atomic_usize(value: Value) -> Result<usize, ExecutionError> {
    let number = numeric_value(value).ok_or(ExecutionError::InvalidArrayLength)?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > usize::MAX as f64 {
        return Err(ExecutionError::InvalidArrayLength);
    }
    Ok(number as usize)
}

#[inline(always)]
fn atomic_read_bits(bytes: &[u8]) -> u64 {
    let mut word = [0_u8; 8];
    word[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(word)
}

#[inline(always)]
fn atomic_width_mask(width: usize) -> u64 {
    if width == 8 {
        u64::MAX
    } else {
        (1_u64 << (width * 8)) - 1
    }
}

#[inline(always)]
fn atomic_data_view_kind(kind: TypedArrayKind) -> Result<DataViewElement, ExecutionError> {
    Ok(match kind {
        TypedArrayKind::Int8 => DataViewElement::Int8,
        TypedArrayKind::Uint8 => DataViewElement::Uint8,
        TypedArrayKind::Int16 => DataViewElement::Int16,
        TypedArrayKind::Uint16 => DataViewElement::Uint16,
        TypedArrayKind::Int32 => DataViewElement::Int32,
        TypedArrayKind::Uint32 => DataViewElement::Uint32,
        TypedArrayKind::BigInt64 | TypedArrayKind::BigUint64 => {
            return Err(ExecutionError::TypedArrayContentTypeMismatch);
        }
        TypedArrayKind::Uint8Clamped | TypedArrayKind::Float32 | TypedArrayKind::Float64 => {
            return Err(ExecutionError::TypedArrayContentTypeMismatch);
        }
    })
}
