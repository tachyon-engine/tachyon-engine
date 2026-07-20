//! Realm-local ECMAScript global Symbol registry operations.

use super::super::*;
use crate::runtime::realm::RegisteredSymbol;

impl Isolate {
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
