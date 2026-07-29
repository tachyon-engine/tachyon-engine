//! Bounded startup transaction for an already-linked module evaluation graph.

use super::*;

#[derive(Debug)]
struct ModuleStartFrame {
    module: ModuleId,
    next_request: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModuleStartPhase {
    Traverse,
    Instantiate,
    Register,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModuleRegistrationStage {
    Scan,
    ReserveParents,
    PublishParents,
    Finish,
}

/// Scheduler-owned cursors for bounded startup; ECMAScript state remains in module records.
#[derive(Debug)]
pub(crate) struct ModuleStartState {
    pub(crate) root: ModuleId,
    epoch: u32,
    frames: Vec<ModuleStartFrame>,
    order: Vec<ModuleId>,
    cursor: usize,
    request_cursor: usize,
    pending_dependencies: Vec<ModuleId>,
    registration_stage: ModuleRegistrationStage,
    dependency_cursor: usize,
    pub(crate) phase: ModuleStartPhase,
}

impl ModuleGraph {
    const TRAVERSAL_ACTIVE: u32 = 1;
    const TRAVERSAL_DONE: u32 = 2;

    #[inline(always)]
    pub(crate) const fn evaluation_start_pending(&self) -> bool {
        self.start_state.is_some()
    }

    #[cfg(test)]
    pub(crate) fn evaluation_start_snapshot(
        &self,
    ) -> Option<(ModuleStartPhase, usize, usize, usize, usize)> {
        self.start_state.as_ref().map(|state| {
            (
                state.phase,
                state.frames.len(),
                state.order.len(),
                state.cursor,
                state.request_cursor,
            )
        })
    }

    /// Seeds an iterative postorder walk without duplicating linked SCC semantics.
    pub(crate) fn begin_evaluation_start(&mut self, root: ModuleId) -> Result<(), ModuleError> {
        if self.start_state.is_some() {
            return Err(ModuleError::InvalidLinkState);
        }
        let root = self.cycle_root(root)?;
        let epoch = self.next_start_epoch;
        self.next_start_epoch = epoch.checked_add(4).ok_or(ModuleError::CapacityOverflow {
            collection: "module evaluation traversal epoch",
        })?;
        let initial = INITIAL_LINK_WORK_CAPACITY.min(self.limits.max_modules as usize);
        let mut frames = Vec::new();
        let mut order = Vec::new();
        frames
            .try_reserve_exact(initial)
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "module evaluation frames",
            })?;
        order
            .try_reserve_exact(initial)
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "module evaluation order",
            })?;
        frames.push(ModuleStartFrame {
            module: root,
            next_request: 0,
        });
        self.record_mut(root)?.evaluation_traversal_mark = epoch | Self::TRAVERSAL_ACTIVE;
        self.start_state = Some(ModuleStartState {
            root,
            epoch,
            frames,
            order,
            cursor: 0,
            request_cursor: 0,
            pending_dependencies: Vec::with_capacity(INITIAL_ASYNC_PARENT_CAPACITY),
            registration_stage: ModuleRegistrationStage::Scan,
            dependency_cursor: 0,
            phase: ModuleStartPhase::Traverse,
        });
        Ok(())
    }

    /// Advances exactly one requested edge or one postorder publication.
    pub(crate) fn advance_evaluation_traversal(&mut self) -> Result<bool, ModuleError> {
        let mut state = self
            .start_state
            .take()
            .ok_or(ModuleError::InvalidLinkState)?;
        if state.phase != ModuleStartPhase::Traverse {
            self.start_state = Some(state);
            return Err(ModuleError::InvalidLinkState);
        }
        let result = (|| {
            let Some(frame) = state.frames.last_mut() else {
                self.prepare_evaluation(state.order.len())?;
                state.phase = ModuleStartPhase::Instantiate;
                return Ok(true);
            };
            let module = frame.module;
            let request_index = frame.next_request;
            if request_index < self.record(module)?.requested_modules.len() {
                frame.next_request += 1;
                let request = self.record(module)?.requested_modules[request_index].clone();
                let child = self
                    .find_specifier(&request)
                    .ok_or(ModuleError::MissingModule)?;
                if self.record(child)?.evaluation_traversal_mark & !3 != state.epoch {
                    if state.frames.len() >= self.limits.max_modules as usize {
                        return Err(ModuleError::EvaluationOrderLimit {
                            limit: self.limits.max_modules,
                        });
                    }
                    state.frames.try_reserve_exact(1).map_err(|_| {
                        ModuleError::AllocationFailed {
                            collection: "module evaluation frames",
                        }
                    })?;
                    self.record_mut(child)?.evaluation_traversal_mark =
                        state.epoch | Self::TRAVERSAL_ACTIVE;
                    state.frames.push(ModuleStartFrame {
                        module: child,
                        next_request: 0,
                    });
                }
                return Ok(false);
            }
            state
                .order
                .try_reserve_exact(1)
                .map_err(|_| ModuleError::AllocationFailed {
                    collection: "module evaluation order",
                })?;
            state.frames.pop();
            self.record_mut(module)?.evaluation_traversal_mark = state.epoch | Self::TRAVERSAL_DONE;
            state.order.push(module);
            Ok(false)
        })();
        self.start_state = Some(state);
        result
    }

    pub(crate) fn evaluation_start_module(&self) -> Result<Option<ModuleId>, ModuleError> {
        let state = self
            .start_state
            .as_ref()
            .ok_or(ModuleError::InvalidLinkState)?;
        Ok(state.order.get(state.cursor).copied())
    }

    pub(crate) fn evaluation_start_phase(&self) -> Result<ModuleStartPhase, ModuleError> {
        self.start_state
            .as_ref()
            .map(|state| state.phase)
            .ok_or(ModuleError::InvalidLinkState)
    }

    pub(crate) fn advance_evaluation_start_cursor(&mut self) -> Result<(), ModuleError> {
        let state = self
            .start_state
            .as_mut()
            .ok_or(ModuleError::InvalidLinkState)?;
        state.cursor = state
            .cursor
            .checked_add(1)
            .ok_or(ModuleError::CapacityOverflow {
                collection: "module evaluation cursor",
            })?;
        Ok(())
    }

    pub(crate) fn transition_evaluation_start_phase(
        &mut self,
        phase: ModuleStartPhase,
    ) -> Result<(), ModuleError> {
        let state = self
            .start_state
            .as_mut()
            .ok_or(ModuleError::InvalidLinkState)?;
        state.phase = phase;
        state.cursor = 0;
        state.request_cursor = 0;
        state.pending_dependencies.clear();
        state.registration_stage = ModuleRegistrationStage::Scan;
        state.dependency_cursor = 0;
        Ok(())
    }

    pub(crate) fn finish_evaluation_start(&mut self) -> Result<ModuleId, ModuleError> {
        let state = self
            .start_state
            .take()
            .ok_or(ModuleError::InvalidLinkState)?;
        Ok(state.root)
    }

    pub(crate) fn function_instantiation_cursor(
        &self,
        module: ModuleId,
    ) -> Result<u32, ModuleError> {
        Ok(self.record(module)?.function_instantiation_cursor)
    }

    pub(crate) fn set_function_instantiation_cursor(
        &mut self,
        module: ModuleId,
        cursor: u32,
    ) -> Result<(), ModuleError> {
        self.record_mut(module)?.function_instantiation_cursor = cursor;
        Ok(())
    }

    /// Advances one dependency scan, reservation, publication, or final state transition.
    pub(crate) fn advance_evaluation_registration(&mut self) -> Result<bool, ModuleError> {
        let mut state = self
            .start_state
            .take()
            .ok_or(ModuleError::InvalidLinkState)?;
        if state.phase != ModuleStartPhase::Register {
            self.start_state = Some(state);
            return Err(ModuleError::InvalidLinkState);
        }
        let result = self.advance_evaluation_registration_inner(&mut state);
        self.start_state = Some(state);
        result
    }

    /// Applies one bounded registration transition while the outer method preserves ownership.
    fn advance_evaluation_registration_inner(
        &mut self,
        state: &mut ModuleStartState,
    ) -> Result<bool, ModuleError> {
        let Some(&module) = state.order.get(state.cursor) else {
            return Ok(true);
        };
        if self.record(module)?.evaluation != ModuleEvaluationState::Unevaluated {
            state.finish_module();
            return Ok(false);
        }
        match state.registration_stage {
            ModuleRegistrationStage::ReserveParents => {
                let dependency = state.pending_dependencies[state.dependency_cursor];
                let parents = &mut self.record_mut(dependency)?.async_parents;
                if parents.capacity() == parents.len() {
                    parents
                        .try_reserve_exact(INITIAL_ASYNC_PARENT_CAPACITY.max(1))
                        .map_err(|_| ModuleError::AllocationFailed {
                            collection: "module async parents",
                        })?;
                }
                state.advance_dependency(ModuleRegistrationStage::PublishParents);
                return Ok(false);
            }
            ModuleRegistrationStage::PublishParents => {
                let dependency = state.pending_dependencies[state.dependency_cursor];
                self.record_mut(dependency)?.async_parents.push(module);
                state.advance_dependency(ModuleRegistrationStage::Finish);
                return Ok(false);
            }
            ModuleRegistrationStage::Finish => {
                let pending = u32::try_from(state.pending_dependencies.len()).map_err(|_| {
                    ModuleError::CapacityOverflow {
                        collection: "module pending dependency count",
                    }
                })?;
                let order = self.next_async_evaluation_order;
                self.next_async_evaluation_order =
                    order.checked_add(1).ok_or(ModuleError::CapacityOverflow {
                        collection: "module async evaluation order",
                    })?;
                let record = self.record_mut(module)?;
                record.pending_async_dependencies = pending;
                record.async_evaluation_order = Some(order);
                record.evaluation = ModuleEvaluationState::Waiting;
                state.finish_module();
                return Ok(false);
            }
            ModuleRegistrationStage::Scan => {}
        }
        self.scan_registration_edge(state, module)
    }

    /// Scans one requested edge, or moves a fully scanned module toward execution.
    fn scan_registration_edge(
        &mut self,
        state: &mut ModuleStartState,
        module: ModuleId,
    ) -> Result<bool, ModuleError> {
        let record = self.record(module)?;
        let ModuleStatus::Linked { cycle_root } = record.status else {
            return Err(ModuleError::InvalidLinkState);
        };
        let Some(request) = record.requested_modules.get(state.request_cursor).cloned() else {
            if state.pending_dependencies.is_empty() {
                self.queue_ready_module(module)?;
                state.finish_module();
            } else {
                state.registration_stage = ModuleRegistrationStage::ReserveParents;
                state.dependency_cursor = 0;
            }
            return Ok(false);
        };
        state.request_cursor += 1;
        let dependency = self
            .find_specifier(&request)
            .ok_or(ModuleError::MissingModule)?;
        let dependency_record = self.record(dependency)?;
        let ModuleStatus::Linked {
            cycle_root: dependency_root,
        } = dependency_record.status
        else {
            return Err(ModuleError::InvalidLinkState);
        };
        if dependency_root != cycle_root
            && let ModuleEvaluationState::Errored(error) = self.record(dependency_root)?.evaluation
        {
            self.complete_evaluation(module, Err(error))?;
            state.finish_module();
            return Ok(false);
        }
        let pending = if dependency_root == cycle_root {
            matches!(
                dependency_record.evaluation,
                ModuleEvaluationState::Waiting | ModuleEvaluationState::AsyncEvaluating(_)
            )
            .then_some(dependency)
        } else if !matches!(
            self.record(dependency_root)?.evaluation,
            ModuleEvaluationState::Evaluated(_)
        ) {
            Some(dependency_root)
        } else {
            None
        };
        if let Some(pending) = pending
            && !state.pending_dependencies.contains(&pending)
        {
            state
                .pending_dependencies
                .try_reserve_exact(1)
                .map_err(|_| ModuleError::AllocationFailed {
                    collection: "module pending dependencies",
                })?;
            state.pending_dependencies.push(pending);
        }
        Ok(false)
    }
}

impl ModuleStartState {
    fn finish_module(&mut self) {
        self.cursor += 1;
        self.request_cursor = 0;
        self.pending_dependencies.clear();
        self.registration_stage = ModuleRegistrationStage::Scan;
        self.dependency_cursor = 0;
    }

    fn advance_dependency(&mut self, next_stage: ModuleRegistrationStage) {
        self.dependency_cursor += 1;
        if self.dependency_cursor == self.pending_dependencies.len() {
            self.registration_stage = next_stage;
            self.dependency_cursor = 0;
        }
    }
}
