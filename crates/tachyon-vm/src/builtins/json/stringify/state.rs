use super::*;

impl Isolate {
    /// Reloads the movable JSON state from its rooted call destination after a safepoint.
    pub(super) fn refresh_json_state(
        &mut self,
        site: NativeContinuationSite,
    ) -> Result<GcRef<PendingJsonStringify>, ExecutionError> {
        let value = self.read(site.caller_base, site.destination)?;
        self.pending_json_stringify_reference(value)
    }

    /// Snapshots enumerable own string keys without retaining object storage borrows.
    pub(super) fn json_ordinary_enumerable_keys(
        &mut self,
        object: Value,
    ) -> Result<Vec<AtomId>, ExecutionError> {
        let (_, snapshot) = self.object_snapshot(object)?;
        let mut source = self.ordinary_own_property_keys(object, snapshot)?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(source.len())
            .map_err(|_| ExecutionError::OwnPropertyKeyAllocationFailed)?;
        while let Some(entry) = source.next_entry() {
            let Some(property) = entry.property else {
                continue;
            };
            if property.attributes.enumerable()
                && self.property_is_present_from_snapshot(snapshot, property)?
                && let Some(atom) = entry.key.atom()
            {
                keys.push(atom);
            }
        }
        Ok(keys)
    }

    /// Reads the internal key-array length without invoking user code.
    pub(super) fn json_key_array_length(&mut self, array: Value) -> Result<u64, ExecutionError> {
        let length_atom = self.length_atom()?;
        let length = self
            .get_data_property(array, length_atom)?
            .and_then(|value| value.as_i32())
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        u64::try_from(length).map_err(|_| ExecutionError::ArrayLengthOverflow)
    }

    /// Materializes stable Atom strings in an internal managed Array.
    pub(super) fn json_atom_key_array(
        &mut self,
        site: NativeContinuationSite,
        keys: Vec<AtomId>,
    ) -> Result<Value, ExecutionError> {
        let prototype = self
            .realm
            .array_prototype
            .expect("Array prototype initializes before JSON");
        let array = self.create_array_object_with_prototype(prototype)?;
        let state = self.refresh_json_state(site)?;
        self.set_json_top_frame_keys(state, array, keys.len() as u64)?;
        for (index, key) in keys.into_iter().enumerate() {
            let index = self.safe_integer_property_atom(index as u64)?;
            let value = self.atom_string_value(key)?;
            let state = self.refresh_json_state(site)?;
            self.set_json_temporary(state, value)?;
            let array = self.json_top_frame_keys_value(state)?;
            self.set_own_data_property(array, index, value)?;
        }
        let state = self.refresh_json_state(site)?;
        self.json_top_frame_keys_value(state)
    }

