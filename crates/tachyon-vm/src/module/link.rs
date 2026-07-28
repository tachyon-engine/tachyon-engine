//! Iterative cyclic-module linking and named export resolution.

use super::*;

#[derive(Debug)]
struct LinkFrame {
    module: ModuleId,
    next_request: usize,
    pending_child: Option<ModuleId>,
}

/// Deterministic SCC completion order for diagnostics and later instantiation scheduling.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct LinkReport {
    components: Box<[Box<[ModuleId]>]>,
}

impl LinkReport {
    #[cfg(test)]
    pub(crate) fn components(&self) -> &[Box<[ModuleId]>] {
        &self.components
    }
}

impl ModuleGraph {
    /// Links one root with iterative Tarjan DFS and transactionally restores incomplete records.
    pub(crate) fn link(&mut self, root: ModuleId) -> Result<LinkReport, ModuleError> {
        match self.record(root)?.status {
            ModuleStatus::Linked { .. } => {
                return Ok(LinkReport {
                    components: Box::new([]),
                });
            }
            ModuleStatus::Linking { .. } => return Err(ModuleError::InvalidLinkState),
            ModuleStatus::Unlinked => {}
        }

        let max_work = self.records.len();
        let max_export_work = self.edge_count.max(1);
        let mut frames = try_work_vec(max_work, "module link frames")?;
        let mut component_stack = try_work_vec(max_work, "module SCC stack")?;
        let mut touched = try_work_vec(max_work, "module link rollback")?;
        let mut components = try_work_vec(max_work, "module link components")?;
        let mut export_visits = try_work_vec(max_work, "module export visits")?;
        let mut import_cells = try_work_vec(max_work, "module import cells")?;
        let mut next_index = 0u32;

        let result = (|| {
            self.enter_module(
                root,
                &mut next_index,
                &mut frames,
                &mut component_stack,
                &mut touched,
                max_work,
            )?;
            while !frames.is_empty() {
                let frame_index = frames.len() - 1;
                let module = frames[frame_index].module;
                if let Some(child) = frames[frame_index].pending_child.take() {
                    if let ModuleStatus::Linking { ancestor_index, .. } = self.record(child)?.status
                    {
                        self.lower_ancestor(module, ancestor_index)?;
                    }
                    continue;
                }

                let request_index = frames[frame_index].next_request;
                if request_index < self.record(module)?.requested_modules.len() {
                    frames[frame_index].next_request = request_index + 1;
                    let child = self.requested_module(module, request_index)?;
                    match self.record(child)?.status {
                        ModuleStatus::Unlinked => {
                            frames[frame_index].pending_child = Some(child);
                            self.enter_module(
                                child,
                                &mut next_index,
                                &mut frames,
                                &mut component_stack,
                                &mut touched,
                                max_work,
                            )?;
                        }
                        ModuleStatus::Linking { ancestor_index, .. } => {
                            self.lower_ancestor(module, ancestor_index)?;
                        }
                        ModuleStatus::Linked { .. } => {}
                    }
                    continue;
                }

                self.resolve_imports(
                    module,
                    &mut export_visits,
                    &mut import_cells,
                    max_export_work,
                )?;
                let (dfs_index, ancestor_index) = match self.record(module)?.status {
                    ModuleStatus::Linking {
                        dfs_index,
                        ancestor_index,
                    } => (dfs_index, ancestor_index),
                    _ => return Err(ModuleError::InvalidLinkState),
                };
                frames.pop();
                if dfs_index == ancestor_index {
                    self.complete_component(
                        module,
                        &mut component_stack,
                        &mut components,
                        max_work,
                    )?;
                }
            }
            Ok(())
        })();

        if let Err(error) = result {
            self.rollback_link(&touched);
            return Err(error);
        }
        Ok(LinkReport {
            components: components.into_boxed_slice(),
        })
    }

    fn enter_module(
        &mut self,
        module: ModuleId,
        next_index: &mut u32,
        frames: &mut Vec<LinkFrame>,
        component_stack: &mut Vec<ModuleId>,
        touched: &mut Vec<ModuleId>,
        max_work: usize,
    ) -> Result<(), ModuleError> {
        if self.record(module)?.status != ModuleStatus::Unlinked {
            return Err(ModuleError::InvalidLinkState);
        }
        let dfs_index = *next_index;
        *next_index = next_index
            .checked_add(1)
            .ok_or(ModuleError::CapacityOverflow {
                collection: "module DFS index",
            })?;
        reserve_work_push(frames, max_work, "module link frames")?;
        reserve_work_push(component_stack, max_work, "module SCC stack")?;
        reserve_work_push(touched, max_work, "module link rollback")?;
        self.records[module.index()].status = ModuleStatus::Linking {
            dfs_index,
            ancestor_index: dfs_index,
        };
        frames.push(LinkFrame {
            module,
            next_request: 0,
            pending_child: None,
        });
        component_stack.push(module);
        touched.push(module);
        Ok(())
    }

    fn lower_ancestor(&mut self, module: ModuleId, child_ancestor: u32) -> Result<(), ModuleError> {
        let ModuleStatus::Linking { ancestor_index, .. } = &mut self.records[module.index()].status
        else {
            return Err(ModuleError::InvalidLinkState);
        };
        *ancestor_index = (*ancestor_index).min(child_ancestor);
        Ok(())
    }

