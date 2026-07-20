//! Realm-local ECMAScript global Symbol registry operations.

use super::super::*;
use crate::runtime::realm::RegisteredSymbol;

impl Isolate {
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

    /// Implements Symbol.for for primitive keys and retains the returned registry symbol in Realm.
    pub(crate) fn symbol_for(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let argument = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        let key = self.primitive_string_value(Some(argument))?;
        let atom = self.property_key_atom(key)?;
        if let Some(entry) = self
            .realm
            .registered_symbols
            .iter()
            .find(|entry| entry.key == atom)
        {
            return Ok(entry.symbol);
        }
        let description = self.atom_string_value(atom)?;
        let symbol = self.allocate_registered_symbol(description)?;
        if self.realm.registered_symbols.len() == self.realm.registered_symbols.capacity() {
            self.realm
                .registered_symbols
                .try_reserve_exact(8)
                .map_err(|_| ExecutionError::SymbolRegistryAllocationFailed)?;
        }
        self.realm
            .registered_symbols
            .push(RegisteredSymbol { key: atom, symbol });
        Ok(symbol)
    }

    /// Implements Symbol.keyFor by resolving only symbols owned by this Realm's registry.
    pub(crate) fn symbol_key_for(&mut self, site: &CallSite) -> Result<Value, ExecutionError> {
        let symbol = self
            .call_argument(site, 0)?
            .unwrap_or(Value::from_immediate(Immediate::Undefined));
        if !self.is_symbol_value(symbol) {
            return Err(ExecutionError::NotObject(symbol));
        }
        let Some(entry) = self
            .realm
            .registered_symbols
            .iter()
            .find(|entry| entry.symbol == symbol)
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
