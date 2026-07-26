use super::super::super::*;

impl Isolate {
    /// Applies one indexed fulfillment or rejection under the selected result policy.
    pub(crate) fn call_promise_all_handler(
        &mut self,
        site: &CallSite,
        element: GcRef<PromiseCombinatorElement>,
        rejected: bool,
    ) -> Result<(), ExecutionError> {
        let Some((state, index)) = self.take_promise_combinator_element(element)? else {
            return self.write_undefined(site);
        };
        let argument = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let pending = self.promise_combinator_snapshot(state)?;
        if pending.settled {
            return self.write_undefined(site);
        }
        if pending.kind == PromiseCombinatorKind::Any {
            let continuation_site = NativeContinuationSite {
                caller_base: site.caller_base,
                destination: site.destination,
                call_site: site.call_site,
            };
            if !rejected {
                return self.finish_promise_combinator_fulfill(
                    continuation_site,
                    state,
                    argument,
                    false,
                );
            }
            let key = self.property_key_atom(safe_integer_value(index))?;
            self.set_own_data_property(pending.values, key, argument)?;
            let remaining = self.decrement_promise_combinator_remaining(state)?;
            if remaining == 0 {
                let (state, error) =
                    self.create_promise_any_aggregate_error(continuation_site, state)?;
                return self.finish_promise_combinator_reject(
                    continuation_site,
                    state,
                    error,
                    false,
                );
            }
            return self.write_undefined(site);
        }
        if pending.kind == PromiseCombinatorKind::AllSettled {
            let (state, values) =
                self.create_promise_all_settled_result(site, state, index, argument, rejected)?;
            let remaining = self.decrement_promise_combinator_remaining(state)?;
            if remaining == 0 {
                return self.finish_promise_combinator_fulfill(
                    NativeContinuationSite {
                        caller_base: site.caller_base,
                        destination: site.destination,
                        call_site: site.call_site,
                    },
                    state,
                    values,
                    false,
                );
            }
            return self.write_undefined(site);
        }
        if rejected {
            self.set_promise_combinator_settled(state)?;
            self.settle_promise(pending.promise, PromiseState::Rejected, argument)?;
            return self.write_undefined(site);
        }
        let key = self.property_key_atom(safe_integer_value(index))?;
        self.set_own_data_property(pending.values, key, argument)?;
        let remaining = self.decrement_promise_combinator_remaining(state)?;
        if remaining == 0 {
            return self.finish_promise_combinator_fulfill(
                NativeContinuationSite {
                    caller_base: site.caller_base,
                    destination: site.destination,
                    call_site: site.call_site,
                },
                state,
                pending.values,
                false,
            );
        }
        self.write_undefined(site)
    }

    /// Settles the aggregate rejection and restores the public result into the caller register.
    pub(super) fn reject_promise_combinator(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingPromiseCombinator>,
        reason: Value,
    ) -> Result<(), ExecutionError> {
        self.finish_promise_combinator_reject(site, state, reason, true)
    }

    /// Calls generic reject or settles the intrinsic aggregate with caller-selected return mode.
    pub(super) fn finish_promise_combinator_reject(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingPromiseCombinator>,
        reason: Value,
        return_promise: bool,
    ) -> Result<(), ExecutionError> {
        let pending = self.promise_combinator_snapshot(state)?;
        if !pending.settled {
            self.set_promise_combinator_settled(state)?;
            if pending.capability.as_immediate() != Some(Immediate::Undefined) {
                self.update_promise_combinator(state, |pending| {
                    pending.return_promise_after_capability_call = return_promise
                })?;
                return self.call_promise_combinator(
                    site,
                    state,
                    PromiseCombinatorStage::CapabilityRejectCall,
                    pending.capability_reject,
                    Value::from_immediate(Immediate::Undefined),
                    &[reason],
                );
            }
            self.settle_promise(pending.promise, PromiseState::Rejected, reason)?;
        }
        self.write(
            site.caller_base,
            site.destination,
            if return_promise {
                pending.promise
            } else {
                Value::from_immediate(Immediate::Undefined)
            },
        )
    }

    /// Calls a generic capability resolve or settles the intrinsic aggregate directly.
    pub(super) fn finish_promise_combinator_fulfill(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingPromiseCombinator>,
        values: Value,
        return_promise: bool,
    ) -> Result<(), ExecutionError> {
        let pending = self.promise_combinator_snapshot(state)?;
        if pending.settled {
            return if return_promise {
                self.write(site.caller_base, site.destination, pending.promise)
            } else {
                self.write(
                    site.caller_base,
                    site.destination,
                    Value::from_immediate(Immediate::Undefined),
                )
            };
        }
        self.set_promise_combinator_settled(state)?;
        if pending.capability.as_immediate() != Some(Immediate::Undefined) {
            self.update_promise_combinator(state, |pending| {
                pending.return_promise_after_capability_call = return_promise
            })?;
            return self.call_promise_combinator(
                site,
                state,
                PromiseCombinatorStage::CapabilityResolveCall,
                pending.capability_resolve,
                Value::from_immediate(Immediate::Undefined),
                &[values],
            );
        }
        self.begin_promise_resolution(
            pending.promise,
            values,
            site,
            if return_promise {
                PromiseResolutionMode::StaticResolve
            } else {
                PromiseResolutionMode::ResolverCall
            },
        )
    }
}
