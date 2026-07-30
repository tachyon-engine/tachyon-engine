//! Function intrinsic source-text operations.

use super::super::*;

const NATIVE_PREFIX: &str = "function ";
const NATIVE_SUFFIX: &str = "() { [native code] }";

impl Isolate {
    /// Implements `Function.prototype.toString` without adding source edges to closure objects.
    pub(crate) fn function_prototype_to_string(
        &mut self,
        receiver: Value,
    ) -> Result<Value, ExecutionError> {
        if self.is_proxy_value(receiver) {
            if !self.is_callable_value(receiver)? {
                return Err(ExecutionError::NonCallable(receiver));
            }
            return self.allocate_native_function_source("");
        }

        let executable = self.resolve_function_executable(receiver)?;
        let source = match executable {
            FunctionExecutable::Bytecode { code, function, .. } => {
                self.bytecode_function_source(code, function)?
            }
            FunctionExecutable::ClassBytecode(data) => {
                let data = self.class_constructor_snapshot(data)?;
                self.bytecode_function_source(data.code, data.function)?
            }
            FunctionExecutable::Native(native) => {
                return self.allocate_native_function_source(native.name());
            }
            FunctionExecutable::Bound(_)
            | FunctionExecutable::ProxyRevoker(_)
            | FunctionExecutable::PromiseResolver { .. }
            | FunctionExecutable::PromiseCapabilityExecutor(_)
            | FunctionExecutable::PromiseFinallyHandler { .. }
            | FunctionExecutable::PromiseFinallyResultHandler { .. }
            | FunctionExecutable::PromiseCombinatorHandler { .. }
            | FunctionExecutable::AsyncFromSyncIteratorUnwrap { .. }
            | FunctionExecutable::AsyncFromSyncIteratorCloseOnReject { .. } => {
                return self.allocate_native_function_source("");
            }
        };
        self.allocate_runtime_string(source)
    }

    /// Copies one verified UTF-8 source slice before entering the allocating GC path.
    fn bytecode_function_source(
        &self,
        code: CodeId,
        function: FunctionId,
    ) -> Result<JsString, ExecutionError> {
        let module = &self.loaded_code(code)?.module;
        let compiled = module
            .function(function)
            .ok_or(ExecutionError::MissingEntryFunction(function))?;
        let span = compiled
            .source_span()
            .ok_or(ExecutionError::MissingFunctionSource { code, function })?;
        let source = &module.source()[span.start as usize..span.end as usize];
        JsString::try_from_str(source).map_err(ExecutionError::FunctionSourceString)
    }

    /// Builds the shared NativeFunction grammar using immutable internal function identity.
    fn allocate_native_function_source(&mut self, name: &str) -> Result<Value, ExecutionError> {
        let mut source = String::new();
        source
            .try_reserve_exact(NATIVE_PREFIX.len() + name.len() + NATIVE_SUFFIX.len())
            .map_err(|_| {
                ExecutionError::FunctionSourceString(StringAllocationError::AllocationFailed)
            })?;
        source.push_str(NATIVE_PREFIX);
        source.push_str(name);
        source.push_str(NATIVE_SUFFIX);
        let source =
            JsString::try_from_str(&source).map_err(ExecutionError::FunctionSourceString)?;
        self.allocate_runtime_string(source)
    }
}
