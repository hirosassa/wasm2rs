//! A read-only summary of a wasm module (`wasm2rs --dump`).
//!
//! The summary reads the binary with `wasmparser` directly and never runs code
//! generation, so it also works for a module the transpiler would reject. Import
//! resolution reuses [`codegen::WasiFn::recognise`], so a function is reported
//! "native WASI" exactly when codegen would generate it natively.

use std::fmt;

use wasmparser::{
    CompositeInnerType, ExternalKind, GlobalType, MemoryType, Operator, Parser, Payload, TypeRef,
    ValType,
};

use crate::{TranspileError, codegen};

/// How an import would be resolved in the generated code.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Resolution {
    /// A recognised WASI/Emscripten function, generated as an inherent method.
    NativeWasi,
    /// Dispatched through the injected `Imports` trait.
    HostTrait,
    /// A shared memory: a handle passed to `Instance::new`, not lent by the host.
    SharedHandle,
}

impl Resolution {
    fn label(self) -> &'static str {
        match self {
            Self::NativeWasi => "native WASI",
            Self::HostTrait => "host trait",
            Self::SharedHandle => "shared memory handle",
        }
    }
}

struct ImportLine {
    module: String,
    name: String,
    /// The rendered type, e.g. `(i32, i32) -> (i32)` or `global i32`.
    ty: String,
    resolution: Resolution,
}

struct MemoryLine {
    min: u64,
    max: Option<u64>,
    shared: bool,
    imported: bool,
}

/// A summary of a parsed wasm module, rendered with [`fmt::Display`]. Built by
/// [`dump`].
pub struct ModuleSummary {
    imported_funcs: usize,
    defined_funcs: usize,
    imports: Vec<ImportLine>,
    exports: Vec<(String, &'static str, u32)>,
    memories: Vec<MemoryLine>,
    tables: usize,
    globals: usize,
    tags: usize,
    data_segments: usize,
    element_segments: usize,
    features: Vec<&'static str>,
}

impl ModuleSummary {
    /// Whether the generated `Instance` would be generic over a host `Imports`
    /// trait (mirrors the code generator's `needs_host_trait`).
    fn needs_host_trait(&self) -> bool {
        self.imports
            .iter()
            .any(|i| i.resolution == Resolution::HostTrait)
    }
}

impl fmt::Display for ModuleSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "functions: {} ({} imported, {} defined)",
            self.imported_funcs.saturating_add(self.defined_funcs),
            self.imported_funcs,
            self.defined_funcs
        )?;
        writeln!(f, "imports: {}", self.imports.len())?;
        for i in &self.imports {
            writeln!(
                f,
                "  {}.{} {} [{}]",
                i.module,
                i.name,
                i.ty,
                i.resolution.label()
            )?;
        }
        writeln!(f, "exports: {}", self.exports.len())?;
        for (name, kind, index) in &self.exports {
            writeln!(f, "  {name} ({kind} {index})")?;
        }
        writeln!(f, "memories: {}", self.memories.len())?;
        for (i, m) in self.memories.iter().enumerate() {
            write!(f, "  memory {i}: min {}", m.min)?;
            match m.max {
                Some(max) => write!(f, ", max {max}")?,
                None => write!(f, ", no max")?,
            }
            if m.shared {
                write!(f, ", shared")?;
            }
            if m.imported {
                write!(f, " (imported)")?;
            }
            writeln!(f)?;
        }
        writeln!(f, "tables: {}", self.tables)?;
        writeln!(f, "globals: {}", self.globals)?;
        writeln!(f, "tags: {}", self.tags)?;
        writeln!(f, "data segments: {}", self.data_segments)?;
        writeln!(f, "element segments: {}", self.element_segments)?;
        if self.features.is_empty() {
            writeln!(f, "features: none")?;
        } else {
            writeln!(f, "features: {}", self.features.join(", "))?;
        }
        let host = self
            .imports
            .iter()
            .filter(|i| i.resolution == Resolution::HostTrait)
            .count();
        if self.needs_host_trait() {
            writeln!(f, "host trait: required ({host} import(s))")
        } else {
            writeln!(f, "host trait: not required")
        }
    }
}

