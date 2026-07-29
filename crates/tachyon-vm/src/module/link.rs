//! Iterative cyclic-module linking and named export resolution.

use super::*;

#[derive(Debug)]
struct LinkFrame {
    module: ModuleId,
    next_request: usize,
    pending_child: Option<ModuleId>,
}

#[derive(Debug)]
struct ResolveFrame {
    module: ModuleId,
    name: ModuleExportName,
    state: ResolveFrameState,
}

#[derive(Debug)]
enum ResolveFrameState {
    Enter,
    AwaitIndirect,
    Stars {
        next_export: usize,
        candidate: Option<ResolvedBinding>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ResolveOutcome {
    Found(ResolvedBinding),
    NotFound,
    Ambiguous,
}

#[derive(Clone, Debug)]
pub(super) struct NamespaceResolution {
    pub(super) name: ModuleExportName,
    pub(super) binding: ResolvedBinding,
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
        let max_export_work = self
            .records
            .len()
            .checked_add(self.edge_count)
            .ok_or(ModuleError::CapacityOverflow {
                collection: "module export resolution work",
            })?
            .max(1);
        let mut frames = try_work_vec(max_work, "module link frames")?;
        let mut component_stack = try_work_vec(max_work, "module SCC stack")?;
        let mut touched = try_work_vec(max_work, "module link rollback")?;
        let mut components = try_work_vec(max_work, "module link components")?;
        let mut export_visits = try_work_vec(max_export_work, "module export visits")?;
        let mut export_frames = try_work_vec(max_export_work, "module export frames")?;
        let mut import_resolutions = try_work_vec(max_work, "module import resolutions")?;
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
                    &mut export_frames,
                    &mut import_resolutions,
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
        export_visits: &mut Vec<(ModuleId, ModuleExportName)>,
        export_frames: &mut Vec<ResolveFrame>,
        import_resolutions: &mut Vec<ResolvedBinding>,
        max_export_work: usize,
    ) -> Result<(), ModuleError> {
        import_resolutions.clear();
        let import_count = self.record(module)?.imports.len();
        if import_resolutions.capacity() < import_count {
            import_resolutions
                .try_reserve_exact(import_count - import_resolutions.capacity())
                .map_err(|_| ModuleError::AllocationFailed {
                    collection: "module import resolutions",
                })?;
        }
        for import_index in 0..import_count {
            let record = self.record(module)?;
            let import = &record.imports[import_index];
            let requested = self.module_by_specifier(&import.module_request)?;
            let resolution = match &import.import_name {
                ModuleImportName::Namespace => ResolvedBinding {
                    module: requested,
                    binding: ResolvedBindingName::Namespace,
                },
                ModuleImportName::Name(name) => {
                    match self.resolve_export(
                        requested,
                        name,
                        export_visits,
                        export_frames,
                        max_export_work,
                    )? {
                        ResolveOutcome::Found(resolution) => resolution,
                        ResolveOutcome::NotFound => return Err(ModuleError::MissingExport),
                        ResolveOutcome::Ambiguous => return Err(ModuleError::AmbiguousExport),
                    }
                }
            };
            import_resolutions.push(resolution);
        }
        for (import, resolution) in self.records[module.index()]
            .imports
            .iter_mut()
            .zip(import_resolutions.iter().cloned())
        {
            import.resolved = Some(resolution);
        }
        Ok(())
    }

    /// Implements ResolveExport with explicit frames so star graphs never consume the Rust stack.
    fn resolve_export(
        &self,
        module: ModuleId,
        name: &ModuleExportName,
        visits: &mut Vec<(ModuleId, ModuleExportName)>,
        frames: &mut Vec<ResolveFrame>,
        max_work: usize,
    ) -> Result<ResolveOutcome, ModuleError> {
        visits.clear();
        frames.clear();
        reserve_work_push(frames, max_work, "module export frames")?;
        frames.push(ResolveFrame {
            module,
            name: name.clone(),
            state: ResolveFrameState::Enter,
        });
        let mut completed = None;
        loop {
            if let Some(outcome) = completed.take() {
                frames.pop().ok_or(ModuleError::InvalidLinkState)?;
                let Some(parent) = frames.last_mut() else {
                    return Ok(outcome);
                };
                match &mut parent.state {
                    ResolveFrameState::AwaitIndirect => completed = Some(outcome),
                    ResolveFrameState::Stars { candidate, .. } => match outcome {
                        ResolveOutcome::Found(found) => {
                            if candidate.as_ref().is_some_and(|saved| saved != &found) {
                                completed = Some(ResolveOutcome::Ambiguous);
                            } else {
                                *candidate = Some(found);
                            }
                        }
                        ResolveOutcome::NotFound => {}
                        ResolveOutcome::Ambiguous => completed = Some(ResolveOutcome::Ambiguous),
                    },
                    ResolveFrameState::Enter => return Err(ModuleError::InvalidLinkState),
                }
                continue;
            }

            let frame = frames.last_mut().ok_or(ModuleError::InvalidLinkState)?;
            match &mut frame.state {
                ResolveFrameState::Enter => {
                    let key = (frame.module, frame.name.clone());
                    if visits.contains(&key) {
                        completed = Some(ResolveOutcome::NotFound);
                        continue;
                    }
                    reserve_work_push(visits, max_work, "module export visits")?;
                    visits.push(key);
                    let record = self.record(frame.module)?;
                    if let Some(export) = record
                        .exports
                        .iter()
                        .find(|entry| entry.export_name() == Some(&frame.name))
                    {
                        match export {
                            ExportEntry::Local { local_name, .. } => {
                                completed = Some(ResolveOutcome::Found(ResolvedBinding {
                                    module: frame.module,
                                    binding: ResolvedBindingName::Local(local_name.clone()),
                                }));
                            }
                            ExportEntry::Indirect {
                                module_request,
                                import_name,
                                ..
                            } => {
                                let target = self.module_by_specifier(module_request)?;
                                match import_name {
                                    ModuleImportName::Namespace => {
                                        completed = Some(ResolveOutcome::Found(ResolvedBinding {
                                            module: target,
                                            binding: ResolvedBindingName::Namespace,
                                        }));
                                    }
                                    ModuleImportName::Name(name) => {
                                        frame.state = ResolveFrameState::AwaitIndirect;
                                        push_resolve_frame(frames, target, name.clone(), max_work)?;
                                    }
                                }
                            }
                            ExportEntry::Star { .. } => unreachable!("star export has no name"),
                        }
                        continue;
                    }
                    if frame.name.is_default() {
                        completed = Some(ResolveOutcome::NotFound);
                    } else {
                        frame.state = ResolveFrameState::Stars {
                            next_export: 0,
                            candidate: None,
                        };
                    }
                }
                ResolveFrameState::AwaitIndirect => return Err(ModuleError::InvalidLinkState),
                ResolveFrameState::Stars {
                    next_export,
                    candidate,
                } => {
                    let record = self.record(frame.module)?;
                    let Some((index, request)) = record
                        .exports
                        .iter()
                        .enumerate()
                        .skip(*next_export)
                        .find_map(|(index, export)| match export {
                            ExportEntry::Star { module_request } => Some((index, module_request)),
                            ExportEntry::Local { .. } | ExportEntry::Indirect { .. } => None,
                        })
                    else {
                        completed = Some(
                            candidate
                                .clone()
                                .map_or(ResolveOutcome::NotFound, ResolveOutcome::Found),
                        );
                        continue;
                    };
                    *next_export = index + 1;
                    let target = self.module_by_specifier(request)?;
                    let name = frame.name.clone();
                    push_resolve_frame(frames, target, name, max_work)?;
                }
            }
        }
    }

    /// Collects star-reachable names and freezes each unambiguous resolution for one namespace.
    pub(super) fn namespace_resolutions(
        &self,
        root: ModuleId,
    ) -> Result<Vec<NamespaceResolution>, ModuleError> {
        if !matches!(self.record(root)?.status, ModuleStatus::Linked { .. }) {
            return Err(ModuleError::InvalidLinkState);
        }
        let max_modules = self.records.len().max(1);
        let max_export_work = self
            .records
            .len()
            .checked_add(self.edge_count)
            .ok_or(ModuleError::CapacityOverflow {
                collection: "module namespace resolution work",
            })?
            .max(1);
        let mut pending = try_work_vec(max_modules, "module namespace traversal")?;
        let mut visited = try_work_vec(max_modules, "module namespace visited set")?;
        let mut names = try_work_vec(max_export_work, "module namespace export names")?;
        pending.push((root, false));
        while let Some((module, through_star)) = pending.pop() {
            if visited.contains(&module) {
                continue;
            }
            reserve_work_push(&mut visited, max_modules, "module namespace visited set")?;
            visited.push(module);
            for export in &self.record(module)?.exports {
                match export {
                    ExportEntry::Local { export_name, .. }
                    | ExportEntry::Indirect { export_name, .. } => {
                        if through_star && export_name.is_default() {
                            continue;
                        }
                        if !names.contains(export_name) {
                            reserve_work_push(
                                &mut names,
                                max_export_work,
                                "module namespace export names",
                            )?;
                            names.push(export_name.clone());
                        }
                    }
                    ExportEntry::Star { module_request } => {
                        let target = self.module_by_specifier(module_request)?;
                        if !visited.contains(&target) {
                            reserve_work_push(
                                &mut pending,
                                max_modules,
                                "module namespace traversal",
                            )?;
                            pending.push((target, true));
                        }
                    }
                }
            }
        }
        names.sort_unstable();
        let mut visits = try_work_vec(max_export_work, "module namespace resolve visits")?;
        let mut frames = try_work_vec(max_export_work, "module namespace resolve frames")?;
        let mut resolutions = try_work_vec(names.len().max(1), "module namespace resolutions")?;
        for name in names {
            match self.resolve_export(root, &name, &mut visits, &mut frames, max_export_work)? {
                ResolveOutcome::Found(binding) => {
                    resolutions.push(NamespaceResolution { name, binding });
                }
                ResolveOutcome::Ambiguous => {}
                ResolveOutcome::NotFound => return Err(ModuleError::MissingExport),
            }
        }
        Ok(resolutions)
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

    fn module_by_specifier(&self, specifier: &ModuleIdentity) -> Result<ModuleId, ModuleError> {
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
                    import.resolved = None;
                }
            }
        }
    }
}

fn push_resolve_frame(
    frames: &mut Vec<ResolveFrame>,
    module: ModuleId,
    name: ModuleExportName,
    max_work: usize,
) -> Result<(), ModuleError> {
    reserve_work_push(frames, max_work, "module export frames")?;
    frames.push(ResolveFrame {
        module,
        name,
        state: ResolveFrameState::Enter,
    });
    Ok(())
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
