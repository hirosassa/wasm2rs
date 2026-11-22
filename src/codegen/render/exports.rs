//! Exported-function entry points: the internal entry set, the `named_export` methods and `call_start`.

use super::globals::{global_getter_lines, global_setter_lines, settable_globals};
use crate::TranspileError;
use crate::codegen::{
    ExportInfo, ImportInfo, ModuleCtx, ModuleParts, WasiFn, rust_type, rust_types,
};
use wasmparser::ValType;

/// The export name wasi-threads looks up to start a spawned thread.
const WASI_THREAD_START: &str = "wasi_thread_start";

/// The exports an emscripten pthread's start-up drives, with their signatures:
/// `_emscripten_thread_init(ptr, is_main, is_runtime, can_block, 0, 0)`,
/// `_emscripten_tls_init() -> i32` and `_emscripten_thread_exit(result)`.
const EMSCRIPTEN_ENTRIES: [(&str, &[ValType], &[ValType]); 3] = {
    use ValType::I32;
    [
        (
            "_emscripten_thread_init",
            &[I32, I32, I32, I32, I32, I32],
            &[],
        ),
        ("_emscripten_tls_init", &[], &[I32]),
        ("_emscripten_thread_exit", &[I32], &[]),
    ]
};

/// The stack exports a Worker's start-up calls first. Optional: a module
/// exporting both has its child threads' stacks set up (see
/// `pthread_create_js_lines`); emscripten always exports them.
pub(super) const EMSCRIPTEN_STACK_ENTRIES: [(&str, &[ValType], &[ValType]); 2] = {
    use ValType::I32;
    [
        ("emscripten_stack_set_limits", &[I32, I32], &[]),
        ("_emscripten_stack_restore", &[I32], &[]),
    ]
};

/// The export names the generated code itself calls into, and whether the
/// module also reads the table through them (`emscripten_call_start` calls a
/// table entry by index). Used to keep them live under `entry_exports`.
pub(in crate::codegen) fn internal_entry_names(
    imports: &[ImportInfo],
) -> (Vec<&'static str>, bool) {
    let imports_wasi = |f: WasiFn| imports.iter().any(|im| im.wasi == Some(f));
    let mut names = vec![WASI_THREAD_START];
    if imports_wasi(WasiFn::PthreadCreateJs) || imports_wasi(WasiFn::EmscriptenInitMainThread) {
        names.extend(EMSCRIPTEN_ENTRIES.iter().map(|(name, ..)| *name));
    }
    if imports_wasi(WasiFn::PthreadCreateJs) {
        names.extend(EMSCRIPTEN_STACK_ENTRIES.iter().map(|(name, ..)| *name));
    }
    (names, imports_wasi(WasiFn::PthreadCreateJs))
}

/// By-name entry points forwarding to the exported functions' `func{N}`: always
/// `wasi_thread_start` when exported, and, for a module importing
/// `env.__pthread_create_js`, the emscripten thread start-up exports plus
/// `emscripten_call_start`, which calls a table entry `(i32) -> i32` by index
/// (the Worker's `getWasmTableEntry(start)(arg)`). Any other export keeps only
/// its `func{N}` name, so ordinary modules are emitted unchanged. A stateful
/// module gets methods, a stateless one free functions.
pub(in crate::codegen) fn entry_point_lines(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
) -> Result<Vec<String>, TranspileError> {
    let mut lines = Vec::new();
    if parts.exports.iter().any(|e| e.name == WASI_THREAD_START) {
        lines.extend(named_entry(
            parts,
            ctx,
            WASI_THREAD_START,
            &[ValType::I32, ValType::I32],
            &[],
        )?);
    }
    let imports_wasi = |f: WasiFn| parts.imports.iter().any(|im| im.wasi == Some(f));
    if imports_wasi(WasiFn::PthreadCreateJs) || imports_wasi(WasiFn::EmscriptenInitMainThread) {
        for (name, params, results) in EMSCRIPTEN_ENTRIES {
            if !parts.exports.iter().any(|e| e.name == name) {
                return Err(TranspileError::Unsupported(format!(
                    "emscripten thread imports require an exported {name}"
                )));
            }
            lines.extend(named_entry(parts, ctx, name, params, results)?);
        }
        if imports_wasi(WasiFn::PthreadCreateJs) {
            for (name, params, results) in EMSCRIPTEN_STACK_ENTRIES {
                lines.extend(named_entry(parts, ctx, name, params, results)?);
            }
            lines.extend(call_start_lines(ctx)?);
        }
    }
    if parts.opts.named_exports {
        lines.extend(named_export_lines(parts, ctx)?);
    }
    Ok(lines)
}

