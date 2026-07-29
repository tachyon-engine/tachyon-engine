//! Owned ECMAScript module records and isolate-local live binding storage.

mod lifecycle;
mod link;
mod namespace;

pub(crate) use namespace::ModuleNamespaceObject;

pub use lifecycle::{
    LoadedModule, ModuleEvaluationError, ModuleLoadError, ModuleLoader, ResolvedModuleRequest,
};

use core::num::NonZeroU32;
use std::sync::Arc;

use tachyon_gc::{Trace, Tracer};

use crate::{CompiledModule, Value, tuning::modules::*};

/// Stable index of one record in an append-only module graph.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ModuleId(NonZeroU32);

impl ModuleId {
    fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
    }

    pub(crate) const fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

/// Stable index of one mutable cell shared by exports and importing aliases.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub(crate) struct BindingCellId(NonZeroU32);

impl BindingCellId {
    fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
    }

    const fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

const _: [(); 4] = [(); core::mem::size_of::<ModuleId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<ModuleId>>()];
const _: [(); 4] = [(); core::mem::size_of::<BindingCellId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<BindingCellId>>()];

/// Canonical host-resolved module identity preserving exact ECMAScript string code units.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModuleIdentity(Arc<[u8]>);

impl ModuleIdentity {
    pub fn try_new(value: &str) -> Result<Self, ModuleError> {
        Self::try_from_bytes(value.as_bytes())
    }

    pub fn try_from_bytes(value: &[u8]) -> Result<Self, ModuleError> {
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(value.len())
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "module identity",
            })?;
        owned.extend_from_slice(value);
        Ok(Self(owned.into()))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Identifier binding name, which cannot contain lone surrogates by ECMAScript grammar.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModuleBindingName(Arc<str>);

