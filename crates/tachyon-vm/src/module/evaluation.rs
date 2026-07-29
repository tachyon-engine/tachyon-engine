//! Persistent ECMAScript module evaluation state and dependency completion propagation.

use super::*;

impl ModuleGraph {
    pub(crate) fn evaluation_promise(
        &self,
        module: ModuleId,
    ) -> Result<Option<Value>, ModuleError> {
        Ok(self.record(self.cycle_root(module)?)?.evaluation_promise)
    }

    pub(crate) fn evaluation_outcome(
        &self,
        module: ModuleId,
    ) -> Result<Option<crate::RunOutcome>, ModuleError> {
        let root = self.cycle_root(module)?;
        Ok(match self.record(root)?.evaluation {
            ModuleEvaluationState::Evaluated(value) => Some(crate::RunOutcome::Completed(value)),
            ModuleEvaluationState::Errored(error) => Some(crate::RunOutcome::Thrown(error)),
            ModuleEvaluationState::Unevaluated
            | ModuleEvaluationState::Waiting
            | ModuleEvaluationState::Evaluating
            | ModuleEvaluationState::AsyncEvaluating(_) => None,
        })
    }

    /// Resolves a driver target to its cycle-root owner once, outside the poll hot path.
    pub(crate) fn evaluation_root_for_promise(&self, promise: Value) -> Option<ModuleId> {
        self.records
            .iter()
            .find(|record| record.evaluation_promise == Some(promise))
            .map(|record| record.id)
    }

    pub(crate) fn publish_evaluation_promise(
        &mut self,
        module: ModuleId,
        promise: Value,
    ) -> Result<(), ModuleError> {
        let root = self.cycle_root(module)?;
        let record = self.record_mut(root)?;
        if record.evaluation_promise.is_some() {
            return Err(ModuleError::InvalidLinkState);
        }
        record.evaluation_promise = Some(promise);
        Ok(())
    }

    /// Rolls back a capability published before evaluation state became observable.
    pub(crate) fn clear_evaluation_promise(
        &mut self,
        module: ModuleId,
        promise: Value,
    ) -> Result<(), ModuleError> {
        let root = self.cycle_root(module)?;
        let record = self.record_mut(root)?;
        if record.evaluation_promise != Some(promise)
            || record.evaluation != ModuleEvaluationState::Unevaluated
        {
            return Err(ModuleError::InvalidLinkState);
        }
        record.evaluation_promise = None;
        Ok(())
    }

