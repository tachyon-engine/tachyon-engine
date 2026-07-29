//! Host-driven in-memory module loading and synchronous evaluation lifecycle.

use super::*;
use crate::{ExecutionBudget, ExecutionError, Isolate, RunOutcome};

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
    AsyncEvaluationRequired(ModuleId),
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
    fn find_specifier(&self, specifier: &ModuleIdentity) -> Option<ModuleId> {
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
            if self.module_graph.records[module.index()].has_top_level_await {
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
