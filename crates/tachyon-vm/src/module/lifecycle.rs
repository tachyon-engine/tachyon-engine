//! Host-driven in-memory module loading and synchronous evaluation lifecycle.

use super::*;
use crate::{ExecutionBudget, ExecutionError, Isolate, RunOutcome};

/// One complete loader result. Source parsing remains outside this VM-owned lifecycle slice.
#[derive(Debug)]
pub struct LoadedModule {
    pub(crate) record: ModuleRecordInit,
    pub(crate) body: ModuleBody,
}

impl LoadedModule {
    #[must_use]
    pub const fn new(record: ModuleRecordInit, body: ModuleBody) -> Self {
        Self { record, body }
    }
}

/// Host capability for canonical resolution and loading from memory or another adapter.
pub trait ModuleLoader {
    type Error;

    fn resolve(
        &mut self,
        request: &ModuleSpecifier,
        referrer: Option<&ModuleSpecifier>,
    ) -> Result<ModuleSpecifier, Self::Error>;

    fn load(&mut self, resolved: &ModuleSpecifier) -> Result<Option<LoadedModule>, Self::Error>;
}

#[derive(Debug)]
pub enum ModuleLoadError<E> {
    Loader(E),
    Missing(ModuleSpecifier),
    Graph(ModuleError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleEvaluationError {
    Graph(ModuleError),
    Execution(ExecutionError),
    AsyncEvaluationRequired(ModuleId),
}

impl ModuleRecordInit {
    /// Resolves each distinct request once and rewrites all import/export references consistently.
    fn resolve_requests<E>(&mut self, loader: &mut impl ModuleLoader<Error = E>) -> Result<(), E> {
        for request_index in 0..self.requested_modules.len() {
            let original = self.requested_modules[request_index].clone();
            let resolved = loader.resolve(&original, Some(&self.specifier))?;
            self.requested_modules[request_index] = resolved.clone();
            for import in &mut self.imports {
                if import.module_request == original {
                    import.module_request = resolved.clone();
                }
            }
            for export in &mut self.exports {
                if let ExportEntry::Indirect { module_request, .. } = export
                    && *module_request == original
                {
                    *module_request = resolved.clone();
                }
            }
        }
        Ok(())
    }
}

impl ModuleGraph {
    fn find_specifier(&self, specifier: &ModuleSpecifier) -> Option<ModuleId> {
        self.records
            .iter()
            .find(|record| &record.specifier == specifier)
            .map(|record| record.id)
    }

    /// Produces dependency postorder without recursion; cyclic back-edges are ignored while active.
    fn evaluation_order(&self, root: ModuleId) -> Result<Vec<ModuleId>, ModuleError> {
        let max_work = self.limits.max_modules as usize;
        let mut state = Vec::new();
        let mut frames = Vec::new();
        let mut order = Vec::new();
        state
            .try_reserve_exact(self.records.len())
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "module evaluation state",
            })?;
        state.resize(self.records.len(), 0);
        let initial_work = INITIAL_LINK_WORK_CAPACITY.min(max_work);
        frames
            .try_reserve_exact(initial_work)
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "module evaluation frames",
            })?;
        order
            .try_reserve_exact(initial_work)
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "module evaluation order",
            })?;
        frames.push((root, 0_usize));
        state[root.index()] = 1;
        while let Some((module, next_request)) = frames.last_mut() {
            let record = self.record(*module)?;
            if *next_request < record.requested_modules.len() {
                let request = &record.requested_modules[*next_request];
                *next_request += 1;
                let child = self
                    .find_specifier(request)
                    .ok_or(ModuleError::MissingModule)?;
                if state[child.index()] == 0 {
                    if frames.len() >= max_work {
                        return Err(ModuleError::EvaluationOrderLimit {
                            limit: self.limits.max_modules,
                        });
                    }
                    frames
                        .try_reserve_exact(1)
                        .map_err(|_| ModuleError::AllocationFailed {
                            collection: "module evaluation frames",
                        })?;
                    state[child.index()] = 1;
                    frames.push((child, 0));
                }
                continue;
            }
            let module = *module;
            frames.pop();
            state[module.index()] = 2;
            order
                .try_reserve_exact(1)
                .map_err(|_| ModuleError::AllocationFailed {
                    collection: "module evaluation order",
                })?;
            order.push(module);
        }
        Ok(order)
    }
}

impl Isolate {
    /// Initializes or updates one local module binding; imported aliases remain read-only.
    pub fn write_module_binding(
        &mut self,
        module: ModuleId,
        name: &str,
        value: Value,
    ) -> Result<(), ModuleError> {
        self.module_graph.write_binding(module, name, value)
    }

    /// Reads a linked local/imported binding while preserving TDZ errors.
    pub fn read_module_binding(&self, module: ModuleId, name: &str) -> Result<Value, ModuleError> {
        self.module_graph.read_binding(module, name)
    }

    /// Resolves and loads a complete graph through host callbacks, then links it transactionally.
    pub fn load_module_graph<L: ModuleLoader>(
        &mut self,
        loader: &mut L,
        root_request: &ModuleSpecifier,
    ) -> Result<ModuleId, ModuleLoadError<L::Error>> {
        let checkpoint = self.module_graph.checkpoint();
        let result = self.load_module_graph_inner(loader, root_request);
        if result.is_err() {
            self.module_graph.rollback(checkpoint);
        }
        result
    }

