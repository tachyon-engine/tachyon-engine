//! Resumable `Math.sumPrecise` acquisition and fixed-layout exact binary64 summation.

use super::*;

const EXACT_SUM_LIMBS: usize = 34;

/// Spec state folded into the exact accumulator, following QuickJS' fixed-limb design.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum ExactSumState {
    MinusZero,
    Finite,
    PlusInfinity,
    MinusInfinity,
    NaN,
}

/// Signed two's-complement sum of every finite binary64 significand at its exact exponent.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct ExactSumAccumulator {
    limbs: [u64; EXACT_SUM_LIMBS],
    limb_count: u8,
    state: ExactSumState,
}

impl Trace for ExactSumAccumulator {
    #[inline(always)]
    fn trace(&mut self, _tracer: &mut dyn Tracer) {}
}

impl ExactSumAccumulator {
    #[inline]
    const fn new() -> Self {
        Self {
            limbs: [0; EXACT_SUM_LIMBS],
            limb_count: 1,
            state: ExactSumState::MinusZero,
        }
    }

    /// Adds one Number exactly, including the proposal's signed-zero and infinity state machine.
    fn add(&mut self, number: f64) {
        let bits = number.to_bits();
        let negative = bits >> 63 != 0;
        let exponent = ((bits >> 52) & 0x7ff) as usize;
        let mut significand = bits & ((1_u64 << 52) - 1);
        if exponent == 0x7ff {
            self.add_non_finite(negative, significand);
            return;
        }
        if exponent == 0 {
            if significand == 0 {
                if self.state == ExactSumState::MinusZero && !negative {
                    self.state = ExactSumState::Finite;
                }
                return;
            }
            self.add_significand(negative, significand, 0, 0);
            return;
        }
        significand |= 1_u64 << 52;
        let shift = exponent - 1;
        self.add_significand(negative, significand, shift / 64, shift % 64);
    }

    #[inline]
    fn add_non_finite(&mut self, negative: bool, payload: u64) {
        if payload != 0
            || self.state == ExactSumState::NaN
            || (self.state == ExactSumState::MinusInfinity && !negative)
            || (self.state == ExactSumState::PlusInfinity && negative)
        {
            self.state = ExactSumState::NaN;
        } else {
            self.state = if negative {
                ExactSumState::MinusInfinity
            } else {
                ExactSumState::PlusInfinity
            };
        }
    }

    /// Extends and adds one signed significand without heap storage or overflow checks per term.
    fn add_significand(
        &mut self,
        negative: bool,
        significand: u64,
        mut position: usize,
        shift: usize,
    ) {
        if matches!(
            self.state,
            ExactSumState::PlusInfinity | ExactSumState::MinusInfinity | ExactSumState::NaN
        ) {
            return;
        }
        self.state = ExactSumState::Finite;
        let mut count = usize::from(self.limb_count);
        let accumulator_sign = self.limbs[count - 1] >> 63;
        if position >= count {
            for limb in &mut self.limbs[count..=position] {
                *limb = 0_u64.wrapping_sub(accumulator_sign);
            }
        }
        let operand_sign = 0_u64.wrapping_sub(u64::from(negative));
        let mut carry = u64::from(negative);
        (self.limbs[position], carry) = add_with_carry(
            self.limbs[position],
            (significand << shift) ^ operand_sign,
            carry,
        );
        if shift >= 12 {
            position += 1;
            if position >= count {
                self.limbs[position] = 0_u64.wrapping_sub(accumulator_sign);
            }
            (self.limbs[position], carry) = add_with_carry(
                self.limbs[position],
                (significand >> (64 - shift)) ^ operand_sign,
                carry,
            );
        }
        position += 1;
        if position >= count {
            count = position;
        } else {
            for limb in &mut self.limbs[position..count] {
                if carry == u64::from(negative) {
                    self.limb_count = count as u8;
                    return;
                }
                (*limb, carry) = add_with_carry(*limb, operand_sign, carry);
            }
        }
        let extension = carry
            .wrapping_add(0_u64.wrapping_sub(accumulator_sign))
            .wrapping_add(operand_sign);
        let current_sign = self.limbs[count - 1] >> 63;
        if extension != 0_u64.wrapping_sub(current_sign) {
            debug_assert!(count < EXACT_SUM_LIMBS);
            self.limbs[count] = extension;
            count += 1;
        }
        self.limb_count = count as u8;
    }

