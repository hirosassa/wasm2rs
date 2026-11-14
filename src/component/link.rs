//! A component whose core modules are wired to each other (a preview1 adapter with
//! `wit-component`'s shim and fixup, or lifts from several modules) is transpiled as one
//! program: the modules are linked into a single [`ModuleIr`] (see `crate::link`) and the glue
//! is made for that unit. See `docs/core-module-linking-design.md`, stage L3.

use std::collections::BTreeMap;

use wasmparser::{ExternalKind, Parser, Payload, TypeRef};

use super::Layout;
use crate::WitAsyncAbi;
use crate::codegen;
use crate::link::{ImportSources, LinkModule, Source, link, rewrite_bodies};
use crate::parse::{ModuleIr, parse_module};
use crate::wit::{HostFunc, Supplier, WiredInstance, core_wiring, wit_host_glue_for_module};
use crate::{
    TranspileError, TranspileOptions, WitExport, WitOptions, transpile_ir_with_host_imports,
};

/// The name the linked unit exports the first lowering `realloc` that is not the lifts' under
/// (an adapter's `cabi_import_realloc`); the n-th (n > 0) is `cabi_import_realloc_{n}`. It is
/// not the module's own export name, which may be the `cabi_realloc` of the lifts as well.
const IMPORT_REALLOC: &str = "cabi_import_realloc";

