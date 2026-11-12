//! A component whose core modules are wired to each other (a preview1 adapter with
//! `wit-component`'s shim and fixup, or lifts from several modules) is transpiled as one
//! program: the modules are linked into a single [`ModuleIr`] (see `crate::link`) and the glue
//! is made for that unit. See `docs/core-module-linking-design.md`, stage L3.

use std::collections::BTreeMap;

use wasmparser::{ExternalKind, Parser, Payload, TypeRef};

use super::Layout;
use crate::link::{ImportSources, LinkModule, Source, link, rewrite_bodies};
use crate::parse::{ModuleIr, parse_module};
use crate::wit::{HostFunc, Supplier, WiredInstance, core_wiring, wit_host_glue_for_module};
use crate::{
    TranspileError, TranspileOptions, WitExport, WitOptions, transpile_ir_with_host_imports,
};

/// The name the linked unit exports an adapter's lowering `realloc` under. It is not the
/// adapter's own export name, which may be the `cabi_realloc` of the lifts as well.
const IMPORT_REALLOC: &str = "cabi_import_realloc";

fn unsupported(what: impl Into<String>) -> TranspileError {
    TranspileError::Unsupported(what.into())
}

/// The imports and exports of a core module, read from its bytes.
struct ModuleInfo {
    imports: Vec<(String, String, TypeRef)>,
    exports: Vec<(String, ExternalKind, u32)>,
    defines_memory: bool,
}

impl ModuleInfo {
    fn read(bytes: &[u8]) -> Result<Self, TranspileError> {
        let mut info = Self {
            imports: Vec::new(),
            exports: Vec::new(),
            defines_memory: false,
        };
        for payload in Parser::new(0).parse_all(bytes) {
            match payload? {
                Payload::ImportSection(reader) => {
                    for import in reader.into_imports() {
                        let import = import?;
                        info.imports.push((
                            import.module.to_string(),
                            import.name.to_string(),
                            import.ty,
                        ));
                    }
                }
                Payload::ExportSection(reader) => {
                    for export in reader {
                        let export = export?;
                        info.exports
                            .push((export.name.to_string(), export.kind, export.index));
                    }
                }
                Payload::MemorySection(reader) => info.defines_memory |= reader.count() > 0,
                _ => {}
            }
        }
        Ok(info)
    }

    fn export(&self, name: &str) -> Option<(ExternalKind, u32)> {
        self.exports
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, kind, index)| (*kind, *index))
    }
}

/// An instantiated core module of the component.
struct Member<'a> {
    /// Its core instance index.
    instance: u32,
    /// Its ordinal among the component's core modules.
    ordinal: usize,
    bytes: &'a [u8],
    info: ModuleInfo,
}

/// Transpiles the component's core modules as one linked program and appends the glue.
pub(super) fn transpile_linked(
    component: &[u8],
    layout: &Layout,
    exports: &[WitExport],
    transpile: &TranspileOptions,
    wit: &WitOptions,
) -> Result<String, TranspileError> {
    let wiring = core_wiring(component)?;
    let mut members = Vec::new();
    for (i, instance) in wiring.instances.iter().enumerate() {
        let WiredInstance::Module { module, .. } = instance else {
            continue;
        };
        let (ordinal, bytes) = module
            .and_then(|m| layout.modules.get(m).map(|b| (m, b)))
            .ok_or_else(|| {
                unsupported("a core instance of the component instantiates a module it cannot read")
            })?;
        members.push(Member {
            instance: u32::try_from(i)
                .map_err(|_| TranspileError::Internal("too many core instances".into()))?,
            ordinal,
            bytes,
            info: ModuleInfo::read(bytes)?,
        });
    }
    // The program keeps its indices: it is the first module that owns a memory (the adapter
    // imports its memory from it, the shim and fixup have none).
    let main = members
        .iter()
        .position(|m| m.info.defines_memory)
        .unwrap_or(0);
    if main > 0 {
        let program = members.remove(main);
        members.insert(0, program);
    }
    // The glue reads and writes memory 0 only, so a memory of an appended module would be
    // addressed wrongly.
    if members.iter().skip(1).any(|m| m.info.defines_memory) {
        return Err(unsupported(
            "a core module other than the program defines its own memory; linked modules must \
             share the program's memory",
        ));
    }
    let at: BTreeMap<u32, usize> = members
        .iter()
        .enumerate()
        .map(|(k, m)| (m.instance, k))
        .collect();

    let mut reallocs: Vec<(u32, String)> = Vec::new();
    let mut modules = Vec::with_capacity(members.len());
    for (k, member) in members.iter().enumerate() {
        let mut ir = parse_module(member.bytes, transpile)?;
        let mut sources = ImportSources::default();
        let mut func_import = 0usize;
        for (module, name, ty) in &member.info.imports {
            let supplier = wiring.supplier(member.instance, module, name);
            let source = match &supplier {
                Some(Supplier::Export { instance, name }) => {
                    let (provider, member) = at
                        .get(instance)
                        .and_then(|&p| members.get(p).map(|m| (p, m)))
                        .ok_or_else(|| {
                            unsupported(format!(
                                "the import `{module}` `{name}` is provided by something that \
                                 is not a core module of the component"
                            ))
                        })?;
                    from_export(provider, &member.info, name, *ty)?
                }
                _ => Source::Host,
            };
            match ty {
                TypeRef::Func(_) | TypeRef::FuncExact(_) => {
                    if let Some(Supplier::Host(host)) = &supplier {
                        if let Some(r) = host.realloc.as_ref().filter(|r| !reallocs.contains(r)) {
                            reallocs.push(r.clone());
                        }
                        // The module's own name for the import (`actual` `1` of the fixup)
                        // says nothing: the WIT function it lowers is the host's name for it.
                        if k > 0 {
                            rename(&mut ir, func_import, host)?;
                        }
                    }
                    func_import = func_import.saturating_add(1);
                    sources.funcs.push(source);
                }
                TypeRef::Global(_) => sources.globals.push(source),
                TypeRef::Memory(_) => sources.memory = Some(source),
                TypeRef::Table(_) => sources.tables.push(source),
                TypeRef::Tag(_) => {
                    return Err(unsupported(format!(
                        "the exception tag import `{module}` `{name}` of a linked core module"
                    )));
                }
            }
        }
        modules.push(LinkModule {
            ir,
            sources,
            carried: Vec::new(),
        });
    }

    let import_realloc = import_realloc(&at, &reallocs, &mut modules)?;
    carry_lifts(&members, exports, &mut modules)?;

    let rewritten = rewrite_bodies(&modules)?;
    let ir: ModuleIr<'_> = link(modules, &rewritten)?;
    let (mut out, core_imports) = transpile_ir_with_host_imports(ir, transpile)?;
    out.push('\n');
    out.push_str(&wit_host_glue_for_module(
        component,
        wit,
        &core_imports,
        import_realloc.as_deref(),
    )?);
    Ok(out)
}

