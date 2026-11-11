//! Function reachability for `TranspileOptions::entry_exports`.
//!
//! Starting from the chosen entry exports, the start function, the entry points
//! the generated code itself calls and any `ref.func` in a global initialiser,
//! every function reachable through `call`, `return_call` and `ref.func` is
//! live. Table reads are not tracked per slot: once reachable code reads the
//! table (`call_indirect`, `table.get/copy/init`) every element-segment
//! function is live, which is the sound over-approximation (a per-signature
//! split removed nothing on a real C++ module, whose vtables share signatures).

use wasmparser::Operator;

use super::info::{ElemSegment, ExportInfo, FuncInput, ImportInfo};
use super::render::internal_entry_names;
use crate::TranspileError;

/// What reachability needs to know about the module.
pub(crate) struct ReachInput<'a> {
    pub(crate) funcs: &'a [FuncInput<'a>],
    pub(crate) imports: &'a [ImportInfo],
    pub(crate) exports: &'a [ExportInfo],
    pub(crate) elements: &'a [ElemSegment],
    /// Functions named by `ref.func` in a global initialiser.
    pub(crate) global_funcs: &'a [u32],
    pub(crate) start: Option<u32>,
}

/// For every defined function, whether it is live given the chosen
/// `entry_exports`. An unknown export name is an error.
pub(crate) fn live_functions(
    input: &ReachInput<'_>,
    entry_exports: &[String],
) -> Result<Vec<bool>, TranspileError> {
    let n_imports = input.imports.len();
    let mut roots: Vec<u32> = Vec::new();
    for name in entry_exports {
        let export = input
            .exports
            .iter()
            .find(|e| e.name == *name)
            .ok_or_else(|| {
                TranspileError::InvalidOptions(format!(
                    "entry export `{name}` is not an exported function"
                ))
            })?;
        roots.push(export.func_index);
    }
    roots.extend(input.start);
    roots.extend(input.global_funcs);
    let (internal, reads_table) = internal_entry_names(input.imports);
    for name in internal {
        roots.extend(
            input
                .exports
                .iter()
                .filter(|e| e.name == name)
                .map(|e| e.func_index),
        );
    }

    let mut live = vec![false; input.funcs.len()];
    let mut table_read = reads_table;
    let mut elements_added = false;
    let mut work = roots;
    loop {
        while let Some(f) = work.pop() {
            let Some(i) = (f as usize).checked_sub(n_imports) else {
                continue;
            };
            // `live` and `funcs` have the same length; an out-of-range index
            // (a malformed module) is skipped rather than panicking.
            let (Some(slot), Some(func)) = (live.get_mut(i), input.funcs.get(i)) else {
                continue;
            };
            if std::mem::replace(slot, true) {
                continue;
            }
            for op in func.body.get_operators_reader()? {
                match op? {
                    Operator::Call { function_index }
                    | Operator::ReturnCall { function_index }
                    | Operator::RefFunc { function_index } => work.push(function_index),
                    Operator::CallIndirect { .. }
                    | Operator::ReturnCallIndirect { .. }
                    | Operator::TableGet { .. }
                    | Operator::TableCopy { .. }
                    | Operator::TableInit { .. }
                    // `array.new_elem`/`array.init_elem` read a segment's functions too.
                    | Operator::ArrayNewElem { .. }
                    | Operator::ArrayInitElem { .. } => table_read = true,
                    _ => {}
                }
            }
        }
        if table_read && !elements_added {
            elements_added = true;
            work.extend(input.elements.iter().flat_map(|e| e.funcs.iter().copied()));
        } else {
            break;
        }
    }
    Ok(live)
}