    /// Rounds the exact signed integer to binary64 using round-to-nearest, ties-to-even.
    fn result(self) -> f64 {
        match self.state {
            ExactSumState::MinusZero => return -0.0,
            ExactSumState::PlusInfinity => return f64::INFINITY,
            ExactSumState::MinusInfinity => return f64::NEG_INFINITY,
            ExactSumState::NaN => return f64::NAN,
            ExactSumState::Finite => {}
        }
        let mut limbs = self.limbs;
        let mut count = usize::from(self.limb_count);
        let negative = limbs[count - 1] >> 63 != 0;
        if negative {
            let mut carry = 1;
            for limb in &mut limbs[..count] {
                (*limb, carry) = add_with_carry(!*limb, 0, carry);
            }
        }
        while count > 0 && limbs[count - 1] == 0 {
            count -= 1;
        }
        if count == 0 {
            return 0.0;
        }
        if count == 1 && limbs[0] < (1_u64 << 52) {
            return f64::from_bits((u64::from(negative) << 63) | limbs[0]);
        }
        round_exact_limbs(&limbs, count, negative)
    }
}

impl Isolate {
    /// Starts ordinary GetIterator(items, sync) before handing its record to the eager driver.
    pub(crate) fn begin_math_sum_precise(&mut self, site: &CallSite) -> Result<(), ExecutionError> {
        let items = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if is_nullish(items) {
            return Err(ExecutionError::NotObject(items));
        }
        let symbol = self
            .agent
            .well_known_symbols
            .iterator
            .expect("Symbol.iterator initializes before Math.sumPrecise");
        let key = self.property_key(symbol)?;
        self.dispatch_math_sum_precise_get(Self::native_site(site), items, key)
    }

    /// Resumes either observable GetIterator boundary and then starts exact eager summation.
    pub(crate) fn resume_math_sum_precise(
        &mut self,
        continuation: NativeContinuation,
        stage: MathSumPreciseStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        match stage {
            MathSumPreciseStage::IteratorMethodGet => {
                self.resolve_function_object(value)?;
                self.dispatch_property_callback(
                    NativeContinuation::math_sum_precise(
                        site,
                        MathSumPreciseStage::IteratorMethodCall,
                        continuation.first(),
                        value,
                    ),
                    value,
                )
                .map(|_| ())
            }
            MathSumPreciseStage::IteratorMethodCall => {
                if !self.is_object_value(value) {
                    return Err(ExecutionError::NotObject(value));
                }
                self.write(site.caller_base, site.destination, value)?;
                let iterator = self.read(site.caller_base, site.destination)?;
                self.begin_iterator_eager_sum_precise(site, iterator)
            }
        }
    }

