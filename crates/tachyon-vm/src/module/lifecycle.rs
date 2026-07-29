//! Host-driven in-memory module loading and synchronous evaluation lifecycle.

use super::*;
use crate::{ExecutionError, Fiber, Isolate, RunOutcome};

/// Loader content; precompiled records are derived exclusively from verified module stencils.
#[derive(Debug)]
pub struct LoadedModule {
    kind: LoadedModuleKind,
}

impl LoadedModule {
    #[must_use]
    pub const fn precompiled(module: CompiledModule) -> Self {
        Self {
            kind: LoadedModuleKind::Precompiled(module),
        }
    }

    #[cfg(test)]
    pub(crate) const fn new(record: ModuleRecordInit, body: ModuleBody) -> Self {
        Self {
            kind: LoadedModuleKind::Legacy(record, body),
        }
    }

    #[cfg(test)]
    pub(crate) fn identity(&self) -> &ModuleIdentity {
        match &self.kind {
            LoadedModuleKind::Legacy(record, _) => &record.specifier,
            LoadedModuleKind::Precompiled(_) => {
                panic!("test identity is only defined for legacy modules")
            }
        }
    }
}

#[derive(Debug)]
enum LoadedModuleKind {
    Precompiled(CompiledModule),
    #[cfg(test)]
    Legacy(ModuleRecordInit, ModuleBody),
}

/// One resolved edge passed to `load` without discarding its source request or referrer.
#[derive(Clone, Copy, Debug)]
pub struct ResolvedModuleRequest<'a> {
    identity: &'a ModuleIdentity,
    request: Option<&'a tachyon_bytecode::ModuleRequest>,
    referrer: Option<&'a ModuleIdentity>,
}

impl<'a> ResolvedModuleRequest<'a> {
    #[must_use]
    pub const fn identity(self) -> &'a ModuleIdentity {
        self.identity
    }

    #[must_use]
    pub const fn request(self) -> Option<&'a tachyon_bytecode::ModuleRequest> {
        self.request
    }

    #[must_use]
    pub const fn referrer(self) -> Option<&'a ModuleIdentity> {
        self.referrer
    }
}

/// Host capability for canonical resolution and loading from memory or another adapter.
pub trait ModuleLoader {
    type Error;

    fn resolve(
        &mut self,
        request: &tachyon_bytecode::ModuleRequest,
        referrer: Option<&ModuleIdentity>,
    ) -> Result<ModuleIdentity, Self::Error>;

    fn load(
        &mut self,
        resolved: ResolvedModuleRequest<'_>,
    ) -> Result<Option<LoadedModule>, Self::Error>;
}