/// Summarise `wasm` without generating code: function/import/export counts, how
/// each import would be resolved, memory limits, the proposals in use, and
/// whether the generated module needs a host `Imports` implementation.
///
/// Only a malformed binary is an error; a module that [`crate::transpile`] would
/// reject as unsupported can still be summarised.
pub fn dump(wasm: &[u8]) -> Result<ModuleSummary, TranspileError> {
    let mut sigs: Vec<(Vec<ValType>, Vec<ValType>)> = Vec::new();
    let mut s = ModuleSummary {
        imported_funcs: 0,
        defined_funcs: 0,
        imports: Vec::new(),
        exports: Vec::new(),
        memories: Vec::new(),
        tables: 0,
        globals: 0,
        tags: 0,
        data_segments: 0,
        element_segments: 0,
        features: Vec::new(),
    };
    let mut feat = Features::default();

    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::TypeSection(reader) => {
                for group in reader {
                    for sub in group?.types() {
                        match &sub.composite_type.inner {
                            CompositeInnerType::Func(ft) => {
                                sigs.push((ft.params().to_vec(), ft.results().to_vec()));
                            }
                            CompositeInnerType::Struct(_) | CompositeInnerType::Array(_) => {
                                feat.gc = true;
                                sigs.push((Vec::new(), Vec::new()));
                            }
                            _ => {
                                feat.cont = true;
                                sigs.push((Vec::new(), Vec::new()));
                            }
                        }
                    }
                }
            }
            Payload::ImportSection(reader) => {
                for group in reader {
                    match group? {
                        wasmparser::Imports::Single(_, i) => {
                            add_import(&mut s, &sigs, i.module, i.name, i.ty);
                        }
                        wasmparser::Imports::Compact1 { module, items } => {
                            for item in items {
                                let item = item?;
                                add_import(&mut s, &sigs, module, item.name, item.ty);
                            }
                        }
                        wasmparser::Imports::Compact2 { module, ty, names } => {
                            for name in names {
                                add_import(&mut s, &sigs, module, name?, ty);
                            }
                        }
                    }
                }
            }
            Payload::FunctionSection(reader) => {
                s.defined_funcs = s.defined_funcs.saturating_add(reader.count() as usize);
            }
            Payload::TableSection(reader) => {
                s.tables = s.tables.saturating_add(reader.count() as usize);
            }
            Payload::MemorySection(reader) => {
                for mem in reader {
                    s.memories.push(memory_line(&mem?, false));
                }
            }
            Payload::TagSection(reader) => {
                s.tags = s.tags.saturating_add(reader.count() as usize);
            }
            Payload::GlobalSection(reader) => {
                s.globals = s.globals.saturating_add(reader.count() as usize);
            }
            Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export?;
                    s.exports.push((
                        export.name.to_string(),
                        kind_name(export.kind),
                        export.index,
                    ));
                }
            }
            Payload::ElementSection(reader) => {
                s.element_segments = reader.count() as usize;
            }
            Payload::DataSection(reader) => {
                s.data_segments = reader.count() as usize;
            }
            Payload::CodeSectionEntry(body) => {
                for op in body.get_operators_reader()? {
                    feat.note(&op?);
                }
            }
            _ => {}
        }
    }

    feat.threads = s
        .imports
        .iter()
        .any(|i| i.module == "wasi" && i.name == "thread-spawn");
    feat.multi_memory = s.memories.len() > 1;
    s.features = feat.names();
    Ok(s)
}