    /// Performs Proxy/accessor-aware @@iterator lookup below a typed Math parent.
    fn dispatch_math_sum_precise_get(
        &mut self,
        site: NativeContinuationSite,
        items: Value,
        key: PropertyKey,
    ) -> Result<(), ExecutionError> {
        let kind = NativeContinuationKind::MathSumPrecise(MathSumPreciseStage::IteratorMethodGet);
        self.fiber
            .completions
            .push_native(NativeContinuation::math_sum_precise(
                site,
                MathSumPreciseStage::IteratorMethodGet,
                items,
                Value::from_immediate(Immediate::Undefined),
            ))
            .map_err(Self::completion_stack_error)?;
        if let Err(error) = self.dispatch_proxy_aware_property_read(site, items, items, key) {
            if self.fiber.completions.last_native_matches(kind, site) {
                self.pop_native_continuation()?;
            }
            return Err(error);
        }
        if !self.fiber.completions.last_native_matches(kind, site) {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(site.caller_base, site.destination)?;
        self.resume_math_sum_precise(continuation, MathSumPreciseStage::IteratorMethodGet, value)
    }

    /// Allocates one no-edge fixed accumulator while all VM-owned roots remain published.
    pub(super) fn allocate_exact_sum_accumulator(
        &mut self,
    ) -> Result<GcRef<ExactSumAccumulator>, ExecutionError> {
        let mut roots = VmRoots {
            fiber: &mut self.fiber,
            suspended_fibers: &mut self.suspended_fibers,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            inactive_realms: &mut self.inactive_realms,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.exact_sum_accumulator,
                0,
                0,
                ExactSumAccumulator::new(),
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(super) fn exact_sum_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<ExactSumAccumulator>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.exact_sum_accumulator)
            .map_err(ExecutionError::HeapReference)
    }

    /// Adds without allocation while the fixed payload is held under a no-GC borrow.
    pub(super) fn add_exact_sum(
        &mut self,
        accumulator: GcRef<ExactSumAccumulator>,
        number: f64,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let accumulator = scope.root(accumulator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let accumulator = no_gc
                    .borrow_mut(accumulator, self.types.exact_sum_accumulator)
                    .map_err(ExecutionError::NoGcBorrow)?;
                accumulator.add(number);
                Ok::<_, ExecutionError>(())
            })
        })
    }

    pub(super) fn exact_sum_result(
        &mut self,
        accumulator: GcRef<ExactSumAccumulator>,
    ) -> Result<f64, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let accumulator = scope.root(accumulator).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(accumulator, self.types.exact_sum_accumulator)
                    .copied()
                    .map(ExactSumAccumulator::result)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }
}

#[inline(always)]
fn add_with_carry(left: u64, right: u64, carry: u64) -> (u64, u64) {
    let (partial, first_overflow) = left.overflowing_add(right);
    let (result, second_overflow) = partial.overflowing_add(carry);
    (result, u64::from(first_overflow | second_overflow))
}

/// Extracts guard/sticky bits from normalized exact limbs and encodes the rounded binary64.
fn round_exact_limbs(limbs: &[u64; EXACT_SUM_LIMBS], count: usize, negative: bool) -> f64 {
    let mut exponent = count * 64;
    let mut position = count - 1;
    let mut mantissa = limbs[position];
    let shift = mantissa.leading_zeros() as usize;
    exponent = exponent - shift - 52;
    if shift != 0 {
        mantissa <<= shift;
        if position > 0 {
            position -= 1;
            let low_width = 64 - shift;
            let mask = (1_u64 << low_width) - 1;
            let sticky = limbs[position] & mask != 0;
            mantissa |= (limbs[position] >> low_width) | u64::from(sticky);
        }
    }
    if mantissa & ((1_u64 << 10) - 1) == 0 {
        while position > 0 {
            position -= 1;
            if limbs[position] != 0 {
                mantissa |= 1;
                break;
            }
        }
    }
    let addend = (1_u64 << 10) - 1 + ((mantissa >> 11) & 1);
    mantissa = mantissa.wrapping_add(addend) >> 11;
    if mantissa == 0 {
        exponent += 1;
    }
    let sign = u64::from(negative) << 63;
    if exponent >= 0x7ff {
        return f64::from_bits(sign | (0x7ff_u64 << 52));
    }
    let fraction = mantissa & ((1_u64 << 52) - 1);
    f64::from_bits(sign | ((exponent as u64) << 52) | fraction)
}

#[cfg(test)]
mod tests {
    use super::ExactSumAccumulator;

    #[test]
    fn exact_accumulator_preserves_cancellation_rounding_and_special_states() {
        let mut sum = ExactSumAccumulator::new();
        for number in [1e100, 1.0, -1e100] {
            sum.add(number);
        }
        assert_eq!(sum.result(), 1.0);

        let mut zeros = ExactSumAccumulator::new();
        zeros.add(-0.0);
        assert!(zeros.result().is_sign_negative());
        zeros.add(0.0);
        assert!(zeros.result().is_sign_positive());

        let mut infinities = ExactSumAccumulator::new();
        infinities.add(f64::INFINITY);
        infinities.add(f64::NEG_INFINITY);
        assert!(infinities.result().is_nan());
    }
}