fn import_realloc_name(n: usize) -> String {
    match n {
        0 => IMPORT_REALLOC.to_string(),
        n => format!("{IMPORT_REALLOC}_{n}"),
    }
}

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

    // The `realloc` of each function import the host provides, in the order of the linked
    // unit's host imports (the program's, then each appended module's).
    let mut host_reallocs: Vec<Option<(u32, String)>> = Vec::new();
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
                        // The module's own name for the import (`actual` `1` of the fixup)
                        // says nothing: the WIT function it lowers is the host's name for it.
                        if k > 0 {
                            rename(&mut ir, func_import, host)?;
                        }
                    }
                    if source == Source::Host {
                        host_reallocs.push(match &supplier {
                            Some(Supplier::Host(host)) => host.realloc.clone(),
                            _ => None,
                        });
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

    let mut plan = UnitExports::default();
    let (lifts, lift_realloc) = name_lifts(&members, exports, &mut plan)?;
    let import_reallocs = import_reallocs(&at, &host_reallocs, lift_realloc.as_ref(), &mut plan)?;
    plan.apply(&mut modules)?;

    let rewritten = rewrite_bodies(&modules)?;
    let ir: ModuleIr<'_> = link(modules, &rewritten)?;
    let (mut out, mut core_imports) = transpile_ir_with_host_imports(ir, transpile)?;
    // A host import the transpiler runs as native WASI is not in `core_imports`; that is
    // harmless while no import allocates through a `realloc` of its own.
    if core_imports.len() != import_reallocs.len() && import_reallocs.iter().any(Option::is_some) {
        return Err(TranspileError::Internal(
            "the linked unit's host imports do not match the component's lowered imports".into(),
        ));
    }
    if core_imports.len() == import_reallocs.len() {
        for (import, realloc) in core_imports.iter_mut().zip(import_reallocs) {
            import.realloc = realloc;
        }
    }
    out.push('\n');
    out.push_str(&wit_host_glue_for_module(
        component,
        wit,
        &core_imports,
        Some(lifts),
        super::has_imports_trait(&out),
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

/// For each host function import (`host_reallocs`, with the `realloc` its lowering names),
/// the export of the linked program it allocates its results through, or `None` for the lifts'
/// `cabi_realloc` (or when it does not allocate). Each distinct other `realloc` is exported
/// under [`import_realloc_name`].
///
/// An import whose lowering names no `realloc` takes the one `realloc` all the others share,
/// if there is exactly one.
fn import_reallocs(
    at: &BTreeMap<u32, usize>,
    host_reallocs: &[Option<(u32, String)>],
    lift_realloc: Option<&Realloc>,
    plan: &mut UnitExports,
) -> Result<Vec<Option<String>>, TranspileError> {
    let mut distinct: Vec<&(u32, String)> = Vec::new();
    for r in host_reallocs.iter().flatten() {
        if !distinct.contains(&r) {
            distinct.push(r);
        }
    }
    let mut exported = 0usize;
    let mut resolved: Vec<Option<String>> = Vec::with_capacity(distinct.len());
    for (instance, name) in &distinct {
        let provider = at.get(instance).copied().ok_or_else(|| {
            unsupported("the `realloc` of a lowered import is not an export of a core module")
        })?;
        let unit = if lift_realloc.is_some_and(|(k, own)| *k == provider && own == name) {
            None
        } else if lift_realloc.is_none() && provider == 0 && name == "cabi_realloc" {
            // The program's own export: the glue's default allocator.
            plan.add(provider, name, "cabi_realloc");
            None
        } else {
            let unit = import_realloc_name(exported);
            exported = exported.saturating_add(1);
            plan.add(provider, name, &unit);
            Some(unit)
        };
        resolved.push(unit);
    }
    let sole = match resolved.as_slice() {
        [only] => only.clone(),
        _ => None,
    };
    Ok(host_reallocs
        .iter()
        .map(|r| match r {
            Some(r) => distinct
                .iter()
                .position(|d| *d == r)
                .and_then(|i| resolved.get(i))
                .cloned()
                .flatten(),
            None => sole.clone(),
        })
        .collect())
}

/// The name the unit exports the core function of a lift under: the lift's own WIT name
/// (`interface#name`, as `wit-component` names it), which is unique in the component and does
/// not depend on what the core modules happen to call their exports.
fn unit_name(e: &WitExport) -> String {
    if e.func.interface.is_empty() {
        e.func.name.clone()
    } else {
        format!("{}#{}", e.func.interface, e.func.name)
    }
}

/// A `realloc` function: its member and the module's own export name.
type Realloc = (usize, String);

/// The exports of the linked unit: (member, the module's own export name, the unit's name).
/// The same function may be exported under several names.
#[derive(Default)]
struct UnitExports(Vec<(usize, String, String)>);

impl UnitExports {
    fn add(&mut self, member: usize, own: &str, unit: &str) {
        if !self
            .0
            .iter()
            .any(|(m, o, u)| *m == member && o == own && u == unit)
        {
            self.0.push((member, own.to_string(), unit.to_string()));
        }
    }

    /// Hands each module its exports: the program's replace its export list (so everything
    /// else it exports is dropped), an appended module's are carried.
    fn apply(self, modules: &mut [LinkModule<'_>]) -> Result<(), TranspileError> {
        let missing = || TranspileError::Internal("lifted module out of range".into());
        let program = modules.first_mut().ok_or_else(missing)?;
        let mut kept = Vec::new();
        for (m, own, unit) in &self.0 {
            if *m != 0 {
                continue;
            }
            let export = program
                .ir
                .exports
                .iter()
                .find(|e| e.name == *own)
                .ok_or_else(|| {
                    unsupported(format!("the program does not export function `{own}`"))
                })?;
            kept.push(codegen::ExportInfo {
                name: unit.clone(),
                func_index: export.func_index,
            });
        }
        program.ir.exports = kept;
        for (m, own, unit) in self.0 {
            if m != 0 {
                modules
                    .get_mut(m)
                    .ok_or_else(missing)?
                    .carried
                    .push((own, unit));
            }
        }
        Ok(())
    }
}

/// Decides the exports of the linked unit (decision Q4): what the component lifts, and the
/// `realloc` and `post-return` they name; the core modules' other exports are internal wiring
/// and are dropped. Returns the lifts with their core export names changed to the unit's, and
/// the function the lifts allocate through (the first lift's `realloc`).
///
/// The glue allocates through one `cabi_realloc` only, so the first lift's realloc is that and
/// the realloc of a lift from a different function is exported as `cabi_realloc_m{module}_{n}`,
/// which the glue refuses to use when the lift allocates.
fn name_lifts(
    members: &[Member<'_>],
    exports: &[WitExport],
    plan: &mut UnitExports,
) -> Result<(Vec<WitExport>, Option<Realloc>), TranspileError> {
    let mut lifts = Vec::with_capacity(exports.len());
    let mut lift_realloc: Option<Realloc> = None;
    // The other reallocs, numbered so that two in one module stay apart.
    let mut others: Vec<Realloc> = Vec::new();
    for e in exports {
        let Some(k) = members.iter().position(|m| m.ordinal == e.core_module) else {
            return Err(TranspileError::Internal(format!(
                "export `{}` is lifted from a module that is not linked",
                e.core_export
            )));
        };
        let unit = unit_name(e);
        plan.add(k, &e.core_export, &unit);
        let mut lift = e.clone();
        lift.core_export = unit.clone();
        if let Some(WitAsyncAbi::Callback { callback }) = &e.async_abi {
            // Lifts sharing a callback share one export of the unit.
            let name = format!("cabi_callback_m{}_{callback}", e.core_module);
            plan.add(k, callback, &name);
            lift.async_abi = Some(WitAsyncAbi::Callback { callback: name });
        }
        if let Some(own) = &e.post_return {
            let name = format!("cabi_post_{unit}");
            plan.add(k, own, &name);
            lift.post_return = Some(name);
        }
        if let Some(own) = &e.realloc {
            let first = lift_realloc.get_or_insert_with(|| (k, own.clone()));
            let name = if (first.0, first.1.as_str()) == (k, own.as_str()) {
                "cabi_realloc".to_string()
            } else {
                let at = match others
                    .iter()
                    .position(|(m, o)| (*m, o.as_str()) == (k, own.as_str()))
                {
                    Some(at) => at,
                    None => {
                        others.push((k, own.clone()));
                        others.len()
                    }
                };
                format!("cabi_realloc_m{}_{at}", e.core_module)
            };
            plan.add(k, own, &name);
            lift.realloc = Some(name);
        }
        lifts.push(lift);
    }
    Ok((lifts, lift_realloc))
}
