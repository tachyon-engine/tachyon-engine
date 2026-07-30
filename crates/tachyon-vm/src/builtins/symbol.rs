//! Agent-wide ECMAScript Symbol registry operations and Symbol prototype methods.

use super::super::*;

impl Isolate {
    /// Moves one managed Symbol identity into the isolate-owned persistent root table.
    pub(crate) fn persist_symbol_value(
        &mut self,
        symbol: Value,
    ) -> Result<PersistentRootId<SymbolValue>, ExecutionError> {
        let raw = symbol
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedPropertyKey(symbol))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.symbol)
            .map_err(ExecutionError::HeapReference)?;
        self.heap.with_running_scope(|scope| {
            let local = scope.root(reference).map_err(ExecutionError::Root)?;
            scope
                .persist(local, self.types.symbol)
                .map_err(ExecutionError::PersistentRoot)
        })
    }

    /// Resolves one Agent-owned Symbol root into a Value for the current operation.
    pub(crate) fn resolve_persistent_symbol(
        &mut self,
        root: PersistentRootId<SymbolValue>,
    ) -> Result<Value, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            scope
                .local_from_persistent(root, self.types.symbol)
                .map(|symbol| Value::from_heap_ref(symbol.as_gc_ref().raw()))
                .map_err(ExecutionError::PersistentResolve)
        })
    }

    /// Reads a Symbol's immutable isolate-local serial without retaining a payload borrow.
    fn symbol_serial(&mut self, symbol: Value) -> Result<NonZeroU32, ExecutionError> {
        let raw = symbol
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedPropertyKey(symbol))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.symbol)
            .map_err(ExecutionError::HeapReference)?;
        self.heap.with_running_scope(|scope| {
            let symbol = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(symbol, self.types.symbol)
                    .map(|symbol| symbol.serial)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Returns the primitive Symbol receiver or rejects incompatible receivers before observable work.
    fn this_symbol_value(&mut self, value: Value) -> Result<Value, ExecutionError> {
        if self.is_symbol_value(value) {
            return Ok(value);
        }
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        let symbol = self
            .heap
            .checked_reference(raw, self.types.symbol_object)
            .map_err(|_| ExecutionError::NotObject(value))?;
        self.heap.with_running_scope(|scope| {
            let symbol = scope.root(symbol).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(symbol, self.types.symbol_object)
                    .map_err(ExecutionError::NoGcBorrow)?
                    .symbol_data
                    .ok_or(ExecutionError::NotObject(value))
            })
        })
    }

    /// Boxes a Symbol using the observable prototype of the active Symbol intrinsic.
    pub(crate) fn box_symbol(&mut self, symbol: Value) -> Result<Value, ExecutionError> {
        debug_assert!(self.is_symbol_value(symbol));
        let prototype = self
            .realm
            .symbol_prototype
            .expect("Symbol prototype initializes before Symbol boxing");
        self.allocate_symbol_object(Some(symbol), prototype, AllocationSpace::Young)
    }

    /// Implements Symbol.prototype.toString using the primitive Symbol conversion path.
    pub(crate) fn symbol_to_string(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        let symbol = self.this_symbol_value(receiver)?;
        self.primitive_string_value(Some(symbol))
    }

    pub(crate) fn symbol_value_of(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.this_symbol_value(receiver)
    }

    pub(crate) fn symbol_to_primitive(&mut self, receiver: Value) -> Result<Value, ExecutionError> {
        self.this_symbol_value(receiver)
    }

    /// Reads the optional description retained by a primitive Symbol without converting it.
    pub(crate) fn symbol_description_get(
        &mut self,
        receiver: Value,
    ) -> Result<Value, ExecutionError> {
        let symbol = self.this_symbol_value(receiver)?;
        let raw = symbol.as_heap_ref().expect("symbols are heap values");
        let symbol = self
            .heap
            .checked_reference(raw, self.types.symbol)
            .map_err(ExecutionError::HeapReference)?;
        self.heap.with_running_scope(|scope| {
            let symbol = scope.root(symbol).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(symbol, self.types.symbol)
                    .map(|symbol| {
                        symbol
                            .description
                            .unwrap_or(Value::from_immediate(Immediate::Undefined))
                    })
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    /// Resolves an already converted String key in the registry shared by every Realm in this Agent.
    pub(crate) fn symbol_for_string(&mut self, key: Value) -> Result<Value, ExecutionError> {
        debug_assert!(self.is_string_value(key));
        let atom = self.property_key_atom(key)?;
        if let Some(root) = self
            .agent
            .registered_symbols
            .iter()
            .find(|entry| entry.key == atom)
            .map(|entry| entry.root)
        {
            return self.resolve_persistent_symbol(root);
        }
        if self.agent.registered_symbols.len() == self.agent.registered_symbols.capacity() {
            self.agent
                .registered_symbols
                .try_reserve_exact(tuning::symbols::REGISTRY_CAPACITY_GROWTH)
                .map_err(|_| ExecutionError::SymbolRegistryAllocationFailed)?;
        }
        let description = self.atom_string_value(atom)?;
        let symbol = self.allocate_registered_symbol(description)?;
        let serial = self.symbol_serial(symbol)?;
        let root = self.persist_symbol_value(symbol)?;
        self.agent.registered_symbols.push(RegisteredSymbol {
            key: atom,
            serial,
            root,
        });
        Ok(symbol)
    }

    /// Implements Symbol.keyFor by consulting the Agent-wide global registry.
    pub(crate) fn symbol_key_for(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let symbol = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_symbol_value(symbol) {
            return Err(ExecutionError::NotObject(symbol));
        }
        let serial = self.symbol_serial(symbol)?;
        let Some(entry) = self
            .agent
            .registered_symbols
            .iter()
            .find(|entry| entry.serial == serial)
        else {
            return Ok(Value::from_immediate(Immediate::Undefined));
        };
        self.atom_string_value(entry.key)
    }

    /// Returns whether a Symbol is excluded from CanBeHeldWeakly by the global registry.
    pub(crate) fn is_registered_symbol(&mut self, value: Value) -> Result<bool, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedPropertyKey(value))?;
        let symbol = self
            .heap
            .checked_reference(raw, self.types.symbol)
            .map_err(|_| ExecutionError::UnsupportedPropertyKey(value))?;
        self.heap.with_running_scope(|scope| {
            let symbol = scope.root(symbol).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(symbol, self.types.symbol)
                    .map(|symbol| symbol.registered)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }
}