/// A by-name wrapper forwarding to `func{N}` for every exported function that is
/// defined (not imported) and emitted, with the same signature. Names come from
/// [`crate::codegen::export_names::assign`]; `wasi_thread_start` already has its own entry
/// point, so it is skipped here.
fn named_export_lines(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
) -> Result<Vec<String>, TranspileError> {
    let exports: Vec<&ExportInfo> = parts
        .exports
        .iter()
        .filter(|e| e.name != WASI_THREAD_START)
        .collect();
    // Global getters and setters share the namespace with the function wrappers, so
    // the names are assigned together (functions, then getters, then setters; an
    // earlier name wins a clash, so a setter never displaces an export).
    let settable = settable_globals(parts, ctx);
    let setter_raw: Vec<String> = settable
        .iter()
        .map(|(e, _)| format!("set_{}", e.name))
        .collect();
    let idents = crate::codegen::export_names::assign(
        exports
            .iter()
            .map(|e| (e.name.as_str(), e.func_index))
            .chain(
                parts
                    .global_exports
                    .iter()
                    .map(|g| (g.name.as_str(), g.global_index)),
            )
            .chain(
                settable
                    .iter()
                    .zip(&setter_raw)
                    .map(|((e, _), raw)| (raw.as_str(), e.global_index)),
            ),
    );
    let getters_end = exports.len() + parts.global_exports.len();
    let mut lines = Vec::new();
    lines.extend(global_getter_lines(
        parts,
        ctx,
        &idents[exports.len()..getters_end],
    ));
    lines.extend(global_setter_lines(&settable, &idents[getters_end..]));
    for (export, ident) in exports.iter().zip(idents) {
        let Some(defined) = usize::try_from(export.func_index)
            .ok()
            .and_then(|i| i.checked_sub(parts.imports.len()))
        else {
            continue;
        };
        let (Some(sig), Some(&live)) = (parts.funcs.get(defined), parts.live.get(defined)) else {
            continue;
        };
        if !live {
            continue;
        }
        let mut decls: Vec<String> = Vec::new();
        if ctx.is_method {
            decls.push("&mut self".to_string());
        }
        let mut args: Vec<String> = Vec::new();
        for (k, ty) in sig.params.iter().enumerate() {
            decls.push(format!("a{k}: {}", rust_type(*ty, ctx.type_kinds)?));
            args.push(format!("a{k}"));
        }
        let ret = match sig.results {
            [] => String::new(),
            [ty] => format!(" -> {}", rust_type(*ty, ctx.type_kinds)?),
            many => format!(" -> ({})", rust_types(many, ctx.type_kinds)?.join(", ")),
        };
        let call = ctx.call_expr(export.func_index as usize, &args.join(", "));
        lines.push(String::new());
        lines.push(format!(
            "pub fn {ident}({}){ret} {{ {call} }}",
            decls.join(", ")
        ));
    }
    Ok(lines)
}

/// One by-name wrapper for the export `name`, whose function must be defined
/// (not imported) and have exactly the given signature.
fn named_entry(
    parts: &ModuleParts<'_>,
    ctx: &ModuleCtx<'_>,
    name: &str,
    params: &[ValType],
    results: &[ValType],
) -> Result<Vec<String>, TranspileError> {
    let Some(export) = parts.exports.iter().find(|e| e.name == name) else {
        return Ok(Vec::new());
    };
    let unsupported = |why: &str| TranspileError::Unsupported(format!("{name}: {why}"));
    let sig = usize::try_from(export.func_index)
        .ok()
        .and_then(|i| i.checked_sub(parts.imports.len()))
        .and_then(|i| parts.funcs.get(i))
        .ok_or_else(|| unsupported("must be a defined function, not an import"))?;
    if sig.params != params || sig.results != results {
        return Err(unsupported(&format!(
            "must have type {params:?} -> {results:?}"
        )));
    }
    let self_param = if ctx.is_method { "&mut self" } else { "" };
    let arg_decls: Vec<String> = (0..params.len()).map(|k| format!("a{k}: i32")).collect();
    let arg_list: Vec<String> = (0..params.len()).map(|k| format!("a{k}")).collect();
    let sep = if ctx.is_method && !arg_decls.is_empty() {
        ", "
    } else {
        ""
    };
    let ret = if results.is_empty() { "" } else { " -> i32" };
    let call = ctx.call_expr(export.func_index as usize, &arg_list.join(", "));
    Ok(vec![
        String::new(),
        format!(
            "pub fn {name}({self_param}{sep}{}){ret} {{ {call} }}",
            arg_decls.join(", ")
        ),
    ])
}

/// `emscripten_call_start(f, a0) -> i32`: look `f` up in the table and call the
/// `(i32) -> i32` function it names, panicking (a trap) on null or a wrong type. Needs a
/// module-owned table, since the start routine is a table index.
fn call_start_lines(ctx: &ModuleCtx<'_>) -> Result<Vec<String>, TranspileError> {
    if !ctx.is_method {
        return Err(TranspileError::Unsupported(
            "env.__pthread_create_js requires a stateful module".into(),
        ));
    }
    let mut lines = vec![
        String::new(),
        "pub fn emscripten_call_start(&mut self, f: u32, a0: i32) -> i32 {".to_string(),
        "    match self.table().get(f as usize).copied().unwrap_or(u32::MAX) {".to_string(),
    ];
    for &fidx in ctx.funcs_with_sig(&[ValType::I32], &[ValType::I32]) {
        // A continuation step function is `cont_step_func{N}`, not `func{N}`.
        if u32::try_from(fidx).is_ok_and(|f| ctx.cont.step_set.binary_search(&f).is_ok()) {
            continue;
        }
        lines.push(format!(
            "        {fidx}u32 => {},",
            ctx.call_expr(fidx, "a0")
        ));
    }
    lines.push(
        "        _ => panic!(\"emscripten pthread: start routine is not a (i32) -> i32 function\"),"
            .to_string(),
    );
    lines.push("    }".to_string());
    lines.push("}".to_string());
    Ok(lines)
}