    /// Resolves every import before publishing an SCC as linked, so aliases are all-or-nothing.
    fn resolve_imports(
        &mut self,
        module: ModuleId,
        export_visits: &mut Vec<(ModuleId, usize)>,
        import_cells: &mut Vec<BindingCellId>,
        max_export_work: usize,
    ) -> Result<(), ModuleError> {
        import_cells.clear();
        let import_count = self.record(module)?.imports.len();
        if import_cells.capacity() < import_count {
            import_cells
                .try_reserve_exact(import_count - import_cells.capacity())
                .map_err(|_| ModuleError::AllocationFailed {
                    collection: "module import resolutions",
                })?;
        }
        for import_index in 0..import_count {
            let record = self.record(module)?;
            let import = &record.imports[import_index];
            let requested = self.module_by_specifier(&import.module_request)?;
            export_visits.clear();
            let cell = self.resolve_export(
                requested,
                import.import_name.as_str(),
                export_visits,
                max_export_work,
            )?;
            import_cells.push(cell);
        }
        for (import, cell) in self.records[module.index()]
            .imports
            .iter_mut()
            .zip(import_cells.iter().copied())
        {
            import.resolved_cell = Some(cell);
        }
        Ok(())
    }

    /// Follows local-import and indirect-export aliases iteratively without trusting graph depth.
    fn resolve_export<'a>(
        &'a self,
        mut module: ModuleId,
        mut name: &'a str,
        visits: &mut Vec<(ModuleId, usize)>,
        max_work: usize,
    ) -> Result<BindingCellId, ModuleError> {
        loop {
            let record = self.record(module)?;
            let export_index = record
                .exports
                .iter()
                .position(|entry| entry.export_name().as_str() == name)
                .ok_or(ModuleError::MissingExport)?;
            if visits.contains(&(module, export_index)) {
                return Err(ModuleError::CircularExport);
            }
            reserve_work_push(visits, max_work, "module export visits")?;
            visits.push((module, export_index));
            match &record.exports[export_index] {
                ExportEntry::Local { local_name, .. } => {
                    if let Some(local) = record
                        .local_bindings
                        .iter()
                        .find(|binding| binding.name == *local_name)
                    {
                        return Ok(local.cell);
                    }
                    let import = record
                        .imports
                        .iter()
                        .find(|entry| entry.local_name == *local_name)
                        .ok_or(ModuleError::MissingLocalBinding)?;
                    module = self.module_by_specifier(&import.module_request)?;
                    name = import.import_name.as_str();
                }
                ExportEntry::Indirect {
                    module_request,
                    import_name,
                    ..
                } => {
                    module = self.module_by_specifier(module_request)?;
                    name = import_name.as_str();
                }
            }
        }
    }

    fn requested_module(
        &self,
        module: ModuleId,
        request_index: usize,
    ) -> Result<ModuleId, ModuleError> {
        let request = self
            .record(module)?
            .requested_modules
            .get(request_index)
            .ok_or(ModuleError::MissingModule)?;
        self.module_by_specifier(request)
    }

    fn module_by_specifier(&self, specifier: &ModuleSpecifier) -> Result<ModuleId, ModuleError> {
        self.records
            .iter()
            .find(|record| record.specifier == *specifier)
            .map(|record| record.id)
            .ok_or(ModuleError::MissingModule)
    }

    /// Pops one complete SCC in stack order and publishes a shared cycle root.
    fn complete_component(
        &mut self,
        root: ModuleId,
        component_stack: &mut Vec<ModuleId>,
        components: &mut Vec<Box<[ModuleId]>>,
        max_work: usize,
    ) -> Result<(), ModuleError> {
        let root_position = component_stack
            .iter()
            .rposition(|module| *module == root)
            .ok_or(ModuleError::InvalidLinkState)?;
        let member_count = component_stack.len() - root_position;
        let mut members = Vec::new();
        members
            .try_reserve_exact(member_count)
            .map_err(|_| ModuleError::AllocationFailed {
                collection: "module SCC members",
            })?;
        while component_stack.len() > root_position {
            let member = component_stack.pop().ok_or(ModuleError::InvalidLinkState)?;
            self.records[member.index()].status = ModuleStatus::Linked { cycle_root: root };
            members.push(member);
        }
        reserve_work_push(components, max_work, "module link components")?;
        components.push(members.into_boxed_slice());
        Ok(())
    }

    fn rollback_link(&mut self, touched: &[ModuleId]) {
        for module in touched.iter().copied() {
            let record = &mut self.records[module.index()];
            if matches!(record.status, ModuleStatus::Linking { .. }) {
                record.status = ModuleStatus::Unlinked;
                for import in &mut record.imports {
                    import.resolved_cell = None;
                }
            }
        }
    }
}

fn try_work_vec<T>(max_work: usize, collection: &'static str) -> Result<Vec<T>, ModuleError> {
    let mut work = Vec::new();
    work.try_reserve_exact(INITIAL_LINK_WORK_CAPACITY.min(max_work))
        .map_err(|_| ModuleError::AllocationFailed { collection })?;
    Ok(work)
}

/// Grows cold link scratch in named chunks while respecting the exact graph-size ceiling.
fn reserve_work_push<T>(
    work: &mut Vec<T>,
    max_work: usize,
    collection: &'static str,
) -> Result<(), ModuleError> {
    if work.len() >= max_work {
        return Err(ModuleError::CapacityOverflow { collection });
    }
    if work.len() == work.capacity() {
        let remaining = max_work - work.len();
        work.try_reserve_exact(INITIAL_LINK_WORK_CAPACITY.min(remaining))
            .map_err(|_| ModuleError::AllocationFailed { collection })?;
    }
    Ok(())
}