    /// Reserves completion worklists before any module body can produce observable effects.
    pub(crate) fn prepare_evaluation(&mut self, module_count: usize) -> Result<(), ModuleError> {
        self.ready_async_modules
            .try_reserve(module_count)
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "ready async modules",
            })?;
        self.rejection_worklist
            .try_reserve(module_count)
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "module rejection worklist",
            })?;
        Ok(())
    }

    pub(crate) fn begin_async_evaluation(
        &mut self,
        module: ModuleId,
        state: Value,
    ) -> Result<(), ModuleError> {
        let needs_order = {
            let record = self.record(module)?;
            if record.evaluation != ModuleEvaluationState::Evaluating {
                return Err(ModuleError::InvalidLinkState);
            }
            record.async_evaluation_order.is_none()
        };
        let order = if needs_order {
            let order = self.next_async_evaluation_order;
            let next = order.checked_add(1).ok_or(ModuleError::CapacityOverflow {
                collection: "module async evaluation order",
            })?;
            Some((order, next))
        } else {
            None
        };
        let record = self.record_mut(module)?;
        record.evaluation = ModuleEvaluationState::AsyncEvaluating(state);
        if let Some((order, next)) = order {
            record.async_evaluation_order = Some(order);
            self.next_async_evaluation_order = next;
        }
        Ok(())
    }

    pub(crate) fn finish_async_evaluation(
        &mut self,
        module: ModuleId,
        result: Result<Value, Value>,
    ) -> Result<(), ExecutionError> {
        if !matches!(
            self.record(module)
                .map_err(ExecutionError::Module)?
                .evaluation,
            ModuleEvaluationState::AsyncEvaluating(_)
        ) {
            return Err(ExecutionError::Module(ModuleError::InvalidLinkState));
        }
        self.complete_evaluation(module, result)
            .map_err(ExecutionError::Module)
    }

    /// Completes one module and iteratively releases or rejects its registered ancestors.
    pub(crate) fn complete_evaluation(
        &mut self,
        module: ModuleId,
        result: Result<Value, Value>,
    ) -> Result<(), ModuleError> {
        match result {
            Ok(value) => {
                let parents = core::mem::take(&mut self.record_mut(module)?.async_parents);
                self.record_mut(module)?.evaluation = ModuleEvaluationState::Evaluated(value);
                for parent in parents {
                    let record = self.record_mut(parent)?;
                    if record.evaluation != ModuleEvaluationState::Waiting
                        || record.pending_async_dependencies == 0
                    {
                        continue;
                    }
                    record.pending_async_dependencies -= 1;
                    if record.pending_async_dependencies == 0 {
                        self.ready_async_modules.push(parent);
                    }
                }
            }
            Err(error) => self.reject_evaluation_tree(module, error)?,
        }
        Ok(())
    }

    /// Propagates one rejection without consuming the native stack.
    fn reject_evaluation_tree(
        &mut self,
        module: ModuleId,
        error: Value,
    ) -> Result<(), ModuleError> {
        self.rejection_worklist.clear();
        self.rejection_worklist.push(module);
        while let Some(current) = self.rejection_worklist.pop() {
            if matches!(
                self.record(current)?.evaluation,
                ModuleEvaluationState::Errored(_)
            ) {
                continue;
            }
            let parents = core::mem::take(&mut self.record_mut(current)?.async_parents);
            self.record_mut(current)?.evaluation = ModuleEvaluationState::Errored(error);
            for parent in parents {
                if !matches!(
                    self.record(parent)?.evaluation,
                    ModuleEvaluationState::Errored(_)
                ) && !self.rejection_worklist.contains(&parent)
                {
                    self.rejection_worklist.push(parent);
                }
            }
        }
        Ok(())
    }

    /// Removes the earliest ready ancestor while retaining queue allocation.
    pub(crate) fn take_ready_module(&mut self) -> Option<ModuleId> {
        let (index, _) =
            self.ready_async_modules
                .iter()
                .enumerate()
                .min_by_key(|(_, module)| {
                    self.records[module.index()]
                        .async_evaluation_order
                        .unwrap_or(u64::MAX)
                })?;
        Some(self.ready_async_modules.swap_remove(index))
    }

    /// Queues a dependency-ready module in deterministic discovery order.
    pub(super) fn queue_ready_module(&mut self, module: ModuleId) -> Result<(), ModuleError> {
        if self.record(module)?.evaluation != ModuleEvaluationState::Unevaluated {
            return Err(ModuleError::InvalidLinkState);
        }
        let order = self.next_async_evaluation_order;
        self.next_async_evaluation_order =
            order.checked_add(1).ok_or(ModuleError::CapacityOverflow {
                collection: "module async evaluation order",
            })?;
        let record = self.record_mut(module)?;
        record.async_evaluation_order = Some(order);
        self.ready_async_modules.push(module);
        Ok(())
    }

    pub(crate) fn reset_async_evaluation(&mut self, module: ModuleId) -> Result<(), ModuleError> {
        let record = self.record_mut(module)?;
        if !matches!(record.evaluation, ModuleEvaluationState::AsyncEvaluating(_)) {
            return Err(ModuleError::InvalidLinkState);
        }
        record.evaluation = ModuleEvaluationState::Unevaluated;
        Ok(())
    }
}