/// What an export of a provider module (link index `module`) is, for an import of kind `ty`.
fn from_export(
    module: usize,
    info: &ModuleInfo,
    name: &str,
    ty: TypeRef,
) -> Result<Source, TranspileError> {
    let (kind, index) = info
        .export(name)
        .ok_or_else(|| unsupported(format!("a linked core module does not export `{name}`")))?;
    match (ty, kind) {
        (TypeRef::Func(_) | TypeRef::FuncExact(_), ExternalKind::Func) => {
            Ok(Source::Func { module, index })
        }
        (TypeRef::Global(_), ExternalKind::Global) => Ok(Source::Global { module, index }),
        (TypeRef::Memory(_), ExternalKind::Memory) => Ok(Source::Memory { module, index }),
        (TypeRef::Table(_), ExternalKind::Table) => Ok(Source::Table { module, index }),
        _ => Err(unsupported(format!(
            "the export `{name}` of a linked core module is not the kind of item that imports it"
        ))),
    }
}

/// Names the `func_import`-th function import of `ir` after the WIT function it lowers.
fn rename(
    ir: &mut ModuleIr<'_>,
    func_import: usize,
    host: &HostFunc,
) -> Result<(), TranspileError> {
    let import = ir
        .imports
        .get_mut(func_import)
        .ok_or_else(|| TranspileError::Internal("function import out of range".into()))?;
    import.module.clone_from(&host.interface);
    import.field.clone_from(&host.name);
    Ok(())
}

/// The export of the linked program that the lowered imports allocate through, or `None` when
/// it is the lifts' `cabi_realloc` (or nothing allocates). An appended module's is carried
/// under [`IMPORT_REALLOC`].
fn import_realloc(
    at: &BTreeMap<u32, usize>,
    reallocs: &[(u32, String)],
    modules: &mut [LinkModule<'_>],
) -> Result<Option<String>, TranspileError> {
    let [(instance, name)] = reallocs else {
        return if reallocs.is_empty() {
            Ok(None)
        } else {
            Err(unsupported(
                "the component's imports are lowered with different `realloc` functions",
            ))
        };
    };
    let provider = at.get(instance).copied().ok_or_else(|| {
        unsupported("the `realloc` of a lowered import is not an export of a core module")
    })?;
    if provider == 0 {
        // The program's own export, already part of the unit.
        return Ok((name != "cabi_realloc").then(|| name.clone()));
    }
    modules
        .get_mut(provider)
        .ok_or_else(|| TranspileError::Internal("realloc provider out of range".into()))?
        .carried
        .push((name.clone(), IMPORT_REALLOC.to_string()));
    Ok(Some(IMPORT_REALLOC.to_string()))
}

/// Keeps the core exports the component lifts (and their `realloc` and `post-return`) from
/// the modules that are appended to the program, so the glue can call them.
fn carry_lifts(
    members: &[Member<'_>],
    exports: &[WitExport],
    modules: &mut [LinkModule<'_>],
) -> Result<(), TranspileError> {
    for e in exports {
        let Some(k) = members.iter().position(|m| m.ordinal == e.core_module) else {
            return Err(TranspileError::Internal(format!(
                "export `{}` is lifted from a module that is not linked",
                e.core_export
            )));
        };
        if k == 0 {
            continue;
        }
        let carried = &mut modules
            .get_mut(k)
            .ok_or_else(|| TranspileError::Internal("lifted module out of range".into()))?
            .carried;
        for name in [
            Some(&e.core_export),
            e.realloc.as_ref(),
            e.post_return.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if !carried.iter().any(|(own, to)| own == name && to == name) {
                carried.push((name.clone(), name.clone()));
            }
        }
    }
    Ok(())
}