#[derive(Debug)]
pub enum ModuleLoadError<E> {
    Loader(E),
    Missing(ModuleIdentity),
    Graph(ModuleError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleEvaluationError {
    Graph(ModuleError),
    Execution(ExecutionError),
    AsyncEvaluationPending(ModuleId),
}

#[derive(Debug)]
struct PendingModule {
    identity: ModuleIdentity,
    request: Option<tachyon_bytecode::ModuleRequest>,
    referrer: Option<ModuleIdentity>,
}

/// Converts one verified compiler stencil into VM-owned record tables without lossy names.
fn prepare_precompiled_module<E>(
    identity: ModuleIdentity,
    module: CompiledModule,
    loader: &mut impl ModuleLoader<Error = E>,
) -> Result<(ModuleRecordInit, ModuleBody, Vec<PendingModule>), ModuleLoadError<E>> {
    let stencil = module
        .module_stencil()
        .ok_or(ModuleLoadError::Graph(ModuleError::MissingModule))?;
    let mut requests = Vec::with_capacity(stencil.requested_modules().len());
    let mut dependencies = Vec::with_capacity(stencil.requested_modules().len());
    for request in stencil.requested_modules() {
        let request = request.clone();
        let resolved = loader
            .resolve(&request, Some(&identity))
            .map_err(ModuleLoadError::Loader)?;
        requests.push(resolved.clone());
        dependencies.push(PendingModule {
            identity: resolved,
            request: Some(request),
            referrer: Some(identity.clone()),
        });
    }
    let imports = stencil
        .imports()
        .iter()
        .map(|entry| {
            let request = requests
                .get(entry.module_request.index() as usize)
                .cloned()
                .ok_or(ModuleLoadError::Graph(ModuleError::MissingModule))?;
            let import_name = match &entry.import_name {
                tachyon_bytecode::ModuleImportName::Name(name) => ModuleImportName::Name(
                    ModuleExportName::try_from_utf16(name)
                        .map_err(|error| ModuleLoadError::Graph(error))?,
                ),
                tachyon_bytecode::ModuleImportName::Namespace => ModuleImportName::Namespace,
            };
            Ok(ImportEntry::new(
                request,
                import_name,
                ModuleBindingName::try_new(&entry.local_name)
                    .map_err(|error| ModuleLoadError::Graph(error))?,
            ))
        })
        .collect::<Result<Vec<_>, ModuleLoadError<E>>>()?;
    let exports = stencil
        .exports()
        .iter()
        .map(|entry| match entry {
            tachyon_bytecode::ModuleExportEntry::Local {
                export_name,
                local_name,
            } => Ok(ExportEntry::Local {
                export_name: ModuleExportName::try_from_utf16(export_name)?,
                local_name: ModuleBindingName::try_new(local_name)?,
            }),
            tachyon_bytecode::ModuleExportEntry::Indirect {
                export_name,
                module_request,
                import_name,
            } => {
                let request = requests
                    .get(module_request.index() as usize)
                    .cloned()
                    .ok_or(ModuleError::MissingModule)?;
                let import_name = match import_name {
                    tachyon_bytecode::ModuleImportName::Name(name) => {
                        ModuleImportName::Name(ModuleExportName::try_from_utf16(name)?)
                    }
                    tachyon_bytecode::ModuleImportName::Namespace => ModuleImportName::Namespace,
                };
                Ok(ExportEntry::Indirect {
                    export_name: ModuleExportName::try_from_utf16(export_name)?,
                    module_request: request,
                    import_name,
                })
            }
            tachyon_bytecode::ModuleExportEntry::Star { module_request } => {
                let request = requests
                    .get(module_request.index() as usize)
                    .cloned()
                    .ok_or(ModuleError::MissingModule)?;
                Ok(ExportEntry::Star {
                    module_request: request,
                })
            }
        })
        .collect::<Result<Vec<_>, ModuleError>>()
        .map_err(ModuleLoadError::Graph)?;
    let local_bindings = stencil
        .local_bindings()
        .iter()
        .map(|name| ModuleBindingName::try_new(name))
        .collect::<Result<Vec<_>, _>>()
        .map_err(ModuleLoadError::Graph)?;
    let record = ModuleRecordInit {
        specifier: identity,
        requested_modules: requests.into_boxed_slice(),
        imports: imports.into_boxed_slice(),
        exports: exports.into_boxed_slice(),
        local_bindings: local_bindings.into_boxed_slice(),
        has_top_level_await: stencil.has_top_level_await(),
    };
    Ok((record, ModuleBody::Precompiled(module), dependencies))
}

impl ModuleGraph {
    pub(crate) fn find_specifier(&self, specifier: &ModuleIdentity) -> Option<ModuleId> {
        self.records
            .iter()
            .find(|record| &record.specifier == specifier)
            .map(|record| record.id)
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
    pub fn read_module_binding(
        &mut self,
        module: ModuleId,
        name: &str,
    ) -> Result<Value, ExecutionError> {
        match self
            .module_graph
            .binding_target(module, name)
            .map_err(ExecutionError::Module)?
        {
            ModuleBindingTarget::Cell(cell) => self
                .module_graph
                .read_namespace_cell(cell)
                .map_err(ExecutionError::Module),
            ModuleBindingTarget::Namespace(module) => self.get_module_namespace(module),
        }
    }

    /// Resolves and loads a complete graph through host callbacks, then links it transactionally.
    pub fn load_module_graph<L: ModuleLoader>(
        &mut self,
        loader: &mut L,
        root_identity: &ModuleIdentity,
    ) -> Result<ModuleId, ModuleLoadError<L::Error>> {
        let checkpoint = self.module_graph.checkpoint();
        let result = self.load_module_graph_inner(loader, root_identity);
        if result.is_err() {
            self.module_graph.rollback(checkpoint);
        }
        result
    }

    /// Performs one load transaction; the public wrapper owns rollback on every failure branch.
    fn load_module_graph_inner<L: ModuleLoader>(
        &mut self,
        loader: &mut L,
        root_identity: &ModuleIdentity,
    ) -> Result<ModuleId, ModuleLoadError<L::Error>> {
        let root = root_identity.clone();
        let max_work = self.module_graph.limits.max_edges.max(1) as usize;
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(INITIAL_LINK_WORK_CAPACITY.min(max_work))
            .map_err(|_| {
                ModuleLoadError::Graph(ModuleError::AllocationFailed {
                    collection: "pending module loads",
                })
            })?;
        pending.push(PendingModule {
            identity: root.clone(),
            request: None,
            referrer: None,
        });
        while let Some(pending_module) = pending.pop() {
            if self
                .module_graph
                .find_specifier(&pending_module.identity)
                .is_some()
            {
                continue;
            }
            let request = ResolvedModuleRequest {
                identity: &pending_module.identity,
                request: pending_module.request.as_ref(),
                referrer: pending_module.referrer.as_ref(),
            };
            let loaded = loader
                .load(request)
                .map_err(ModuleLoadError::Loader)?
                .ok_or_else(|| ModuleLoadError::Missing(pending_module.identity.clone()))?;
            let (record, body, dependencies) = match loaded.kind {
                LoadedModuleKind::Precompiled(module) => {
                    prepare_precompiled_module(pending_module.identity.clone(), module, loader)?
                }
                #[cfg(test)]
                LoadedModuleKind::Legacy(mut record, body) => {
                    record.specifier = pending_module.identity.clone();
                    let dependencies = record
                        .requested_modules
                        .iter()
                        .cloned()
                        .map(|identity| PendingModule {
                            identity,
                            request: None,
                            referrer: Some(record.specifier.clone()),
                        })
                        .collect();
                    (record, body, dependencies)
                }
            };
            if pending
                .len()
                .checked_add(dependencies.len())
                .is_none_or(|work| work > max_work)
            {
                return Err(ModuleLoadError::Graph(ModuleError::EdgeLimit {
                    limit: self.module_graph.limits.max_edges,
                }));
            }
            pending.try_reserve_exact(dependencies.len()).map_err(|_| {
                ModuleLoadError::Graph(ModuleError::AllocationFailed {
                    collection: "pending module loads",
                })
            })?;
            pending.extend(dependencies.into_iter().rev());
            self.module_graph
                .insert_with_body(record, body)
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
        if let Some(outcome) = self
            .module_graph
            .evaluation_outcome(root)
            .map_err(ModuleEvaluationError::Graph)?
        {
            return Ok(outcome);
        }
        if self.driver_is_busy() {
            return Err(ModuleEvaluationError::Execution(ExecutionError::DriverBusy));
        }
        if !self.module_graph.evaluation_start_pending() {
            self.module_graph
                .begin_evaluation_start(root)
                .map_err(ModuleEvaluationError::Graph)?;
        }
        while self.module_graph.evaluation_start_pending() {
            self.advance_module_start_transition()
                .map_err(ModuleEvaluationError::Execution)?;
        }
        self.evaluate_module_with_batch::<{ crate::tuning::dispatch::DEFAULT_DISPATCH_BATCH }>(root)
    }

    /// Starts or resumes evaluation and returns the cycle root's stable intrinsic Promise.
    pub fn evaluate_module_promise(
        &mut self,
        root: ModuleId,
    ) -> Result<Value, ModuleEvaluationError> {
        self.evaluate_module_promise_start(root)
    }

    /// Creates the public evaluation Promise before any module body becomes observable.
    fn evaluate_module_promise_start(
        &mut self,
        root: ModuleId,
    ) -> Result<Value, ModuleEvaluationError> {
        if let Some(promise) = self
            .module_graph
            .evaluation_promise(root)
            .map_err(ModuleEvaluationError::Graph)?
        {
            return Ok(promise);
        }
        if self.driver_is_busy() {
            return Err(ModuleEvaluationError::Execution(ExecutionError::DriverBusy));
        }
        let promise = self
            .create_promise(
                crate::PromiseState::Pending,
                Value::from_immediate(tachyon_value::Immediate::Undefined),
            )
            .map_err(ModuleEvaluationError::Execution)?;
        self.module_graph
            .publish_evaluation_promise(root, promise)
            .map_err(ModuleEvaluationError::Graph)?;
        if let Err(error) = self.module_graph.begin_evaluation_start(root) {
            let _ = self.module_graph.clear_evaluation_promise(root, promise);
            return Err(ModuleEvaluationError::Graph(error));
        }
        Ok(promise)
    }

    /// Advances one graph traversal, function declaration, or dependency-registration transition.
    pub(crate) fn advance_module_start_transition(&mut self) -> Result<(), ExecutionError> {
        match self
            .module_graph
            .evaluation_start_phase()
            .map_err(ExecutionError::Module)?
        {
            ModuleStartPhase::Traverse => {
                self.module_graph
                    .advance_evaluation_traversal()
                    .map_err(ExecutionError::Module)?;
            }
            ModuleStartPhase::Instantiate => {
                let Some(module) = self
                    .module_graph
                    .evaluation_start_module()
                    .map_err(ExecutionError::Module)?
                else {
                    self.module_graph
                        .transition_evaluation_start_phase(ModuleStartPhase::Register)
                        .map_err(ExecutionError::Module)?;
                    return Ok(());
                };
                if self.module_graph.records[module.index()].evaluation
                    != ModuleEvaluationState::Unevaluated
                {
                    self.module_graph
                        .advance_evaluation_start_cursor()
                        .map_err(ExecutionError::Module)?;
                    return Ok(());
                }
                let ModuleBody::Precompiled(body) =
                    self.module_graph.records[module.index()].body.clone()
                else {
                    self.module_graph
                        .advance_evaluation_start_cursor()
                        .map_err(ExecutionError::Module)?;
                    return Ok(());
                };
                let code = self.load_module(&body)?;
                if self.instantiate_next_module_function(code, module)? {
                    self.module_graph
                        .advance_evaluation_start_cursor()
                        .map_err(ExecutionError::Module)?;
                }
            }
            ModuleStartPhase::Register => {
                if self
                    .module_graph
                    .advance_evaluation_registration()
                    .map_err(ExecutionError::Module)?
                {
                    self.module_graph
                        .finish_evaluation_start()
                        .map_err(ExecutionError::Module)?;
                }
            }
        }
        Ok(())
    }

    /// Starts every dependency-ready body and interleaves suspended modules at Promise job turns.
    fn evaluate_module_with_batch<const N: usize>(
        &mut self,
        root: ModuleId,
    ) -> Result<RunOutcome, ModuleEvaluationError> {
        loop {
            match self.module_graph.records[root.index()].evaluation {
                ModuleEvaluationState::Evaluated(value) => {
                    self.finish_async_module_checkpoint::<N>()
                        .map_err(ModuleEvaluationError::Execution)?;
                    return Ok(RunOutcome::Completed(value));
                }
                ModuleEvaluationState::Errored(error) => {
                    self.finish_async_module_checkpoint::<N>()
                        .map_err(ModuleEvaluationError::Execution)?;
                    return Ok(RunOutcome::Thrown(error));
                }
                ModuleEvaluationState::Unevaluated
                | ModuleEvaluationState::Waiting
                | ModuleEvaluationState::Evaluating
                | ModuleEvaluationState::AsyncEvaluating(_) => {}
            }
            if let Some(module) = self.module_graph.take_ready_module() {
                self.execute_ready_module_with_batch::<N>(module)?;
                continue;
            }
            if self.promise_jobs.has_pending() {
                self.advance_async_module_turn::<N>()
                    .map_err(ModuleEvaluationError::Execution)?;
                continue;
            }
            return Err(ModuleEvaluationError::AsyncEvaluationPending(root));
        }
    }

    /// Executes one module whose external SCC dependencies have all finished.
    fn execute_ready_module_with_batch<const N: usize>(
        &mut self,
        module: ModuleId,
    ) -> Result<(), ModuleEvaluationError> {
        let outcome = match self.start_ready_module_with_budget::<N>(
            module,
            crate::ExecutionBudget {
                fuel: u64::MAX,
                quantum: u32::MAX,
            },
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.module_graph.records[module.index()].evaluation =
                    ModuleEvaluationState::Unevaluated;
                self.fiber = Fiber::default();
                return Err(ModuleEvaluationError::Execution(error));
            }
        };
        self.finish_ready_module_outcome(module, outcome)
            .map_err(ModuleEvaluationError::Execution)?;
        Ok(())
    }

    /// Starts one dependency-ready module without draining work published by its body.
    pub(crate) fn start_ready_module_with_budget<const N: usize>(
        &mut self,
        module: ModuleId,
        budget: crate::ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        let body = self.module_graph.records[module.index()].body.clone();
        if !matches!(
            self.module_graph.records[module.index()].evaluation,
            ModuleEvaluationState::Unevaluated | ModuleEvaluationState::Waiting
        ) {
            return Err(ExecutionError::Module(ModuleError::InvalidLinkState));
        }
        self.module_graph.records[module.index()].evaluation = ModuleEvaluationState::Evaluating;
        let outcome = match body {
            ModuleBody::Synthetic => Ok(RunOutcome::Completed(Value::from_immediate(
                tachyon_value::Immediate::Undefined,
            ))),
            ModuleBody::Precompiled(body) => {
                let code = self.load_module(&body)?;
                if self.module_graph.records[module.index()].has_top_level_await {
                    self.begin_async_module_with_budget::<N>(code, module, budget)
                        .map(|(_, outcome)| outcome)
                } else {
                    self.execute_loaded_module_with_budget::<N>(code, module, budget)
                }
            }
        };
        if outcome.is_err() {
            self.module_graph.records[module.index()].evaluation =
                ModuleEvaluationState::Unevaluated;
            self.fiber = Fiber::default();
        }
        outcome
    }

    /// Transfers a completed driver Fiber into persistent module graph state.
    pub(crate) fn finish_ready_module_outcome(
        &mut self,
        module: ModuleId,
        outcome: RunOutcome,
    ) -> Result<(), ExecutionError> {
        match outcome {
            RunOutcome::Completed(value)
                if !matches!(
                    self.module_graph.records[module.index()].evaluation,
                    ModuleEvaluationState::AsyncEvaluating(_)
                ) =>
            {
                self.module_graph
                    .complete_evaluation(module, Ok(value))
                    .map_err(ExecutionError::Module)?;
            }
            RunOutcome::Thrown(error) => self
                .module_graph
                .complete_evaluation(module, Err(error))
                .map_err(ExecutionError::Module)?,
            RunOutcome::Completed(_) => {}
            RunOutcome::BudgetExhausted => return Ok(()),
        }
        if !matches!(
            self.module_graph.records[module.index()].evaluation,
            ModuleEvaluationState::AsyncEvaluating(_)
        ) {
            self.fiber = Fiber::default();
        }
        Ok(())
    }

    /// Settles the cycle-root evaluation Promise whose ModuleRecord is already terminal.
    pub(crate) fn settle_completed_module_promise(
        &mut self,
        root: Option<ModuleId>,
        promise: Value,
    ) -> Result<(), ExecutionError> {
        let Some(root) = root else {
            return Ok(());
        };
        let record = self
            .module_graph
            .record(root)
            .map_err(ExecutionError::Module)?;
        if record.evaluation_promise != Some(promise) {
            return Err(ExecutionError::Module(ModuleError::InvalidLinkState));
        }
        let terminal = match record.evaluation {
            ModuleEvaluationState::Evaluated(_) => Some((
                crate::PromiseState::Fulfilled,
                Value::from_immediate(tachyon_value::Immediate::Undefined),
            )),
            ModuleEvaluationState::Errored(error) => Some((crate::PromiseState::Rejected, error)),
            ModuleEvaluationState::Unevaluated
            | ModuleEvaluationState::Waiting
            | ModuleEvaluationState::Evaluating
            | ModuleEvaluationState::AsyncEvaluating(_) => None,
        };
        let Some((state, result)) = terminal else {
            return Ok(());
        };
        if self.promise_snapshot(promise)?.state == crate::PromiseState::Pending {
            self.settle_promise(promise, state, result)?;
        }
        Ok(())
    }

    /// Advances one Promise job and any bytecode Fiber it publishes or resumes.
    fn advance_async_module_turn<const N: usize>(&mut self) -> Result<(), ExecutionError> {
        let checkpoint = self.promise_checkpoint(
            Value::from_immediate(tachyon_value::Immediate::Undefined),
            tachyon_bytecode::WordOffset::new(0),
        )?;
        if checkpoint.is_none() && !self.fiber.frames.is_empty() {
            let _ = self.continue_active_module_with_batch::<N>()?;
        }
        Ok(())
    }

    /// Closes the checkpoint that delivered the final Await reaction before another entry starts.
    fn finish_async_module_checkpoint<const N: usize>(&mut self) -> Result<(), ExecutionError> {
        while self.promise_jobs.checkpoint_result.is_some() || self.promise_jobs.has_pending() {
            let checkpoint = self.promise_checkpoint(
                Value::from_immediate(tachyon_value::Immediate::Undefined),
                tachyon_bytecode::WordOffset::new(0),
            )?;
            if checkpoint.is_none() && !self.fiber.frames.is_empty() {
                let _ = self.continue_active_module_with_batch::<N>()?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn evaluate_module_with_test_batch<const N: usize>(
        &mut self,
        root: ModuleId,
    ) -> Result<RunOutcome, ModuleEvaluationError> {
        if !self.module_graph.evaluation_start_pending()
            && self.module_graph.records[root.index()].evaluation
                == ModuleEvaluationState::Unevaluated
        {
            self.module_graph
                .begin_evaluation_start(root)
                .map_err(ModuleEvaluationError::Graph)?;
        }
        while self.module_graph.evaluation_start_pending() {
            self.advance_module_start_transition()
                .map_err(ModuleEvaluationError::Execution)?;
        }
        self.evaluate_module_with_batch::<N>(root)
    }
}