impl ModuleBindingName {
    pub fn try_new(value: &str) -> Result<Self, ModuleError> {
        try_owned_str(value, "module binding name").map(|name| Self(name.into()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Public import/export name preserving exact ECMAScript string code units.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModuleExportName(Arc<[u16]>);

impl ModuleExportName {
    pub fn try_new(value: &str) -> Result<Self, ModuleError> {
        try_owned_units(
            value.encode_utf16(),
            value.encode_utf16().count(),
            "module export name",
        )
        .map(Self)
    }

    pub fn try_from_utf16(value: &[u16]) -> Result<Self, ModuleError> {
        try_owned_units(value.iter().copied(), value.len(), "module export name").map(Self)
    }

    #[must_use]
    pub fn as_utf16(&self) -> &[u16] {
        &self.0
    }

    fn is_default(&self) -> bool {
        const DEFAULT: &[u16] = &[0x64, 0x65, 0x66, 0x61, 0x75, 0x6c, 0x74];
        self.0.as_ref() == DEFAULT
    }
}

/// Import target where namespace is a semantic sentinel, never the string `"*"`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleImportName {
    Name(ModuleExportName),
    Namespace,
}

fn try_owned_str(value: &str, collection: &'static str) -> Result<Box<str>, ModuleError> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(value.len())
        .map_err(|_| ModuleError::AllocationFailed { collection })?;
    owned.push_str(value);
    Ok(owned.into_boxed_str())
}

fn try_owned_units(
    value: impl Iterator<Item = u16>,
    unit_len: usize,
    collection: &'static str,
) -> Result<Arc<[u16]>, ModuleError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(unit_len)
        .map_err(|_| ModuleError::AllocationFailed { collection })?;
    owned.extend(value);
    Ok(owned.into())
}

/// One named import whose local alias resolves to the exporting module's cell during linking.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportEntry {
    module_request: ModuleIdentity,
    import_name: ModuleImportName,
    local_name: ModuleBindingName,
    resolved: Option<ResolvedBinding>,
}

impl ImportEntry {
    pub const fn new(
        module_request: ModuleIdentity,
        import_name: ModuleImportName,
        local_name: ModuleBindingName,
    ) -> Self {
        Self {
            module_request,
            import_name,
            local_name,
            resolved: None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn resolved(&self) -> Option<&ResolvedBinding> {
        self.resolved.as_ref()
    }
}

/// Local, indirect, or star export with namespace represented by `ModuleImportName`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportEntry {
    Local {
        export_name: ModuleExportName,
        local_name: ModuleBindingName,
    },
    Indirect {
        export_name: ModuleExportName,
        module_request: ModuleIdentity,
        import_name: ModuleImportName,
    },
    Star {
        module_request: ModuleIdentity,
    },
}

impl ExportEntry {
    pub(crate) fn export_name(&self) -> Option<&ModuleExportName> {
        match self {
            Self::Local { export_name, .. } | Self::Indirect { export_name, .. } => {
                Some(export_name)
            }
            Self::Star { .. } => None,
        }
    }
}

/// Specification-level resolved binding used for star equality and namespace propagation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedBinding {
    module: ModuleId,
    binding: ResolvedBindingName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ResolvedBindingName {
    Local(ModuleBindingName),
    Namespace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModuleBindingTarget {
    Cell(BindingCellId),
    Namespace(ModuleId),
}

/// Inputs already counted and frozen by the compiler or synthetic-module builder.
#[derive(Debug)]
pub struct ModuleRecordInit {
    pub(crate) specifier: ModuleIdentity,
    pub(crate) requested_modules: Box<[ModuleIdentity]>,
    pub(crate) imports: Box<[ImportEntry]>,
    pub(crate) exports: Box<[ExportEntry]>,
    pub(crate) local_bindings: Box<[ModuleBindingName]>,
    pub(crate) has_top_level_await: bool,
}

impl ModuleRecordInit {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModuleStatus {
    Unlinked,
    Linking { dfs_index: u32, ancestor_index: u32 },
    Linked { cycle_root: ModuleId },
}

#[derive(Debug)]
struct LocalBinding {
    name: ModuleBindingName,
    cell: BindingCellId,
}

/// Pure in-memory source/synthetic module record; evaluation fields are intentionally absent.
#[derive(Debug)]
pub(crate) struct ModuleRecord {
    id: ModuleId,
    specifier: ModuleIdentity,
    requested_modules: Box<[ModuleIdentity]>,
    imports: Box<[ImportEntry]>,
    exports: Box<[ExportEntry]>,
    local_bindings: Box<[LocalBinding]>,
    status: ModuleStatus,
    evaluation: ModuleEvaluationState,
    body: ModuleBody,
    has_top_level_await: bool,
    namespace: Option<Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModuleEvaluationState {
    Unevaluated,
    Evaluating,
    Evaluated(Value),
    Errored(Value),
}

/// Executable content supplied by a host loader after canonical identity resolution.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum ModuleBody {
    Synthetic,
    Precompiled(CompiledModule),
}

impl ModuleRecord {
    #[cfg(test)]
    pub(crate) const fn status(&self) -> ModuleStatus {
        self.status
    }

    #[cfg(test)]
    pub(crate) fn imports(&self) -> &[ImportEntry] {
        &self.imports
    }
}

/// TDZ-aware storage shared by a local export and every resolved importing alias.
#[derive(Debug, Default)]
pub(crate) struct LiveBindingCell {
    value: Option<Value>,
}

impl LiveBindingCell {
    pub(crate) fn read(&self) -> Result<Value, ModuleError> {
        self.value.ok_or(ModuleError::UninitializedBinding)
    }

    pub(crate) fn write(&mut self, value: Value) {
        self.value = Some(value);
    }
}

impl Trace for LiveBindingCell {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.value.trace(tracer);
    }
}

/// Host hard limits for cold module graph construction and linking work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModuleLimits {
    pub(crate) max_modules: u32,
    pub(crate) max_binding_cells: u32,
    pub(crate) max_edges: u32,
}

impl ModuleLimits {
    #[must_use]
    pub const fn new(max_modules: u32, max_binding_cells: u32, max_edges: u32) -> Self {
        Self {
            max_modules,
            max_binding_cells,
            max_edges,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleError {
    AllocationFailed { collection: &'static str },
    CapacityOverflow { collection: &'static str },
    ModuleLimit { limit: u32 },
    BindingCellLimit { limit: u32 },
    EdgeLimit { limit: u32 },
    DuplicateSpecifier,
    DuplicateRequestedModule,
    DuplicateBinding,
    DuplicateExport,
    UndeclaredModuleRequest,
    MissingLocalBinding,
    UnknownModule(ModuleId),
    MissingModule,
    MissingExport,
    AmbiguousExport,
    ImportedBinding,
    UninitializedBinding,
    UnlinkedImport,
    NamespaceObjectRequired,
    InvalidLinkState,
    EvaluationOrderLimit { limit: u32 },
}

/// Append-only module records and binding cells with deterministic iteration order.
#[derive(Debug)]
pub(crate) struct ModuleGraph {
    records: Vec<ModuleRecord>,
    cells: Vec<LiveBindingCell>,
    limits: ModuleLimits,
    edge_count: usize,
}

#[derive(Clone, Copy)]
struct ModuleGraphCheckpoint {
    records: usize,
    cells: usize,
    edges: usize,
}

impl ModuleGraph {
    const fn checkpoint(&self) -> ModuleGraphCheckpoint {
        ModuleGraphCheckpoint {
            records: self.records.len(),
            cells: self.cells.len(),
            edges: self.edge_count,
        }
    }

    fn rollback(&mut self, checkpoint: ModuleGraphCheckpoint) {
        self.records.truncate(checkpoint.records);
        self.cells.truncate(checkpoint.cells);
        self.edge_count = checkpoint.edges;
    }

    pub(crate) fn try_new(limits: ModuleLimits) -> Result<Self, ModuleError> {
        let mut records = Vec::new();
        records
            .try_reserve_exact(INITIAL_MODULE_CAPACITY.min(limits.max_modules as usize))
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "module records",
            })?;
        let mut cells = Vec::new();
        cells
            .try_reserve_exact(INITIAL_BINDING_CELL_CAPACITY.min(limits.max_binding_cells as usize))
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "module binding cells",
            })?;
        Ok(Self {
            records,
            cells,
            limits,
            edge_count: 0,
        })
    }

    /// Validates a frozen record and publishes all of its stable IDs atomically.
    #[cfg(test)]
    pub(crate) fn insert(&mut self, init: ModuleRecordInit) -> Result<ModuleId, ModuleError> {
        self.insert_with_body(init, ModuleBody::Synthetic)
    }

    /// Validates and publishes a loaded record together with its immutable executable body.
    pub(crate) fn insert_with_body(
        &mut self,
        init: ModuleRecordInit,
        body: ModuleBody,
    ) -> Result<ModuleId, ModuleError> {
        self.validate_record(&init)?;
        if self.records.len() >= self.limits.max_modules as usize {
            return Err(ModuleError::ModuleLimit {
                limit: self.limits.max_modules,
            });
        }
        let added_edges = init
            .requested_modules
            .len()
            .checked_add(init.imports.len())
            .and_then(|count| count.checked_add(init.exports.len()))
            .ok_or(ModuleError::CapacityOverflow {
                collection: "module graph edges",
            })?;
        let next_edge_count =
            self.edge_count
                .checked_add(added_edges)
                .ok_or(ModuleError::CapacityOverflow {
                    collection: "module graph edges",
                })?;
        if next_edge_count > self.limits.max_edges as usize {
            return Err(ModuleError::EdgeLimit {
                limit: self.limits.max_edges,
            });
        }
        let next_cell_count = self
            .cells
            .len()
            .checked_add(init.local_bindings.len())
            .ok_or(ModuleError::CapacityOverflow {
                collection: "module binding cells",
            })?;
        if next_cell_count > self.limits.max_binding_cells as usize {
            return Err(ModuleError::BindingCellLimit {
                limit: self.limits.max_binding_cells,
            });
        }
        self.records
            .try_reserve_exact(1)
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "module records",
            })?;
        self.cells
            .try_reserve_exact(init.local_bindings.len())
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "module binding cells",
            })?;
        let mut local_bindings = Vec::new();
        local_bindings
            .try_reserve_exact(init.local_bindings.len())
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "module local binding index",
            })?;
        for (offset, name) in init.local_bindings.into_vec().into_iter().enumerate() {
            let index =
                self.cells
                    .len()
                    .checked_add(offset)
                    .ok_or(ModuleError::CapacityOverflow {
                        collection: "module binding cell IDs",
                    })?;
            let cell = BindingCellId::from_index(index).ok_or(ModuleError::CapacityOverflow {
                collection: "module binding cell IDs",
            })?;
            local_bindings.push(LocalBinding { name, cell });
        }
        let id = ModuleId::from_index(self.records.len()).ok_or(ModuleError::CapacityOverflow {
            collection: "module record IDs",
        })?;
        self.cells
            .extend((0..local_bindings.len()).map(|_| LiveBindingCell::default()));
        self.records.push(ModuleRecord {
            id,
            specifier: init.specifier,
            requested_modules: init.requested_modules,
            imports: init.imports,
            exports: init.exports,
            local_bindings: local_bindings.into_boxed_slice(),
            status: ModuleStatus::Unlinked,
            evaluation: ModuleEvaluationState::Unevaluated,
            body,
            has_top_level_await: init.has_top_level_await,
            namespace: None,
        });
        self.edge_count = next_edge_count;
        Ok(id)
    }

    /// Checks local uniqueness and request ownership before stable IDs become observable.
    fn validate_record(&self, init: &ModuleRecordInit) -> Result<(), ModuleError> {
        if self
            .records
            .iter()
            .any(|record| record.specifier == init.specifier)
        {
            return Err(ModuleError::DuplicateSpecifier);
        }
        ensure_unique_specifiers(&init.requested_modules)?;
        ensure_unique_bindings(&init.local_bindings)?;
        for (index, import) in init.imports.iter().enumerate() {
            if !contains_request(&init.requested_modules, &import.module_request) {
                return Err(ModuleError::UndeclaredModuleRequest);
            }
            if init
                .local_bindings
                .iter()
                .any(|name| name == &import.local_name)
                || init.imports[..index]
                    .iter()
                    .any(|entry| entry.local_name == import.local_name)
            {
                return Err(ModuleError::DuplicateBinding);
            }
        }
        for (index, export) in init.exports.iter().enumerate() {
            if export.export_name().is_some()
                && init.exports[..index]
                    .iter()
                    .any(|entry| entry.export_name() == export.export_name())
            {
                return Err(ModuleError::DuplicateExport);
            }
            match export {
                ExportEntry::Local { local_name, .. } => {
                    if !init.local_bindings.iter().any(|name| name == local_name) {
                        return Err(ModuleError::MissingLocalBinding);
                    }
                }
                ExportEntry::Indirect { module_request, .. }
                | ExportEntry::Star { module_request }
                    if !contains_request(&init.requested_modules, module_request) =>
                {
                    return Err(ModuleError::UndeclaredModuleRequest);
                }
                ExportEntry::Indirect { .. } | ExportEntry::Star { .. } => {}
            }
        }
        Ok(())
    }

    pub(crate) fn record(&self, id: ModuleId) -> Result<&ModuleRecord, ModuleError> {
        self.records
            .get(id.index())
            .ok_or(ModuleError::UnknownModule(id))
    }

    /// Updates local cell storage while preserving imported bindings as read-only aliases.
    pub(crate) fn write_binding(
        &mut self,
        module: ModuleId,
        name: &str,
        value: Value,
    ) -> Result<(), ModuleError> {
        let record = self.record(module)?;
        let cell = record
            .local_bindings
            .iter()
            .find(|binding| binding.name.as_str() == name)
            .map(|binding| binding.cell);
        if cell.is_none()
            && record
                .imports
                .iter()
                .any(|entry| entry.local_name.as_str() == name)
        {
            return Err(ModuleError::ImportedBinding);
        }
        let cell = cell.ok_or(ModuleError::MissingLocalBinding)?;
        self.cells[cell.index()].write(value);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn read_binding(&self, module: ModuleId, name: &str) -> Result<Value, ModuleError> {
        match self.binding_target(module, name)? {
            ModuleBindingTarget::Cell(cell) => self.cells[cell.index()].read(),
            ModuleBindingTarget::Namespace(_) => Err(ModuleError::NamespaceObjectRequired),
        }
    }

    /// Resolves a local/import alias without forcing namespace object allocation in the graph.
    fn binding_target(
        &self,
        module: ModuleId,
        name: &str,
    ) -> Result<ModuleBindingTarget, ModuleError> {
        let record = self.record(module)?;
        let local = record
            .local_bindings
            .iter()
            .find(|binding| binding.name.as_str() == name)
            .map(|binding| ModuleBindingTarget::Cell(binding.cell));
        let imported = record
            .imports
            .iter()
            .find(|entry| entry.local_name.as_str() == name)
            .map(|entry| entry.resolved.as_ref().ok_or(ModuleError::UnlinkedImport))
            .transpose()?;
        let imported = imported
            .map(|resolved| match &resolved.binding {
                ResolvedBindingName::Local(name) => self
                    .record(resolved.module)?
                    .local_bindings
                    .iter()
                    .find(|binding| binding.name == *name)
                    .map(|binding| ModuleBindingTarget::Cell(binding.cell))
                    .ok_or(ModuleError::MissingLocalBinding),
                ResolvedBindingName::Namespace => {
                    Ok(ModuleBindingTarget::Namespace(resolved.module))
                }
            })
            .transpose()?;
        local.or(imported).ok_or(ModuleError::MissingLocalBinding)
    }
}

impl Trace for ModuleGraph {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for cell in &mut self.cells {
            cell.trace(tracer);
        }
        for record in &mut self.records {
            record.namespace.trace(tracer);
            match &mut record.evaluation {
                ModuleEvaluationState::Evaluated(value) | ModuleEvaluationState::Errored(value) => {
                    value.trace(tracer)
                }
                ModuleEvaluationState::Unevaluated | ModuleEvaluationState::Evaluating => {}
            }
        }
    }
}

fn contains_request(requests: &[ModuleIdentity], request: &ModuleIdentity) -> bool {
    requests.iter().any(|candidate| candidate == request)
}

fn ensure_unique_specifiers(requests: &[ModuleIdentity]) -> Result<(), ModuleError> {
    for (index, request) in requests.iter().enumerate() {
        if requests[..index].iter().any(|existing| existing == request) {
            return Err(ModuleError::DuplicateRequestedModule);
        }
    }
    Ok(())
}

fn ensure_unique_bindings(bindings: &[ModuleBindingName]) -> Result<(), ModuleError> {
    for (index, binding) in bindings.iter().enumerate() {
        if bindings[..index].iter().any(|existing| existing == binding) {
            return Err(ModuleError::DuplicateBinding);
        }
    }
    Ok(())
}
