use std::sync::Arc;

#[cfg(test)]
mod tests;

/// Index of a deduplicated request in `ModuleStencil::requested_modules`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct ModuleRequestId(u32);

impl ModuleRequestId {
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// One import attribute, preserving exact ECMAScript string code units.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleAttribute {
    pub key: Arc<[u16]>,
    pub value: Arc<[u16]>,
}

/// A source-ordered, deduplicated module request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleRequest {
    pub specifier: Arc<[u16]>,
    pub attributes: Arc<[ModuleAttribute]>,
}

/// The requested export binding, with namespace imports kept distinct from the string `"*"`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleImportName {
    Name(Arc<[u16]>),
    Namespace,
}

/// One local binding introduced by an import declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleImportEntry {
    pub module_request: ModuleRequestId,
    pub import_name: ModuleImportName,
    pub local_name: Arc<str>,
}

/// A normalized export entry following ParseModule's local/indirect/star partition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleExportEntry {
    Local {
        export_name: Arc<[u16]>,
        local_name: Arc<str>,
    },
    Indirect {
        export_name: Arc<[u16]>,
        module_request: ModuleRequestId,
        import_name: ModuleImportName,
    },
    Star {
        module_request: ModuleRequestId,
    },
}

/// Immutable static module semantics retained after the frontend arena is released.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleStencil {
    requested_modules: Arc<[ModuleRequest]>,
    imports: Arc<[ModuleImportEntry]>,
    exports: Arc<[ModuleExportEntry]>,
    local_bindings: Arc<[Arc<str>]>,
    has_top_level_await: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleStencilError {
    TooManyRequests {
        count: usize,
    },
    DuplicateRequest {
        first: u32,
        duplicate: u32,
    },
    DuplicateAttributeKey {
        request: u32,
        attribute: u32,
    },
    RequestOutOfRange {
        request: ModuleRequestId,
        count: u32,
    },
    EmptyLocalName,
    DuplicateImportLocalName {
        name: Arc<str>,
    },
    DuplicateLocalBinding {
        name: Arc<str>,
    },
    DuplicateExportName {
        name: Arc<[u16]>,
    },
}

impl ModuleStencil {
    /// Validates cross-table references and early-error invariants before freezing module metadata.
    pub fn new(
        mut requested_modules: Vec<ModuleRequest>,
        imports: Vec<ModuleImportEntry>,
        exports: Vec<ModuleExportEntry>,
        local_bindings: Vec<Arc<str>>,
        has_top_level_await: bool,
    ) -> Result<Self, ModuleStencilError> {
        for request in &mut requested_modules {
            let mut attributes = request.attributes.to_vec();
            attributes.sort_unstable_by(|left, right| left.key.cmp(&right.key));
            request.attributes = attributes.into();
        }
        validate_requests(&requested_modules)?;
        let request_count = u32::try_from(requested_modules.len()).map_err(|_| {
            ModuleStencilError::TooManyRequests {
                count: requested_modules.len(),
            }
        })?;
        validate_imports(&imports, request_count)?;
        validate_exports(&exports, request_count)?;
        validate_local_bindings(&local_bindings)?;
        Ok(Self {
            requested_modules: requested_modules.into(),
            imports: imports.into(),
            exports: exports.into(),
            local_bindings: local_bindings.into(),
            has_top_level_await,
        })
    }

    #[must_use]
    pub fn requested_modules(&self) -> &[ModuleRequest] {
        &self.requested_modules
    }

    #[must_use]
    pub fn imports(&self) -> &[ModuleImportEntry] {
        &self.imports
    }

    #[must_use]
    pub fn exports(&self) -> &[ModuleExportEntry] {
        &self.exports
    }

    #[must_use]
    pub fn local_bindings(&self) -> &[Arc<str>] {
        &self.local_bindings
    }

    #[must_use]
    pub const fn has_top_level_await(&self) -> bool {
        self.has_top_level_await
    }
}

/// Rejects duplicate requests and attribute keys so request identity is unambiguous at runtime.
fn validate_requests(requests: &[ModuleRequest]) -> Result<(), ModuleStencilError> {
    for (index, request) in requests.iter().enumerate() {
        for attribute in 0..request.attributes.len() {
            if request.attributes[..attribute]
                .iter()
                .any(|previous| previous.key == request.attributes[attribute].key)
            {
                return Err(ModuleStencilError::DuplicateAttributeKey {
                    request: index as u32,
                    attribute: attribute as u32,
                });
            }
        }
        if let Some(first) = requests[..index]
            .iter()
            .position(|previous| previous == request)
        {
            return Err(ModuleStencilError::DuplicateRequest {
                first: first as u32,
                duplicate: index as u32,
            });
        }
    }
    Ok(())
}

/// Checks import request indices and uniqueness of the bindings they introduce.
fn validate_imports(
    imports: &[ModuleImportEntry],
    request_count: u32,
) -> Result<(), ModuleStencilError> {
    for (index, entry) in imports.iter().enumerate() {
        validate_request_id(entry.module_request, request_count)?;
        if entry.local_name.is_empty() {
            return Err(ModuleStencilError::EmptyLocalName);
        }
        if imports[..index]
            .iter()
            .any(|previous| previous.local_name == entry.local_name)
        {
            return Err(ModuleStencilError::DuplicateImportLocalName {
                name: entry.local_name.clone(),
            });
        }
    }
    Ok(())
}

/// Checks normalized exports without conflating star exports with named exports.
fn validate_exports(
    exports: &[ModuleExportEntry],
    request_count: u32,
) -> Result<(), ModuleStencilError> {
    for (index, entry) in exports.iter().enumerate() {
        if let ModuleExportEntry::Indirect { module_request, .. }
        | ModuleExportEntry::Star { module_request } = entry
        {
            validate_request_id(*module_request, request_count)?;
        }
        let Some(name) = export_name(entry) else {
            continue;
        };
        if exports[..index]
            .iter()
            .filter_map(export_name)
            .any(|previous| previous == name)
        {
            return Err(ModuleStencilError::DuplicateExportName { name: name.clone() });
        }
    }
    Ok(())
}

fn export_name(entry: &ModuleExportEntry) -> Option<&Arc<[u16]>> {
    match entry {
        ModuleExportEntry::Local { export_name, .. }
        | ModuleExportEntry::Indirect { export_name, .. } => Some(export_name),
        ModuleExportEntry::Star { .. } => None,
    }
}

/// Ensures declarations do not create two environment slots for one lexical name.
fn validate_local_bindings(bindings: &[Arc<str>]) -> Result<(), ModuleStencilError> {
    for (index, name) in bindings.iter().enumerate() {
        if name.is_empty() {
            return Err(ModuleStencilError::EmptyLocalName);
        }
        if bindings[..index].contains(name) {
            return Err(ModuleStencilError::DuplicateLocalBinding { name: name.clone() });
        }
    }
    Ok(())
}

fn validate_request_id(
    request: ModuleRequestId,
    request_count: u32,
) -> Result<(), ModuleStencilError> {
    if request.index() >= request_count {
        return Err(ModuleStencilError::RequestOutOfRange {
            request,
            count: request_count,
        });
    }
    Ok(())
}
