//! Module namespace exotic payload and live-binding materialization.

use core::mem::size_of;

use tachyon_gc::{GcExternalMemory, Trace, Tracer};

use super::{BindingCellId, ModuleError, ModuleGraph, ModuleId, ResolvedBindingName};
use crate::tuning::modules::NAMESPACE_LINEAR_LOOKUP_LIMIT;
use crate::{
    AccessorPropertyDescriptor, AllocationSpace, AtomId, DataPropertyDescriptor, ExecutionError,
    Immediate, Isolate, JsString, OrdinaryObject, PropertyAttributes, PropertyDescriptor,
    PropertyKey, ShapeId, Value, VmRoots,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NamespaceBinding {
    Cell(BindingCellId),
    Namespace(ModuleId),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct NamespaceExport {
    pub(super) key: AtomId,
    pub(super) binding: NamespaceBinding,
}

/// GC-managed namespace identity; export values remain owned by module live-binding cells.
#[derive(Debug)]
pub(crate) struct ModuleNamespaceObject {
    #[allow(
        dead_code,
        reason = "the specification-visible module identity is retained for future import reflection"
    )]
    module: ModuleId,
    exports: Box<[NamespaceExport]>,
    lookup: Box<[u32]>,
    pub(crate) ordinary: OrdinaryObject,
}

impl Trace for ModuleNamespaceObject {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.ordinary.trace(tracer);
    }
}

impl GcExternalMemory for ModuleNamespaceObject {
    #[inline(always)]
    fn external_memory_bytes(&self) -> usize {
        self.exports.len() * size_of::<NamespaceExport>() + self.lookup.len() * size_of::<u32>()
    }
}

struct NamespaceAllocationRoots<'a> {
    vm: VmRoots<'a>,
    to_string_tag: Value,
}

impl Trace for NamespaceAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.to_string_tag.trace(tracer);
    }
}

impl ModuleGraph {
    pub(super) fn cached_namespace(&self, module: ModuleId) -> Result<Option<Value>, ModuleError> {
        Ok(self.record(module)?.namespace)
    }

    pub(super) fn cache_namespace(
        &mut self,
        module: ModuleId,
        namespace: Value,
    ) -> Result<(), ModuleError> {
        let record = self
            .records
            .get_mut(module.index())
            .ok_or(ModuleError::UnknownModule(module))?;
        record.namespace = Some(namespace);
        Ok(())
    }

    pub(super) fn namespace_binding(
        &self,
        resolution: &super::ResolvedBinding,
    ) -> Result<NamespaceBinding, ModuleError> {
        match &resolution.binding {
            ResolvedBindingName::Namespace => Ok(NamespaceBinding::Namespace(resolution.module)),
            ResolvedBindingName::Local(name) => self
                .record(resolution.module)?
                .local_bindings
                .iter()
                .find(|binding| binding.name == *name)
                .map(|binding| NamespaceBinding::Cell(binding.cell))
                .ok_or(ModuleError::MissingLocalBinding),
        }
    }

    pub(crate) fn read_namespace_cell(&self, cell: BindingCellId) -> Result<Value, ModuleError> {
        self.cells
            .get(cell.index())
            .ok_or(ModuleError::MissingLocalBinding)?
            .read()
    }
}

impl Isolate {
    /// Implements GetModuleNamespace with one cached exotic object per linked module record.
    pub(crate) fn get_module_namespace(
        &mut self,
        module: ModuleId,
    ) -> Result<Value, ExecutionError> {
        if let Some(namespace) = self
            .module_graph
            .cached_namespace(module)
            .map_err(ExecutionError::Module)?
        {
            return Ok(namespace);
        }
        let resolutions = self
            .module_graph
            .namespace_resolutions(module)
            .map_err(ExecutionError::Module)?;
        let checkpoint = self.atoms.checkpoint();
        let result = self.create_module_namespace(module, resolutions);
        if result.is_err() {
            self.atoms.rollback(checkpoint);
        }
        result
    }