    pub(super) fn allocate_json_stringify_state(
        &mut self,
        pending: PendingJsonStringify,
    ) -> Result<GcRef<PendingJsonStringify>, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            promise_jobs: &mut self.promise_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
            module_graph: &mut self.module_graph,
        };
        self.heap
            .try_allocate_external_with_gc(
                self.types.pending_json_stringify,
                0,
                pending,
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_json_stringify_reference(
        &mut self,
        value: Value,
    ) -> Result<GcRef<PendingJsonStringify>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_json_stringify)
            .map_err(|_| ExecutionError::MissingNativeContinuation)
    }

    pub(super) fn root_json_stringify_state(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        self.write(
            site.caller_base,
            site.destination,
            Value::from_heap_ref(state.raw()),
        )
    }

    pub(super) fn json_snapshot(
        &mut self,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<JsonSnapshot, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_json_stringify)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(JsonSnapshot {
                    replacer: pending.replacer,
                    property_list: pending.property_list,
                    property_list_source: pending.property_list_source,
                    holder: pending.holder,
                    key: pending.key,
                    value: pending.value,
                    indentation: pending.indentation,
                    space: pending.space,
                    property_list_index: pending.property_list_index,
                    property_list_length: pending.property_list_length,
                    property_list_count: pending.property_list_count,
                    frame_depth: pending.frames.len(),
                })
            })
        })
    }

    pub(super) fn json_top_frame_snapshot(
        &mut self,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<Option<JsonFrameSnapshot>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_json_stringify)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(pending.frames.last().map(|frame| JsonFrameSnapshot {
                    container: frame.container,
                    index: frame.index,
                    length: frame.length,
                    wrote_property: frame.wrote_property,
                    descriptor_checks: frame.descriptor_checks,
                    kind: frame.kind,
                }))
            })
        })
    }

    pub(super) fn json_temporary(
        &mut self,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_json_stringify)
                    .map(|pending| pending.temporary)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(super) fn set_json_value(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.update_json_value_edge(state, value, |pending| &mut pending.value)
    }

    pub(super) fn set_json_temporary(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.update_json_value_edge(state, value, |pending| &mut pending.temporary)
    }

    pub(super) fn set_json_property_list(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.update_json_value_edge(state, value, |pending| &mut pending.property_list)
    }

    pub(super) fn set_json_current_property(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        holder: Value,
        key: Value,
    ) -> Result<(), ExecutionError> {
        self.update_json_value_edge(state, holder, |pending| &mut pending.holder)?;
        self.update_json_value_edge(state, key, |pending| &mut pending.key)
    }

    pub(super) fn update_json_value_edge(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        value: Value,
        select: impl FnOnce(&mut PendingJsonStringify) -> &mut Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_json_stringify)
                    .map_err(ExecutionError::NoGcBorrow)?;
                *select(pending) = value;
                Ok(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    pub(super) fn set_json_indentation(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        indentation: JsonIndentation,
    ) -> Result<(), ExecutionError> {
        self.with_json_state_mut(state, |pending| {
            pending.indentation = indentation;
            Ok(())
        })
    }

    pub(super) fn set_json_property_list_length(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        length: u64,
    ) -> Result<(), ExecutionError> {
        self.with_json_state_mut(state, |pending| {
            pending.property_list_length = length;
            Ok(())
        })
    }

    pub(super) fn advance_json_property_list_index(
        &mut self,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        self.with_json_state_mut(state, |pending| {
            pending.property_list_index = pending
                .property_list_index
                .checked_add(1)
                .ok_or(ExecutionError::ArrayLengthOverflow)?;
            Ok(())
        })
    }

    /// Checks the internal property-list Array without invoking user code.
    pub(super) fn json_property_list_contains(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        atom: AtomId,
    ) -> Result<bool, ExecutionError> {
        let snapshot = self.json_snapshot(state)?;
        for index in 0..snapshot.property_list_count {
            let key = self.safe_integer_property_atom(index)?;
            let value = self
                .get_data_property(snapshot.property_list, key)?
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            if self.property_key_atom(value)? == atom {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Appends one stable Atom string to the rooted internal property-list Array.
    pub(super) fn append_json_property_list_atom(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        atom: AtomId,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.json_snapshot(state)?;
        let key = self.safe_integer_property_atom(snapshot.property_list_count)?;
        let value = self.atom_string_value(atom)?;
        let state = self.refresh_json_state(site)?;
        self.set_json_temporary(state, value)?;
        let property_list = self.json_snapshot(state)?.property_list;
        self.set_own_data_property(property_list, key, value)?;
        let state = self.refresh_json_state(site)?;
        self.with_json_state_mut(state, |pending| {
            pending.property_list_count = pending
                .property_list_count
                .checked_add(1)
                .ok_or(ExecutionError::ArrayLengthOverflow)?;
            Ok(())
        })
    }

    pub(super) fn push_json_frame(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        kind: JsonContainerKind,
    ) -> Result<GcRef<PendingJsonStringify>, ExecutionError> {
        let state = self.ensure_json_frame_capacity(site, state, 1)?;
        let container = self.json_snapshot(state)?.value;
        self.with_json_state_mut(state, |pending| {
            pending.frames.push(JsonFrame {
                container,
                keys: Value::from_immediate(Immediate::Undefined),
                index: 0,
                length: 0,
                wrote_property: false,
                descriptor_checks: false,
                kind,
            });
            Ok(())
        })?;
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope
                .write_value_barrier(state, container)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })?;
        Ok(state)
    }

    pub(super) fn pop_json_frame(
        &mut self,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        self.with_json_state_mut(state, |pending| {
            pending
                .frames
                .pop()
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            Ok(())
        })
    }

    pub(super) fn set_json_top_frame_length(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        length: u64,
    ) -> Result<(), ExecutionError> {
        self.with_json_state_mut(state, |pending| {
            pending
                .frames
                .last_mut()
                .ok_or(ExecutionError::MissingNativeContinuation)?
                .length = length;
            Ok(())
        })
    }

    pub(super) fn set_json_top_frame_keys(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        keys: Value,
        length: u64,
    ) -> Result<(), ExecutionError> {
        self.with_json_state_mut(state, |pending| {
            let frame = pending
                .frames
                .last_mut()
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            frame.length = length;
            frame.keys = keys;
            Ok(())
        })?;
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope
                .write_value_barrier(state, keys)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    pub(super) fn set_json_top_frame_wrote(
        &mut self,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        self.with_json_state_mut(state, |pending| {
            pending
                .frames
                .last_mut()
                .ok_or(ExecutionError::MissingNativeContinuation)?
                .wrote_property = true;
            Ok(())
        })
    }

    pub(super) fn set_json_top_frame_descriptor_checks(
        &mut self,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        self.with_json_state_mut(state, |pending| {
            pending
                .frames
                .last_mut()
                .ok_or(ExecutionError::MissingNativeContinuation)?
                .descriptor_checks = true;
            Ok(())
        })
    }

    pub(super) fn advance_json_top_frame_index(
        &mut self,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(), ExecutionError> {
        self.with_json_state_mut(state, |pending| {
            let frame = pending
                .frames
                .last_mut()
                .ok_or(ExecutionError::MissingNativeContinuation)?;
            frame.index = frame
                .index
                .checked_add(1)
                .ok_or(ExecutionError::ArrayLengthOverflow)?;
            Ok(())
        })
    }

    pub(super) fn json_top_frame_key(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        index: usize,
    ) -> Result<Option<AtomId>, ExecutionError> {
        self.heap
            .with_running_scope(|scope| {
                let state = scope.root(state).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let pending = no_gc
                        .borrow(state, self.types.pending_json_stringify)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    let keys = pending
                        .frames
                        .last()
                        .map(|frame| frame.keys)
                        .ok_or(ExecutionError::MissingNativeContinuation)?;
                    Ok(keys)
                })
            })
            .and_then(|keys| {
                let index = self.safe_integer_property_atom(index as u64)?;
                self.get_data_property(keys, index)?
                    .map(|value| self.property_key_atom(value))
                    .transpose()
            })
    }

    pub(super) fn json_top_frame_keys_value(
        &mut self,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_json_stringify)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .frames
                    .last()
                    .map(|frame| frame.keys)
                    .ok_or(ExecutionError::MissingNativeContinuation)
            })
        })
    }

    pub(super) fn json_contains_container(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        container: Value,
    ) -> Result<bool, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_json_stringify)
                    .map(|pending| {
                        pending
                            .frames
                            .iter()
                            .any(|frame| frame.container == container)
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(super) fn append_json_ascii(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        ascii: &[u8],
    ) -> Result<GcRef<PendingJsonStringify>, ExecutionError> {
        let state = self.ensure_json_output_capacity(site, state, ascii.len())?;
        self.with_json_state_mut(state, |pending| {
            pending.output.extend(ascii.iter().copied().map(u16::from));
            Ok(())
        })?;
        Ok(state)
    }

    pub(super) fn append_json_units(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        units: &[u16],
    ) -> Result<GcRef<PendingJsonStringify>, ExecutionError> {
        let state = self.ensure_json_output_capacity(site, state, units.len())?;
        self.with_json_state_mut(state, |pending| {
            pending.output.extend_from_slice(units);
            Ok(())
        })?;
        Ok(state)
    }

    pub(super) fn append_json_line_indent(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        depth: usize,
    ) -> Result<GcRef<PendingJsonStringify>, ExecutionError> {
        let indentation = self.json_snapshot(state)?.indentation;
        let mut units = Vec::new();
        indentation.append_line_indent(depth, &mut units)?;
        self.append_json_units(site, state, &units)
    }

    pub(super) fn copy_json_output(
        &mut self,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<Vec<u16>, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_json_stringify)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let mut output = Vec::new();
                output
                    .try_reserve_exact(pending.output.len())
                    .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                output.extend_from_slice(&pending.output);
                Ok(output)
            })
        })
    }

    /// Replaces the externally-accounted state before output capacity changes.
    pub(super) fn ensure_json_output_capacity(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        additional: usize,
    ) -> Result<GcRef<PendingJsonStringify>, ExecutionError> {
        let (length, capacity, frame_capacity) = self.json_buffer_capacities(state)?;
        let required = length
            .checked_add(additional)
            .ok_or(ExecutionError::StringBufferAllocationFailed)?;
        if required <= capacity {
            return Ok(state);
        }
        let output_capacity = tuning::json::grown_output_capacity(capacity, required)
            .ok_or(ExecutionError::StringBufferAllocationFailed)?;
        self.replace_json_storage(site, state, output_capacity, frame_capacity)
    }

    /// Replaces the externally-accounted state before frame capacity changes.
    pub(super) fn ensure_json_frame_capacity(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        additional: usize,
    ) -> Result<GcRef<PendingJsonStringify>, ExecutionError> {
        let (output_length, output_capacity, frame_capacity) =
            self.json_buffer_capacities(state)?;
        let frame_length = self.json_snapshot(state)?.frame_depth;
        let required = frame_length
            .checked_add(additional)
            .ok_or(ExecutionError::StringBufferAllocationFailed)?;
        if required <= frame_capacity {
            return Ok(state);
        }
        let frame_capacity = tuning::json::grown_frame_capacity(frame_capacity, required)
            .ok_or(ExecutionError::StringBufferAllocationFailed)?;
        debug_assert!(output_length <= output_capacity);
        self.replace_json_storage(site, state, output_capacity, frame_capacity)
    }

    /// Copies one pending operation into newly charged fixed-capacity Vec backings.
    pub(super) fn replace_json_storage(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingJsonStringify>,
        output_capacity: usize,
        frame_capacity: usize,
    ) -> Result<GcRef<PendingJsonStringify>, ExecutionError> {
        let replacement = self.clone_json_state(state, output_capacity, frame_capacity)?;
        let replacement = self.allocate_json_stringify_state(replacement)?;
        self.root_json_stringify_state(site, replacement)?;
        Ok(replacement)
    }

    /// Copies all traced/scalar state without retaining a managed borrow across allocation.
    pub(super) fn clone_json_state(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        output_capacity: usize,
        frame_capacity: usize,
    ) -> Result<PendingJsonStringify, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_json_stringify)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let mut output = Vec::new();
                output
                    .try_reserve_exact(output_capacity)
                    .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                output.extend_from_slice(&pending.output);
                let mut frames = Vec::new();
                frames
                    .try_reserve_exact(frame_capacity)
                    .map_err(|_| ExecutionError::StringBufferAllocationFailed)?;
                frames.extend(pending.frames.iter().cloned());
                Ok(PendingJsonStringify {
                    replacer: pending.replacer,
                    property_list: pending.property_list,
                    property_list_source: pending.property_list_source,
                    holder: pending.holder,
                    key: pending.key,
                    value: pending.value,
                    temporary: pending.temporary,
                    space: pending.space,
                    property_list_index: pending.property_list_index,
                    property_list_length: pending.property_list_length,
                    property_list_count: pending.property_list_count,
                    indentation: pending.indentation,
                    output,
                    frames,
                })
            })
        })
    }

    /// Returns committed output length and immutable backing capacities.
    pub(super) fn json_buffer_capacities(
        &mut self,
        state: GcRef<PendingJsonStringify>,
    ) -> Result<(usize, usize, usize), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow(state, self.types.pending_json_stringify)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok((
                    pending.output.len(),
                    pending.output.capacity(),
                    pending.frames.capacity(),
                ))
            })
        })
    }

    pub(super) fn with_json_state_mut<T>(
        &mut self,
        state: GcRef<PendingJsonStringify>,
        mutate: impl FnOnce(&mut PendingJsonStringify) -> Result<T, ExecutionError>,
    ) -> Result<T, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_json_stringify)
                    .map_err(ExecutionError::NoGcBorrow)?;
                mutate(pending)
            })
        })
    }
}