    /// Performs one load transaction; the public wrapper owns rollback on every failure branch.
    fn load_module_graph_inner<L: ModuleLoader>(
        &mut self,
        loader: &mut L,
        root_request: &ModuleSpecifier,
    ) -> Result<ModuleId, ModuleLoadError<L::Error>> {
        let root = loader
            .resolve(root_request, None)
            .map_err(ModuleLoadError::Loader)?;
        let max_work = self.module_graph.limits.max_edges.max(1) as usize;
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(INITIAL_LINK_WORK_CAPACITY.min(max_work))
            .map_err(|_| {
                ModuleLoadError::Graph(ModuleError::AllocationFailed {
                    collection: "pending module loads",
                })
            })?;
        pending.push(root.clone());
        while let Some(specifier) = pending.pop() {
            if self.module_graph.find_specifier(&specifier).is_some() {
                continue;
            }
            let mut loaded = loader
                .load(&specifier)
                .map_err(ModuleLoadError::Loader)?
                .ok_or_else(|| ModuleLoadError::Missing(specifier.clone()))?;
            if loaded.record.specifier != specifier {
                return Err(ModuleLoadError::Graph(ModuleError::LoaderIdentityMismatch));
            }
            loaded
                .record
                .resolve_requests(loader)
                .map_err(ModuleLoadError::Loader)?;
            if pending
                .len()
                .checked_add(loaded.record.requested_modules.len())
                .is_none_or(|work| work > max_work)
            {
                return Err(ModuleLoadError::Graph(ModuleError::EdgeLimit {
                    limit: self.module_graph.limits.max_edges,
                }));
            }
            pending
                .try_reserve_exact(loaded.record.requested_modules.len())
                .map_err(|_| {
                    ModuleLoadError::Graph(ModuleError::AllocationFailed {
                        collection: "pending module loads",
                    })
                })?;
            pending.extend(loaded.record.requested_modules.iter().rev().cloned());
            self.module_graph
                .insert_with_body(loaded.record, loaded.body)
                .map_err(ModuleLoadError::Graph)?;
        }
        let root = self
            .module_graph
            .find_specifier(&root)
            .ok_or_else(|| ModuleLoadError::Missing(root.clone()))?;
        self.module_graph
            .link(root)
            .map_err(ModuleLoadError::Graph)?;
        Ok(root)
    }

    /// Evaluates linked synchronous dependencies once and returns the root's cached completion.
    pub fn evaluate_module(&mut self, root: ModuleId) -> Result<RunOutcome, ModuleEvaluationError> {
        self.evaluate_module_with_batch::<{ crate::tuning::dispatch::DEFAULT_DISPATCH_BATCH }>(root)
    }

    /// Executes dependency postorder while keeping graph borrows outside interpreter entry.
    fn evaluate_module_with_batch<const N: usize>(
        &mut self,
        root: ModuleId,
    ) -> Result<RunOutcome, ModuleEvaluationError> {
        let order = self
            .module_graph
            .evaluation_order(root)
            .map_err(ModuleEvaluationError::Graph)?;
        let mut root_result = Value::from_immediate(tachyon_value::Immediate::Undefined);
        for module in order {
            let (state, body) = {
                let record = self
                    .module_graph
                    .record(module)
                    .map_err(ModuleEvaluationError::Graph)?;
                if !matches!(record.status, ModuleStatus::Linked { .. }) {
                    return Err(ModuleEvaluationError::Graph(ModuleError::InvalidLinkState));
                }
                (record.evaluation, record.body.clone())
            };
            match state {
                ModuleEvaluationState::Evaluated(value) => {
                    root_result = value;
                    continue;
                }
                ModuleEvaluationState::Errored(error) => return Ok(RunOutcome::Thrown(error)),
                ModuleEvaluationState::Evaluating => {
                    return Err(ModuleEvaluationError::Graph(ModuleError::InvalidLinkState));
                }
                ModuleEvaluationState::Unevaluated => {}
            }
            if matches!(body, ModuleBody::AsyncPrecompiled(_)) {
                return Err(ModuleEvaluationError::AsyncEvaluationRequired(module));
            }
            self.module_graph.records[module.index()].evaluation =
                ModuleEvaluationState::Evaluating;
            let outcome = match body {
                ModuleBody::Synthetic => Ok(RunOutcome::Completed(Value::from_immediate(
                    tachyon_value::Immediate::Undefined,
                ))),
                ModuleBody::Precompiled(body) => self.load_module(&body).and_then(|code| {
                    self.execute_loaded_with_batch::<N>(
                        code,
                        ExecutionBudget {
                            fuel: u64::MAX,
                            quantum: u32::MAX,
                        },
                    )
                }),
                ModuleBody::AsyncPrecompiled(_) => unreachable!("async bodies return above"),
            };
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.module_graph.records[module.index()].evaluation =
                        ModuleEvaluationState::Unevaluated;
                    return Err(ModuleEvaluationError::Execution(error));
                }
            };
            match outcome {
                RunOutcome::Completed(value) => {
                    self.module_graph.records[module.index()].evaluation =
                        ModuleEvaluationState::Evaluated(value);
                    root_result = value;
                }
                RunOutcome::Thrown(error) => {
                    self.module_graph.records[module.index()].evaluation =
                        ModuleEvaluationState::Errored(error);
                    return Ok(RunOutcome::Thrown(error));
                }
                RunOutcome::BudgetExhausted => unreachable!("unbounded module evaluation"),
            }
        }
        Ok(RunOutcome::Completed(root_result))
    }

    #[cfg(test)]
    pub(crate) fn evaluate_module_with_test_batch<const N: usize>(
        &mut self,
        root: ModuleId,
    ) -> Result<RunOutcome, ModuleEvaluationError> {
        self.evaluate_module_with_batch::<N>(root)
    }
}