fn add_import(
    s: &mut ModuleSummary,
    sigs: &[(Vec<ValType>, Vec<ValType>)],
    module: &str,
    name: &str,
    ty: TypeRef,
) {
    let (ty_text, resolution) = match ty {
        TypeRef::Func(idx) | TypeRef::FuncExact(idx) => {
            s.imported_funcs = s.imported_funcs.saturating_add(1);
            let (params, results) = sigs
                .get(idx as usize)
                .map_or((&[][..], &[][..]), |(p, r)| (p.as_slice(), r.as_slice()));
            let native = codegen::WasiFn::recognise(module, name, params, results).is_some();
            (
                format!("{} -> {}", types(params), types(results)),
                if native {
                    Resolution::NativeWasi
                } else {
                    Resolution::HostTrait
                },
            )
        }
        TypeRef::Global(GlobalType {
            content_type,
            mutable,
            ..
        }) => {
            s.globals = s.globals.saturating_add(1);
            let m = if mutable { "mut " } else { "" };
            (format!("global {m}{content_type}"), Resolution::HostTrait)
        }
        TypeRef::Memory(m) => {
            let line = memory_line(&m, true);
            let resolution = if m.shared {
                Resolution::SharedHandle
            } else {
                Resolution::HostTrait
            };
            s.memories.push(line);
            ("memory".to_string(), resolution)
        }
        TypeRef::Table(_) => {
            s.tables = s.tables.saturating_add(1);
            ("table".to_string(), Resolution::HostTrait)
        }
        TypeRef::Tag(_) => {
            s.tags = s.tags.saturating_add(1);
            ("tag".to_string(), Resolution::HostTrait)
        }
    };
    s.imports.push(ImportLine {
        module: module.to_string(),
        name: name.to_string(),
        ty: ty_text,
        resolution,
    });
}

fn memory_line(m: &MemoryType, imported: bool) -> MemoryLine {
    MemoryLine {
        min: m.initial,
        max: m.maximum,
        shared: m.shared,
        imported,
    }
}

fn types(tys: &[ValType]) -> String {
    let parts: Vec<String> = tys.iter().map(ToString::to_string).collect();
    format!("({})", parts.join(", "))
}

fn kind_name(kind: ExternalKind) -> &'static str {
    match kind {
        ExternalKind::Func | ExternalKind::FuncExact => "func",
        ExternalKind::Table => "table",
        ExternalKind::Memory => "memory",
        ExternalKind::Global => "global",
        ExternalKind::Tag => "tag",
    }
}

/// The proposals a module uses, found from its operators and types.
#[derive(Default)]
struct Features {
    atomics: bool,
    simd: bool,
    exceptions: bool,
    tail_call: bool,
    gc: bool,
    cont: bool,
    multi_memory: bool,
    threads: bool,
}

impl Features {
    /// Classify one operator by its `Debug` name (the variant name), which keeps
    /// this independent of wasmparser's per-proposal operator macros.
    fn note(&mut self, op: &Operator<'_>) {
        let text = format!("{op:?}");
        let name = text.split([' ', '{', '(']).next().unwrap_or("");
        if name.contains("Atomic") {
            self.atomics = true;
        } else if name.starts_with("V128") || is_lane_shape(name) {
            self.simd = true;
        } else if matches!(
            name,
            "Try"
                | "TryTable"
                | "Catch"
                | "CatchAll"
                | "Throw"
                | "ThrowRef"
                | "Rethrow"
                | "Delegate"
        ) {
            self.exceptions = true;
        } else if name.starts_with("ReturnCall") {
            self.tail_call = true;
        } else if name.starts_with("Struct")
            || name.starts_with("Array")
            || name.starts_with("RefTest")
            || name.starts_with("RefCast")
            || name.starts_with("BrOnCast")
            || matches!(name, "AnyConvertExtern" | "ExternConvertAny" | "RefI31")
            || name.starts_with("I31")
        {
            self.gc = true;
        } else if matches!(
            name,
            "ContNew"
                | "ContBind"
                | "Suspend"
                | "Resume"
                | "ResumeThrow"
                | "ResumeThrowRef"
                | "Switch"
        ) {
            self.cont = true;
        }
    }

    fn names(&self) -> Vec<&'static str> {
        [
            (self.atomics, "atomics"),
            (self.simd, "simd"),
            (self.exceptions, "exceptions"),
            (self.tail_call, "tail-call"),
            (self.gc, "gc"),
            (self.cont, "continuations"),
            (self.multi_memory, "multi-memory"),
            (self.threads, "wasi-threads"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect()
    }
}

/// Whether an operator name is a SIMD lane operation such as `I32x4Splat`.
fn is_lane_shape(name: &str) -> bool {
    ["I8x16", "I16x8", "I32x4", "I64x2", "F32x4", "F64x2"]
        .iter()
        .any(|p| name.starts_with(p))
}
