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

use super::{Def, Scope, Sig, WitFunc, def_at, innermost, read_component, unsupported};
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

/// A function of the component's function index space.
#[derive(Clone)]
enum Func {
    Lift(Lift),
    /// Anything else (an import, an alias of an instance's export).
    Other(&'static str),
}

impl Default for Func {
    fn default() -> Self {
        Self::Other("an unknown item")
    }
}

#[derive(Clone)]
struct Lift {
    sig: Result<Sig, String>,
    core_func: u32,
    options: Vec<CanonicalOption>,
}

/// A function of the core function index space.
#[derive(Clone)]
enum CoreFunc {
    /// An alias of an export of a core instance.
    Export { instance: u32, name: String },
    /// A `canon lower`.
    Lowered,
    /// Another canonical built-in.
    Builtin,
}

#[derive(Clone)]
enum CoreInstance {
    /// An instantiation of the top-level core module with this index.
    Module(usize),
    /// An instance made from exports only (a shim or adapter).
    FromExports,
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
    /// An import or an alias.
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
    core_instances: Vec<CoreInstance>,
    instances: Vec<Instance>,
    /// The exports of each top-level core module.
    modules: Vec<Vec<(String, ExternalKind)>>,
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
                        CoreInstanceDef::Instantiate { module_index, .. } => {
                            CoreInstance::Module(index(module_index).unwrap_or(usize::MAX))
                        }
                        CoreInstanceDef::FromExports(_) => CoreInstance::FromExports,
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
                    match import?.ty {
                        ComponentTypeRef::Func(_) => self.funcs.push(Func::Other("an import")),
                        ComponentTypeRef::Instance(_) => self.instances.push(Instance::Other),
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
            ComponentAlias::InstanceExport { kind, .. } => match kind {
                ComponentExternalKind::Func => self
                    .funcs
                    .push(Func::Other("an alias of an export of an instance")),
                ComponentExternalKind::Instance => self.instances.push(Instance::Other),
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
                    Ok(_) => Err("a lift whose type is not a function type".to_string()),
                    Err(e) => Err(e),
                };
                self.funcs.push(Func::Lift(Lift {
                    sig,
                    core_func: core_func_index,
                    options: options.to_vec(),
                }));
            }
            CanonicalFunction::Lower { .. } => self.core_funcs.push(CoreFunc::Lowered),
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
                        Some(InnerKind::Module(exports)) => self.modules.push(exports),
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
                let shim = self.components.get(*component);
                let shim = shim.filter(|s| !s.opaque).ok_or_else(|| {
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
            Some(Func::Other(what)) => {
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

        let (module, core_export) = self.core_export(&label, lift.core_func, "function")?;
        let mut export = WitExport {
            func: WitFunc {
                interface: interface.to_string(),
                name: name.to_string(),
                params,
                result,
            },
            core_module: module.0,
            core_export,
            memory: None,
            realloc: None,
            post_return: None,
            utf8: true,
        };
        let instance = module.1;
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
                    let name = match found {
                        Some((from, name)) if *from == instance => name.clone(),
                        _ => {
                            return Err(unsupported(format!(
                                "export `{label}`: its memory is not an export of the same core \
                                 instance as the function"
                            )));
                        }
                    };
                    self.check_exists(&label, module.0, &name, ExternalKind::Memory)?;
                    export.memory = Some(name);
                }
                CanonicalOption::Realloc(i) | CanonicalOption::PostReturn(i) => {
                    let what = if matches!(option, CanonicalOption::Realloc(_)) {
                        "realloc"
                    } else {
                        "post-return"
                    };
                    let (found, name) = self.core_export(&label, *i, what)?;
                    if found.1 != instance {
                        return Err(unsupported(format!(
                            "export `{label}`: its {what} is not an export of the same core \
                             instance as the function"
                        )));
                    }
                    if what == "realloc" {
                        export.realloc = Some(name);
                    } else {
                        export.post_return = Some(name);
                    }
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
            Some(CoreFunc::Lowered) => {
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
        let module = match index(instance).and_then(|i| self.core_instances.get(i)) {
            Some(CoreInstance::Module(m)) => *m,
            _ => {
                return Err(unsupported(format!(
                    "export `{label}`: its {what} comes from a core instance made from exports \
                     (a shim or adapter), not from instantiating a core module"
                )));
            }
        };
        self.check_exists(label, module, name, ExternalKind::Func)?;
        Ok(((module, instance), name.clone()))
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
            .modules
            .get(module)
            .is_some_and(|m| m.iter().any(|(n, k)| n == name && *k == kind));
        if found {
            Ok(())
        } else {
            Err(unsupported(format!(
                "export `{label}`: core module {module} has no {kind:?} export `{name}`"
            )))
        }
    }
}
