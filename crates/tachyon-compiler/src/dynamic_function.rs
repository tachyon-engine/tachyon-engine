//! Dynamic Function validation over Oxc's public full-program parser surface.

use std::sync::Arc;

use crate::{
    CompileError, CompileOptions, Compiler, MediaType, SourceId, SourceMode, SourceName, SourceText,
};

/// Closed grammar family for CreateDynamicFunction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicFunctionKind {
    Ordinary,
    Generator,
    Async,
    AsyncGenerator,
}

impl DynamicFunctionKind {
    #[inline(always)]
    const fn prefix(self) -> &'static str {
        match self {
            Self::Ordinary => "function",
            Self::Generator => "function*",
            Self::Async => "async function",
            Self::AsyncGenerator => "async function*",
        }
    }
}

/// Performs separate grammar-boundary validation before compiling the combined expression.
pub(super) fn compile(
    compiler: &Compiler,
    source_id: SourceId,
    source_name: SourceName,
    kind: DynamicFunctionKind,
    parameters: &[Box<[u16]>],
    body: &[u16],
) -> Result<tachyon_bytecode::CompiledModule, CompileError> {
    let parameters = decode_parameters(parameters)?;
    let body = String::from_utf16(body).map_err(|_| CompileError::MalformedDynamicFunctionUtf16)?;
    let prefix = kind.prefix();
    let parameter_check = format!("({prefix} __tachyon_validate({parameters}\n){{}})");
    let body_check = format!("({prefix} __tachyon_validate(){{\n{body}\n}})");
    let combined = format!("({prefix} anonymous({parameters}\n) {{\n{body}\n}})");
    let options = CompileOptions {
        source_mode: SourceMode::Script,
        direct_eval: false,
    };
    compiler.parse(
        source(source_id, source_name.clone(), parameter_check),
        options,
    )?;
    compiler.parse(source(source_id, source_name.clone(), body_check), options)?;
    compiler.compile(source(source_id, source_name, combined), options)
}

/// Strictly decodes every parameter and inserts only the specification's commas.
fn decode_parameters(parameters: &[Box<[u16]>]) -> Result<String, CompileError> {
    let mut decoded = Vec::new();
    decoded
        .try_reserve_exact(parameters.len())
        .map_err(|_| CompileError::ConstantAllocationFailed)?;
    let mut capacity = parameters.len().saturating_sub(1);
    for parameter in parameters {
        let parameter = String::from_utf16(parameter)
            .map_err(|_| CompileError::MalformedDynamicFunctionUtf16)?;
        capacity = capacity.saturating_add(parameter.len());
        decoded.push(parameter);
    }
    let mut joined = String::new();
    joined
        .try_reserve_exact(capacity)
        .map_err(|_| CompileError::ConstantAllocationFailed)?;
    for (index, parameter) in decoded.iter().enumerate() {
        if index != 0 {
            joined.push(',');
        }
        joined.push_str(parameter);
    }
    Ok(joined)
}

#[inline]
fn source(id: SourceId, name: SourceName, text: String) -> SourceText {
    SourceText::new(id, name, MediaType::JavaScript, Arc::<str>::from(text))
}
