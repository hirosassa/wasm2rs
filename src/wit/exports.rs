//! The export side of a component: the `canon lift`s behind its exported functions,
//! resolved to the names of core module exports (see `docs/component-exports-design.md`).
//!
//! Only the direct shape is read: a lifted core function that is an export of a core
//! instance made by instantiating a top-level core module. Anything else is an
//! [`Unsupported`](TranspileError::Unsupported) error naming the export.

use wasmparser::{
    CanonicalFunction, CanonicalOption, ComponentAlias, ComponentExternalKind,
    ComponentOuterAliasKind, ComponentTypeRef, ExternalKind, Instance as CoreInstanceDef, Payload,
};

use super::WitFunc;
use super::read::{Def, Scope, Sig, Soft, def_at, innermost, read_component, unsupported};
use super::wiring::{CoreWiring, HostFunc, Supplier, WiredInstance};
use crate::TranspileError;

/// A function the component exports, and the core module export it is lifted from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitExport {
    /// The function. `interface` is the exported interface instance (e.g.
    /// `wasi:cli/run@0.2.0`), or empty for a function exported at the top level of the
    /// component.
    pub func: WitFunc,
    /// The index of the core module among the component's top-level core modules.
    pub core_module: usize,
    /// The name under which that module exports the lifted function, e.g.
    /// `wasi:cli/run@0.2.0#run`.
    pub core_export: String,
    /// The module's memory export named by the lift, if any.
    pub memory: Option<String>,
    /// The module's export of the `realloc` function named by the lift, if any.
    pub realloc: Option<String>,
    /// The module's export of the `post-return` function named by the lift, if any.
    pub post_return: Option<String>,
    /// Whether strings are UTF-8, the only encoding read (others are rejected).
    pub utf8: bool,
}

/// Every function exported by `component`, in export order.
///
/// Each one is traced to the name of the core module export it lifts (and to those of its
/// `memory`, `realloc` and `post-return` options). A lift that is not a direct export of
/// an instantiated top-level core module, a string encoding other than UTF-8, or a type
/// this reader does not model is an [`Unsupported`](TranspileError::Unsupported) error
/// naming the export. A core module is an error too.
pub fn wit_exports(component: &[u8]) -> Result<Vec<WitExport>, TranspileError> {
    let mut reader = Reader::default();
    read_component(component, Some(&mut reader))?;
    reader.resolve()
}

/// The core instance graph of `component` (see [`CoreWiring`]).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by the linker in stage L3")
)]
pub(crate) fn core_wiring(component: &[u8]) -> Result<CoreWiring, TranspileError> {
    let mut reader = Reader::default();
    read_component(component, Some(&mut reader))?;
    Ok(reader.wiring())
}

