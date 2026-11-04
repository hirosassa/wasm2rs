//! Pull the core modules out of a WebAssembly component.

use wasmparser::{Encoding, Parser, Payload};

use crate::TranspileError;

/// The core modules inside `wasm`, as standalone module binaries in the order they
/// appear (modules of nested components included).
///
/// A core module is returned as is, a component without modules gives an empty
/// list. Each result can be passed to [`transpile`](crate::transpile); which one
/// is the program (usually the largest) is for the caller to decide, as is wiring
/// the component's other parts, which wasm2rs does not translate.
pub fn extract_core_modules(wasm: &[u8]) -> Result<Vec<Vec<u8>>, TranspileError> {
    let mut modules = Vec::new();
    let mut first = true;
    let mut is_module = false;
    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::Version { encoding, .. } if first => {
                first = false;
                is_module = encoding == Encoding::Module;
            }
            Payload::ModuleSection {
                unchecked_range, ..
            } => {
                let bytes = usize::try_from(unchecked_range.start)
                    .ok()
                    .zip(usize::try_from(unchecked_range.end).ok())
                    .and_then(|(start, end)| wasm.get(start..end))
                    .ok_or_else(|| {
                        TranspileError::Unsupported("module range out of bounds".into())
                    })?;
                modules.push(bytes.to_vec());
            }
            _ => {}
        }
    }
    // Parsed to the end first so a damaged module is an error, not a pass-through.
    if is_module {
        return Ok(vec![wasm.to_vec()]);
    }
    Ok(modules)
}