    /// Publishes export atoms and the GC payload only after all fallible planning succeeds.
    fn create_module_namespace(
        &mut self,
        module: ModuleId,
        resolutions: Vec<super::link::NamespaceResolution>,
    ) -> Result<Value, ExecutionError> {
        let mut exports = Vec::new();
        exports.try_reserve_exact(resolutions.len()).map_err(|_| {
            ExecutionError::Module(ModuleError::AllocationFailed {
                collection: "module namespace exports",
            })
        })?;
        for resolution in resolutions {
            let name = JsString::try_from_utf16(resolution.name.as_utf16())
                .map_err(ExecutionError::PropertyKeyString)?;
            let key = self
                .atoms
                .try_intern(name)
                .map_err(ExecutionError::PropertyKeyAtom)?;
            let binding = self
                .module_graph
                .namespace_binding(&resolution.binding)
                .map_err(ExecutionError::Module)?;
            exports.push(NamespaceExport { key, binding });
        }
        let mut lookup = Vec::new();
        if exports.len() > NAMESPACE_LINEAR_LOOKUP_LIMIT {
            lookup.try_reserve_exact(exports.len()).map_err(|_| {
                ExecutionError::Module(ModuleError::AllocationFailed {
                    collection: "module namespace lookup",
                })
            })?;
            for index in 0..exports.len() {
                lookup.push(u32::try_from(index).map_err(|_| {
                    ExecutionError::Module(ModuleError::CapacityOverflow {
                        collection: "module namespace lookup",
                    })
                })?);
            }
            lookup.sort_unstable_by_key(|index| exports[*index as usize].key.index());
        }
        let to_string_tag = self.allocate_runtime_string(
            JsString::try_from_latin1(b"Module").map_err(ExecutionError::PropertyKeyString)?,
        )?;
        let (namespace, to_string_tag) = {
            let mut roots = NamespaceAllocationRoots {
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
                to_string_tag,
            };
            let namespace = self
                .heap
                .try_allocate_external_with_gc(
                    self.types.module_namespace_object,
                    0,
                    ModuleNamespaceObject {
                        module,
                        exports: exports.into_boxed_slice(),
                        lookup: lookup.into_boxed_slice(),
                        ordinary: OrdinaryObject {
                            shape: ShapeId::EMPTY,
                            extensible: true,
                            storage: None,
                            prototype: Value::from_immediate(Immediate::Null),
                        },
                    },
                    AllocationSpace::Young,
                    &mut roots,
                )
                .map_err(ExecutionError::HeapAllocation)?;
            (Value::from_heap_ref(namespace.raw()), roots.to_string_tag)
        };
        let tag_key = self.module_namespace_to_string_tag_key()?;
        self.define_fresh_data_property(
            namespace,
            tag_key,
            to_string_tag,
            PropertyAttributes::data(false, false, true),
        )?;
        let (receiver, _) = self.object_snapshot(namespace)?;
        self.set_object_extensible(receiver, false)?;
        self.module_graph
            .cache_namespace(module, namespace)
            .map_err(ExecutionError::Module)?;
        Ok(namespace)
    }

    #[inline(always)]
    pub(crate) fn is_module_namespace_value(&self, value: Value) -> bool {
        value.as_heap_ref().is_some_and(|raw| {
            self.heap
                .checked_reference(raw, self.types.module_namespace_object)
                .is_ok()
        })
    }

    pub(crate) fn module_namespace_ordinary(
        &mut self,
        value: Value,
    ) -> Result<OrdinaryObject, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let namespace = self
            .heap
            .checked_reference(raw, self.types.module_namespace_object)
            .map_err(ExecutionError::HeapReference)?;
        self.heap.with_running_scope(|scope| {
            let namespace = scope.root(namespace).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(namespace, self.types.module_namespace_object)
                    .map(|namespace| namespace.ordinary)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Reads one namespace export without freezing its current live-binding value.
    pub(crate) fn module_namespace_property(
        &mut self,
        value: Value,
        key: PropertyKey,
    ) -> Result<Option<Value>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let namespace = self
            .heap
            .checked_reference(raw, self.types.module_namespace_object)
            .map_err(ExecutionError::HeapReference)?;
        let binding = self.heap.with_running_scope(|scope| {
            let namespace = scope.root(namespace).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let namespace = no_gc
                    .borrow(namespace, self.types.module_namespace_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let binding = key
                    .atom()
                    .and_then(|atom| namespace.export(atom).map(|export| export.binding));
                Ok::<_, ExecutionError>(binding)
            })
        })?;
        match binding {
            Some(NamespaceBinding::Cell(cell)) => self
                .module_graph
                .read_namespace_cell(cell)
                .map(Some)
                .map_err(ExecutionError::Module),
            Some(NamespaceBinding::Namespace(module)) => {
                self.get_module_namespace(module).map(Some)
            }
            None => Ok(None),
        }
    }