/// A function of the component's function index space.
#[derive(Clone)]
enum Func {
    Lift(Lift),
    /// An alias of an export of an imported instance: a function the host provides.
    Imported(HostFunc),
    /// Anything else (an import, an alias of another instance's export).
    Other(&'static str),
}

const ALIAS_OF_INSTANCE_EXPORT: &str = "an alias of an export of an instance";

impl Default for Func {
    fn default() -> Self {
        Self::Other("an unknown item")
    }
}

#[derive(Clone)]
struct Lift {
    sig: Soft<Sig>,
    core_func: u32,
    options: Vec<CanonicalOption>,
}

/// A function of the core function index space.
#[derive(Clone)]
enum CoreFunc {
    /// An alias of an export of a core instance.
    Export { instance: u32, name: String },
    /// A `canon lower` of this function of the component's function index space.
    Lowered { func: u32 },
    /// Another canonical built-in.
    Builtin,
}

#[derive(Clone)]
enum CoreInstance {
    /// An instantiation of the core module with this index of the module index space, with
    /// the core instances given as arguments by import module name.
    Module {
        module: usize,
        args: Vec<(String, u32)>,
    },
    /// An instance made from exports only (a shim or adapter): `(name, kind, index in the
    /// core index space of that kind)`.
    FromExports(Vec<(String, ExternalKind, u32)>),
}

/// An instance of the component's instance index space.
#[derive(Clone, Default)]
enum Instance {
    /// Made from inline exports: the functions among them, by name.
    Exports(Vec<(String, u32)>),
    /// An instantiation of the top-level component with this index, with the functions
    /// among its arguments by name.
    Instantiated {
        component: usize,
        args: Vec<(String, u32)>,
    },
    /// An imported instance (an interface the host provides), by import name.
    Imported(String),
    /// An alias.
    #[default]
    Other,
}

/// A top-level component. `wit-component` exports an interface as an instance of a *shim*
/// component that only re-exports its function imports; that is the only shape read.
#[derive(Clone, Default)]
struct Shim {
    /// The functions it exports, as `(export name, name of the import it re-exports)`.
    funcs: Vec<(String, Option<String>)>,
    /// It defines something of its own (functions, instances, or inner modules and components).
    opaque: bool,
}

impl Shim {
    /// A component whose contents are not read (an import or an alias).
    fn opaque() -> Self {
        Self {
            opaque: true,
            ..Self::default()
        }
    }
}

/// The component func index space of a shim being read.
#[derive(Default)]
struct ShimBuilder {
    shim: Shim,
    /// The shim's function index space: the name of the import, for an imported function.
    funcs: Vec<Option<String>>,
}

enum Exported {
    Func { name: String, index: u32 },
    Instance { name: String, index: u32 },
}

/// A core module defined in the body of the component.
#[derive(Clone)]
struct TopModule {
    /// Its index among the top-level core modules (not in the module index space, which an
    /// export or an alias of a module also grows).
    ordinal: usize,
    exports: Vec<(String, ExternalKind)>,
}

/// The module or component being skipped through, with its nesting depth.
struct Inner {
    depth: usize,
    kind: InnerKind,
}

enum InnerKind {
    Module(Vec<(String, ExternalKind)>),
    Component(ShimBuilder),
}

/// The index spaces and exports of the outermost component, built from its payloads.
#[derive(Default)]
pub(super) struct Reader {
    funcs: Vec<Func>,
    core_funcs: Vec<CoreFunc>,
    core_memories: Vec<(u32, String)>,
    core_tables: Vec<(u32, String)>,
    core_globals: Vec<(u32, String)>,
    core_instances: Vec<CoreInstance>,
    instances: Vec<Instance>,
    /// The core module index space; `None` for a module that is not one of the top-level
    /// core modules (an import, or an alias of an instance's export).
    modules: Vec<Option<TopModule>>,
    /// How many top-level core modules have been read.
    top_modules: usize,
    components: Vec<Shim>,
    exported: Vec<Exported>,
    inner: Option<Inner>,
}

fn index(i: u32) -> Option<usize> {
    usize::try_from(i).ok()
}

/// Pushes a copy of the item that an export or an outer alias re-publishes, so the
/// index spaces stay aligned.
fn republish<T: Clone + Default>(space: &mut Vec<T>, from: u32) {
    let item = index(from)
        .and_then(|i| space.get(i))
        .cloned()
        .unwrap_or_default();
    space.push(item);
}

impl Reader {
    /// Reads one payload of the component: `stack` holds the type index spaces so far.
    pub(super) fn feed(
        &mut self,
        payload: &Payload<'_>,
        stack: &[Scope],
    ) -> Result<(), TranspileError> {
        if self.inner.is_some() {
            return self.feed_inner(payload);
        }
        match payload {
            Payload::ModuleSection { .. } => {
                self.inner = Some(Inner {
                    depth: 1,
                    kind: InnerKind::Module(Vec::new()),
                });
            }
            Payload::ComponentSection { .. } => {
                self.inner = Some(Inner {
                    depth: 1,
                    kind: InnerKind::Component(ShimBuilder::default()),
                });
            }
            Payload::InstanceSection(reader) => {
                for instance in reader.clone() {
                    self.core_instances.push(match instance? {
                        CoreInstanceDef::Instantiate { module_index, args } => {
                            CoreInstance::Module {
                                module: index(module_index).unwrap_or(usize::MAX),
                                args: args.iter().map(|a| (a.name.to_string(), a.index)).collect(),
                            }
                        }
                        CoreInstanceDef::FromExports(exports) => CoreInstance::FromExports(
                            exports
                                .iter()
                                .map(|e| (e.name.to_string(), e.kind, e.index))
                                .collect(),
                        ),
                    });
                }
            }
            Payload::ComponentInstanceSection(reader) => {
                for instance in reader.clone() {
                    self.instances.push(match instance? {
                        wasmparser::ComponentInstance::Instantiate {
                            component_index,
                            args,
                        } => Instance::Instantiated {
                            component: index(component_index).unwrap_or(usize::MAX),
                            args: args
                                .iter()
                                .filter(|a| a.kind == ComponentExternalKind::Func)
                                .map(|a| (a.name.to_string(), a.index))
                                .collect(),
                        },
                        wasmparser::ComponentInstance::FromExports(items) => Instance::Exports(
                            items
                                .iter()
                                .filter(|e| e.kind == ComponentExternalKind::Func)
                                .map(|e| (e.name.full_name().into_owned(), e.index))
                                .collect(),
                        ),
                    });
                }
            }
            Payload::ComponentAliasSection(reader) => {
                for alias in reader.clone() {
                    self.alias(&alias?);
                }
            }
            Payload::ComponentCanonicalSection(reader) => {
                for func in reader.clone() {
                    self.canonical(func?, stack)?;
                }
            }
            Payload::ComponentImportSection(reader) => {
                for import in reader.clone() {
                    let import = import?;
                    match import.ty {
                        ComponentTypeRef::Func(_) => self.funcs.push(Func::Other("an import")),
                        ComponentTypeRef::Instance(_) => self
                            .instances
                            .push(Instance::Imported(import.name.full_name().into_owned())),
                        // Counted as in `transpile_component`.
                        ComponentTypeRef::Module(_) => self.modules.push(None),
                        // An opaque shim, as for an alias of a component.
                        ComponentTypeRef::Component(_) => self.components.push(Shim::opaque()),
                        _ => {}
                    }
                }
            }
            Payload::ComponentExportSection(reader) => {
                for export in reader.clone() {
                    let export = export?;
                    let name = export.name.full_name().into_owned();
                    match export.kind {
                        ComponentExternalKind::Func => {
                            republish(&mut self.funcs, export.index);
                            self.exported.push(Exported::Func {
                                name,
                                index: export.index,
                            });
                        }
                        ComponentExternalKind::Instance => {
                            republish(&mut self.instances, export.index);
                            self.exported.push(Exported::Instance {
                                name,
                                index: export.index,
                            });
                        }
                        ComponentExternalKind::Module => {
                            republish(&mut self.modules, export.index);
                        }
                        ComponentExternalKind::Component => {
                            republish(&mut self.components, export.index);
                        }
                        ComponentExternalKind::Type | ComponentExternalKind::Value => {}
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn alias(&mut self, alias: &ComponentAlias<'_>) {
        match alias {
            ComponentAlias::InstanceExport {
                kind,
                instance_index,
                name,
            } => match kind {
                ComponentExternalKind::Func => {
                    let func = match index(*instance_index).and_then(|i| self.instances.get(i)) {
                        Some(Instance::Imported(interface)) => Func::Imported(HostFunc {
                            interface: interface.clone(),
                            name: (*name).to_string(),
                        }),
                        _ => Func::Other(ALIAS_OF_INSTANCE_EXPORT),
                    };
                    self.funcs.push(func);
                }
                ComponentExternalKind::Instance => self.instances.push(Instance::Other),
                ComponentExternalKind::Module => self.modules.push(None),
                ComponentExternalKind::Component => self.components.push(Shim::opaque()),
                _ => {}
            },
            ComponentAlias::CoreInstanceExport {
                kind,
                instance_index,
                name,
            } => match kind {
                ExternalKind::Func => self.core_funcs.push(CoreFunc::Export {
                    instance: *instance_index,
                    name: (*name).to_string(),
                }),
                ExternalKind::Memory => self
                    .core_memories
                    .push((*instance_index, (*name).to_string())),
                ExternalKind::Table => self
                    .core_tables
                    .push((*instance_index, (*name).to_string())),
                ExternalKind::Global => self
                    .core_globals
                    .push((*instance_index, (*name).to_string())),
                _ => {}
            },
            // An outer alias at the top level can only name the component's own items.
            ComponentAlias::Outer { kind, index, .. } => match kind {
                ComponentOuterAliasKind::CoreModule => republish(&mut self.modules, *index),
                ComponentOuterAliasKind::Component => republish(&mut self.components, *index),
                _ => {}
            },
        }
    }

    fn canonical(
        &mut self,
        func: CanonicalFunction,
        stack: &[Scope],
    ) -> Result<(), TranspileError> {
        match func {
            CanonicalFunction::Lift {
                core_func_index,
                type_index,
                options,
            } => {
                let sig = match def_at(innermost(stack)?, type_index) {
                    Ok(Def::Func(sig)) => (**sig).clone(),
                    Ok(_) => Err("a lift whose type is not a function type".into()),
                    Err(e) => Err(e),
                };
                self.funcs.push(Func::Lift(Lift {
                    sig,
                    core_func: core_func_index,
                    options: options.to_vec(),
                }));
            }
            CanonicalFunction::Lower { func_index, .. } => {
                self.core_funcs.push(CoreFunc::Lowered { func: func_index })
            }
            _ => self.core_funcs.push(CoreFunc::Builtin),
        }
        Ok(())
    }

    /// A payload inside a top-level module or component.
    fn feed_inner(&mut self, payload: &Payload<'_>) -> Result<(), TranspileError> {
        let Some(inner) = self.inner.as_mut() else {
            return Ok(());
        };
        match payload {
            Payload::ModuleSection { .. } | Payload::ComponentSection { .. } => {
                inner.depth = inner.depth.saturating_add(1);
                if let InnerKind::Component(b) = &mut inner.kind {
                    b.shim.opaque = true;
                }
            }
            Payload::End(_) => {
                inner.depth = inner.depth.saturating_sub(1);
                if inner.depth == 0 {
                    match self.inner.take().map(|i| i.kind) {
                        Some(InnerKind::Module(exports)) => {
                            self.modules.push(Some(TopModule {
                                ordinal: self.top_modules,
                                exports,
                            }));
                            self.top_modules = self.top_modules.saturating_add(1);
                        }
                        Some(InnerKind::Component(b)) => self.components.push(b.shim),
                        None => {}
                    }
                }
            }
            // Only the direct children of the module or component are read.
            _ if inner.depth > 1 => {}
            Payload::ExportSection(reader) => {
                if let InnerKind::Module(exports) = &mut inner.kind {
                    for export in reader.clone() {
                        let export = export?;
                        exports.push((export.name.to_string(), export.kind));
                    }
                }
            }
            Payload::ComponentImportSection(reader) => {
                if let InnerKind::Component(b) = &mut inner.kind {
                    for import in reader.clone() {
                        let import = import?;
                        if matches!(import.ty, ComponentTypeRef::Func(_)) {
                            b.funcs.push(Some(import.name.full_name().into_owned()));
                        }
                    }
                }
            }
            Payload::ComponentExportSection(reader) => {
                if let InnerKind::Component(b) = &mut inner.kind {
                    for export in reader.clone() {
                        let export = export?;
                        if export.kind == ComponentExternalKind::Func {
                            let source = index(export.index)
                                .and_then(|i| b.funcs.get(i))
                                .cloned()
                                .flatten();
                            b.shim
                                .funcs
                                .push((export.name.full_name().into_owned(), source.clone()));
                            b.funcs.push(source);
                        }
                    }
                }
            }
            // Anything else that can define a function or an instance makes the shim
            // something other than a re-export of its imports.
            Payload::ComponentCanonicalSection(_)
            | Payload::ComponentAliasSection(_)
            | Payload::ComponentInstanceSection(_)
            | Payload::InstanceSection(_) => {
                if let InnerKind::Component(b) = &mut inner.kind {
                    b.shim.opaque = true;
                }
            }
            _ => {}
        }
        Ok(())
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "consumed by the linker in stage L3")
    )]
    fn wiring(&self) -> CoreWiring {
        let instances = self
            .core_instances
            .iter()
            .map(|instance| match instance {
                CoreInstance::Module { module, args } => WiredInstance::Module {
                    module: self
                        .modules
                        .get(*module)
                        .and_then(Option::as_ref)
                        .map(|m| m.ordinal),
                    args: args.clone(),
                },
                CoreInstance::FromExports(members) => WiredInstance::Exports(
                    members
                        .iter()
                        .map(|(name, kind, at)| (name.clone(), self.supplier(*kind, *at)))
                        .collect(),
                ),
            })
            .collect();
        CoreWiring { instances }
    }

    /// What provides the core item `at` of the index space of `kind`.
    fn supplier(&self, kind: ExternalKind, at: u32) -> Supplier {
        let space = |items: &[(u32, String)]| {
            index(at)
                .and_then(|i| items.get(i))
                .map(|(instance, name)| Supplier::export(*instance, name))
        };
        let found = match kind {
            ExternalKind::Func => match index(at).and_then(|i| self.core_funcs.get(i)) {
                Some(CoreFunc::Export { instance, name }) => {
                    Some(Supplier::export(*instance, name))
                }
                Some(CoreFunc::Lowered { func }) => {
                    match index(*func).and_then(|i| self.funcs.get(i)) {
                        Some(Func::Imported(host)) => Some(Supplier::Host(host.clone())),
                        _ => None,
                    }
                }
                _ => None,
            },
            ExternalKind::Memory => space(&self.core_memories),
            ExternalKind::Table => space(&self.core_tables),
            ExternalKind::Global => space(&self.core_globals),
            _ => None,
        };
        found.unwrap_or(Supplier::Other)
    }

    fn resolve(&self) -> Result<Vec<WitExport>, TranspileError> {
        let mut out = Vec::new();
        for exported in &self.exported {
            match exported {
                Exported::Func { name, index } => out.push(self.resolve_func("", name, *index)?),
                Exported::Instance { name, index } => {
                    for (func, index) in self.instance_funcs(name, *index)? {
                        out.push(self.resolve_func(name, &func, index)?);
                    }
                }
            }
        }
        Ok(out)
    }

    /// The functions of the exported instance `name`, as `(name, func index)`.
    fn instance_funcs(&self, name: &str, at: u32) -> Result<Vec<(String, u32)>, TranspileError> {
        match index(at).and_then(|i| self.instances.get(i)) {
            Some(Instance::Exports(funcs)) => Ok(funcs.clone()),
            Some(Instance::Instantiated { component, args }) => {
                let shim = self
                    .components
                    .get(*component)
                    .filter(|s| !s.opaque)
                    .ok_or_else(|| {
                    unsupported(format!(
                        "export `{name}` is an instance of a nested component that does more than \
                         re-export its imports (nested component exports are not traced)"
                    ))
                })?;
                shim.funcs
                    .iter()
                    .map(|(export, import)| {
                        let arg = import
                            .as_ref()
                            .and_then(|i| args.iter().find(|(n, _)| n == i));
                        arg.map(|(_, index)| (export.clone(), *index))
                            .ok_or_else(|| {
                                unsupported(format!(
                                    "export `{name}#{export}` is not a function passed through a \
                                 nested component"
                                ))
                            })
                    })
                    .collect()
            }
            _ => Err(unsupported(format!(
                "export `{name}` is an instance that is not made of this component's own \
                 functions (an import or an alias)"
            ))),
        }
    }

    fn resolve_func(
        &self,
        interface: &str,
        name: &str,
        at: u32,
    ) -> Result<WitExport, TranspileError> {
        let label = if interface.is_empty() {
            name.to_string()
        } else {
            format!("{interface}#{name}")
        };
        let lift = match index(at).and_then(|i| self.funcs.get(i)) {
            Some(Func::Lift(lift)) => lift,
            Some(other) => {
                let what = match other {
                    Func::Imported(_) => ALIAS_OF_INSTANCE_EXPORT,
                    Func::Other(what) => what,
                    Func::Lift(_) => return Err(TranspileError::Internal("a lift".into())),
                };
                return Err(unsupported(format!(
                    "export `{label}` is {what}, not a `canon lift` in this component \
                     (exports of a nested component are not traced)"
                )));
            }
            None => return Err(unsupported(format!("export `{label}` has no function"))),
        };
        let (params, result) = lift
            .sig
            .clone()
            .map_err(|what| unsupported(format!("{what} in function `{name}` of `{interface}`")))?;

        let ((module, instance), core_export) =
            self.core_export(&label, lift.core_func, "function")?;
        let core_module = self.top_module(&label, module)?.ordinal;
        let mut export = WitExport {
            func: WitFunc {
                interface: interface.to_string(),
                name: name.to_string(),
                params,
                result,
            },
            core_module,
            core_export,
            memory: None,
            realloc: None,
            post_return: None,
            utf8: true,
        };
        for option in &lift.options {
            match option {
                CanonicalOption::UTF8 => {}
                CanonicalOption::UTF16 | CanonicalOption::CompactUTF16 => {
                    return Err(unsupported(format!(
                        "export `{label}` uses a string encoding other than utf8"
                    )));
                }
                CanonicalOption::Memory(i) => {
                    let found = index(*i).and_then(|i| self.core_memories.get(i));
                    let Some((_, name)) = found.filter(|(from, _)| *from == instance) else {
                        return Err(unsupported(format!(
                            "export `{label}`: its memory is not an export of the same core \
                             instance as the function"
                        )));
                    };
                    self.check_exists(&label, module, name, ExternalKind::Memory)?;
                    export.memory = Some(name.clone());
                }
                CanonicalOption::Realloc(i) => {
                    export.realloc = Some(self.option_export(&label, *i, "realloc", instance)?);
                }
                CanonicalOption::PostReturn(i) => {
                    export.post_return =
                        Some(self.option_export(&label, *i, "post-return", instance)?);
                }
                CanonicalOption::Async
                | CanonicalOption::Callback(_)
                | CanonicalOption::CoreType(_)
                | CanonicalOption::Gc => {
                    return Err(unsupported(format!(
                        "export `{label}` uses a canonical option this reader does not model \
                         ({option:?})"
                    )));
                }
            }
        }
        Ok(export)
    }

    /// The export name of the `what` option's core function `at`, which must come from
    /// the same core instance as the lifted function.
    fn option_export(
        &self,
        label: &str,
        at: u32,
        what: &str,
        instance: u32,
    ) -> Result<String, TranspileError> {
        let ((_, from), name) = self.core_export(label, at, what)?;
        if from != instance {
            return Err(unsupported(format!(
                "export `{label}`: its {what} is not an export of the same core instance as \
                 the function"
            )));
        }
        Ok(name)
    }

    /// The top-level module `(index, core instance)` and export name of core function `at`,
    /// which must be a function export of that module.
    fn core_export(
        &self,
        label: &str,
        at: u32,
        what: &str,
    ) -> Result<((usize, u32), String), TranspileError> {
        let (instance, name) = match index(at).and_then(|i| self.core_funcs.get(i)) {
            Some(CoreFunc::Export { instance, name }) => (*instance, name),
            Some(CoreFunc::Lowered { .. }) => {
                return Err(unsupported(format!(
                    "export `{label}`: its {what} is the result of a canon lower, not a core \
                     module export"
                )));
            }
            _ => {
                return Err(unsupported(format!(
                    "export `{label}`: its {what} is not an export of a core instance"
                )));
            }
        };
        let Some(&CoreInstance::Module { module, .. }) =
            index(instance).and_then(|i| self.core_instances.get(i))
        else {
            return Err(unsupported(format!(
                "export `{label}`: its {what} comes from a core instance made from exports \
                 (a shim or adapter), not from instantiating a core module"
            )));
        };
        self.check_exists(label, module, name, ExternalKind::Func)?;
        Ok(((module, instance), name.clone()))
    }

    /// The top-level core module at `module` of the module index space.
    fn top_module(&self, label: &str, module: usize) -> Result<&TopModule, TranspileError> {
        self.modules
            .get(module)
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                unsupported(format!(
                    "export `{label}`: its core instance is not an instantiation of a core \
                     module defined in this component"
                ))
            })
    }

    /// The core module `module` must really export `name` as a `kind`.
    fn check_exists(
        &self,
        label: &str,
        module: usize,
        name: &str,
        kind: ExternalKind,
    ) -> Result<(), TranspileError> {
        let found = self
            .top_module(label, module)?
            .exports
            .iter()
            .any(|(n, k)| n == name && *k == kind);
        if found {
            Ok(())
        } else {
            Err(unsupported(format!(
                "export `{label}`: core module {module} has no {kind:?} export `{name}`"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::wiring::{HostFunc, Supplier, WiredInstance};
    use super::*;
    use wasmparser::Parser;

    /// The `Reader` after reading `wat` (without canonical sections, which need the type
    /// scopes).
    fn read(wat: &str) -> Reader {
        let component = wat::parse_str(wat).unwrap();
        let mut reader = Reader::default();
        for payload in Parser::new(0).parse_all(&component) {
            let payload = payload.unwrap();
            if !matches!(payload, Payload::ComponentCanonicalSection(_)) {
                reader.feed(&payload, &[]).unwrap();
            }
        }
        reader
    }

    fn module_space(wat: &str) -> Vec<Option<usize>> {
        read(wat)
            .modules
            .iter()
            .map(|m| m.as_ref().map(|m| m.ordinal))
            .collect()
    }

    #[test]
    fn an_imported_function_takes_a_slot_in_the_function_index_space() {
        let reader = read(r#"(component (import "f" (func)))"#);
        assert!(matches!(
            reader.funcs.as_slice(),
            [Func::Other("an import")]
        ));
    }

    #[test]
    fn an_imported_core_module_takes_a_slot_in_the_module_index_space() {
        // Same count as `transpile_component`: the import is module 0, `$a` is module 1.
        let space = module_space(
            r#"(component
  (import "m" (core module $im))
  (core module $a (func (export "f"))))"#,
        );
        assert_eq!(space, [None, Some(0)]);
    }

    #[test]
    fn an_imported_component_takes_a_slot_in_the_component_index_space() {
        let reader = read(
            r#"(component
  (import "c" (component $ic))
  (component $inner))"#,
        );
        assert_eq!(reader.components.len(), 2);
        assert!(reader.components[0].opaque);
        // `$inner` lands at index 1, so an `Instantiated` index resolves to it.
        assert!(!reader.components[1].opaque);
    }

    /// A component made like `wasm-tools component new --adapt`: a program module that imports
    /// `wasi_snapshot_preview1.args_get`, an adapter that imports `env.memory` and the WIT
    /// function `wasi:cli/environment@0.2.3` `get-arguments`, and `wit-component`'s shim and
    /// fixup modules that tie them together through a table.
    fn adapter_component() -> Vec<u8> {
        use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
        use wit_parser::Resolve;

        let embed = |wit: &str, world: &str, wat: &str| {
            let mut resolve = Resolve::default();
            let package = resolve.push_str("test.wit", wit).unwrap();
            let world = resolve.select_world(&[package], Some(world)).unwrap();
            let mut module = wat::parse_str(wat).unwrap();
            embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8, false)
                .unwrap();
            module
        };
        let adapter = embed(
            "package wasi:cli@0.2.3;
interface environment { get-arguments: func() -> list<string>; }
world adapter { import environment; }",
            "adapter",
            r#"(module
  (import "env" "memory" (memory 1))
  (import "wasi:cli/environment@0.2.3" "get-arguments" (func $get (param i32)))
  (func (export "cabi_import_realloc") (param i32 i32 i32 i32) (result i32) i32.const 0)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 0)
  (func (export "args_get") (param i32 i32) (result i32)
    i32.const 0 call $get
    i32.const 0 i32.const 0 i32.store
    i32.const 0))"#,
        );
        let main = embed(
            "package t:p;
world w { export run: func(); }",
            "w",
            r#"(module
  (import "wasi_snapshot_preview1" "args_get" (func (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 0)
  (func (export "run") i32.const 0 i32.const 0 call 0 drop))"#,
        );
        ComponentEncoder::default()
            .module(&main)
            .unwrap()
            .adapter("wasi_snapshot_preview1", &adapter)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap()
    }

    /// The core instance that instantiates the top-level core module `module`.
    fn instance_of(wiring: &CoreWiring, module: usize) -> u32 {
        wiring.module_instance(module).unwrap()
    }

    // The modules of `adapter_component`, by ordinal: main 0, adapter 1, shim 2, fixup 3.
    const MAIN: usize = 0;
    const ADAPTER: usize = 1;
    const SHIM: usize = 2;
    const FIXUP: usize = 3;

    #[test]
    fn the_program_module_is_fed_its_preview1_import_by_the_shim() {
        let wiring = core_wiring(&adapter_component()).unwrap();
        let main = instance_of(&wiring, MAIN);
        let shim = instance_of(&wiring, SHIM);
        assert_eq!(
            wiring.supplier(main, "wasi_snapshot_preview1", "args_get"),
            Some(Supplier::export(shim, "0"))
        );
    }

    #[test]
    fn the_adapters_memory_is_the_program_modules_memory() {
        let wiring = core_wiring(&adapter_component()).unwrap();
        let main = instance_of(&wiring, MAIN);
        let adapter = instance_of(&wiring, ADAPTER);
        assert_eq!(
            wiring.supplier(adapter, "env", "memory"),
            Some(Supplier::export(main, "memory"))
        );
    }

    #[test]
    fn the_adapters_host_import_is_fed_by_the_shim() {
        let wiring = core_wiring(&adapter_component()).unwrap();
        let adapter = instance_of(&wiring, ADAPTER);
        let shim = instance_of(&wiring, SHIM);
        assert_eq!(
            wiring.supplier(adapter, "wasi:cli/environment@0.2.3", "get-arguments"),
            Some(Supplier::export(shim, "1"))
        );
    }

    #[test]
    fn the_fixups_table_is_the_shims_table() {
        let wiring = core_wiring(&adapter_component()).unwrap();
        let fixup = instance_of(&wiring, FIXUP);
        let shim = instance_of(&wiring, SHIM);
        assert_eq!(
            wiring.supplier(fixup, "shim", "$imports"),
            Some(Supplier::export(shim, "$imports"))
        );
    }

    #[test]
    fn the_fixups_actual_functions_are_the_adapter_and_the_lowered_host_function() {
        let wiring = core_wiring(&adapter_component()).unwrap();
        let fixup = instance_of(&wiring, FIXUP);
        let adapter = instance_of(&wiring, ADAPTER);
        assert_eq!(
            wiring.supplier(fixup, "actual", "0"),
            Some(Supplier::export(adapter, "args_get"))
        );
        assert_eq!(
            wiring.supplier(fixup, "actual", "1"),
            Some(Supplier::Host(HostFunc {
                interface: "wasi:cli/environment@0.2.3".into(),
                name: "get-arguments".into(),
            }))
        );
    }

    #[test]
    fn a_lone_module_has_one_unwired_instance() {
        let wiring = core_wiring(
            &wat::parse_str(r#"(component (core module $a (func (export "f"))) (core instance (instantiate $a)))"#)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            wiring.instances,
            [WiredInstance::Module {
                module: Some(0),
                args: vec![]
            }]
        );
    }
}