    pub(crate) fn module_namespace_has_export(
        &mut self,
        value: Value,
        key: PropertyKey,
    ) -> Result<bool, ExecutionError> {
        let Some(atom) = key.atom() else {
            return Ok(false);
        };
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let namespace = self
            .heap
            .checked_reference(raw, self.types.module_namespace_object)
            .map_err(ExecutionError::HeapReference)?;
        self.heap.with_running_scope(|scope| {
            let namespace = scope.root(namespace).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(namespace, self.types.module_namespace_object)
                    .map(|namespace| namespace.export(atom).is_some())
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(crate) fn module_namespace_property_descriptor(
        &mut self,
        value: Value,
        key: PropertyKey,
    ) -> Result<Option<PropertyDescriptor>, ExecutionError> {
        let Some(property_value) = self.module_namespace_property(value, key)? else {
            return Ok(None);
        };
        Ok(Some(PropertyDescriptor::Data(DataPropertyDescriptor {
            value: Some(property_value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(false),
        })))
    }

    /// Implements the namespace DefineOwnProperty compatibility checks without mutation.
    pub(crate) fn define_module_namespace_property(
        &mut self,
        value: Value,
        key: PropertyKey,
        descriptor: PropertyDescriptor,
    ) -> Result<bool, ExecutionError> {
        let Some(current) = self.module_namespace_property(value, key)? else {
            return Ok(false);
        };
        match descriptor {
            PropertyDescriptor::Accessor(AccessorPropertyDescriptor { .. }) => Ok(false),
            PropertyDescriptor::Generic(descriptor) => {
                Ok(descriptor.enumerable != Some(false) && descriptor.configurable != Some(true))
            }
            PropertyDescriptor::Data(descriptor) => {
                if descriptor.writable == Some(false)
                    || descriptor.enumerable == Some(false)
                    || descriptor.configurable == Some(true)
                {
                    return Ok(false);
                }
                if let Some(requested) = descriptor.value {
                    return self.same_value(requested, current);
                }
                Ok(true)
            }
        }
    }

    pub(crate) fn module_namespace_export_keys(
        &mut self,
        value: Value,
    ) -> Result<Vec<AtomId>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let namespace = self
            .heap
            .checked_reference(raw, self.types.module_namespace_object)
            .map_err(ExecutionError::HeapReference)?;
        self.heap.with_running_scope(|scope| {
            let namespace = scope.root(namespace).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let namespace = no_gc
                    .borrow(namespace, self.types.module_namespace_object)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let mut keys = Vec::new();
                keys.try_reserve_exact(namespace.exports.len())
                    .map_err(|_| {
                        ExecutionError::Module(ModuleError::AllocationFailed {
                            collection: "module namespace own keys",
                        })
                    })?;
                keys.extend(namespace.exports.iter().map(|export| export.key));
                Ok(keys)
            })
        })
    }

    pub(crate) fn module_namespace_to_string_tag_key(
        &mut self,
    ) -> Result<PropertyKey, ExecutionError> {
        self.property_key(
            self.realm
                .well_known_symbols
                .to_string_tag
                .expect("Symbol.toStringTag initializes before module evaluation"),
        )
    }
}

impl ModuleNamespaceObject {
    /// Keeps tiny namespaces linear and uses an immutable atom index for larger hot reads.
    #[inline(always)]
    fn export(&self, key: AtomId) -> Option<&NamespaceExport> {
        if self.lookup.is_empty() {
            return self.exports.iter().find(|export| export.key == key);
        }
        let position = self
            .lookup
            .binary_search_by_key(&key.index(), |index| {
                self.exports[*index as usize].key.index()
            })
            .ok()?;
        self.lookup
            .get(position)
            .and_then(|index| self.exports.get(*index as usize))
    }
}
