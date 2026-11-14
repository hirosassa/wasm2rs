//! How the core instances of a component are wired to each other (see
//! `docs/core-module-linking-design.md`, stage L1).
//!
//! A component made with a preview1 adapter instantiates several core modules (the program,
//! the adapter, a shim and a fixup) and passes the exports of one to the imports of
//! another. This keeps that graph, and, for a core function that comes from a `canon lower`,
//! the WIT function it calls, so a later stage can link the modules and name the host imports.

use std::collections::BTreeMap;

use super::builtin::Builtin;

/// A function of an imported interface instance. (A top-level imported function is not
/// traced yet: a `canon lower` of one is `Supplier::Other`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostFunc {
    /// The imported instance, e.g. `wasi:cli/environment@0.2.3`.
    pub interface: String,
    pub name: String,
    /// The core instance and export name of the lowering's `realloc` option, if it has one
    /// (an adapter's `cabi_import_realloc`). `None` for a function the reader did not see
    /// lowered.
    pub realloc: Option<(u32, String)>,
}

/// What provides an item that a core instance receives or exports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Supplier {
    /// An export of a core instance (a memory, table, global or function).
    Export { instance: u32, name: String },
    /// A core function made by `canon lower` of an imported function.
    Host(HostFunc),
    /// A core function made by a canonical built-in (`task.return`, `stream.read`, ...).
    Builtin(Builtin),
    /// Anything else (an item this reader does not trace).
    Other,
}

impl Supplier {
    pub fn export(instance: u32, name: &str) -> Self {
        Self::Export {
            instance,
            name: name.to_string(),
        }
    }
}

/// A core instance of the component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WiredInstance {
    /// An instantiation of a core module.
    Module {
        /// Its index among the top-level core modules, or `None` for a module that is not
        /// one of them (an import or an alias).
        module: Option<usize>,
        /// The instantiation arguments: the import module name, and the core instance given.
        args: Vec<(String, u32)>,
    },
    /// An instance made from exports only (a shim's or an adapter's argument bundle): its
    /// exports by name, with what provides each.
    Exports(BTreeMap<String, Supplier>),
}

/// The core instance graph of a component.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct CoreWiring {
    pub instances: Vec<WiredInstance>,
}

impl CoreWiring {
    /// The core instance that instantiates the top-level core module `module`.
    pub fn module_instance(&self, module: usize) -> Option<u32> {
        let at = self.instances.iter().position(
            |i| matches!(i, WiredInstance::Module { module: Some(m), .. } if *m == module),
        )?;
        u32::try_from(at).ok()
    }

    /// What feeds the import `module`.`name` of the core instance `instance`.
    ///
    /// An instance made from exports is looked through: the answer is what provides the
    /// member of that bundle. `None` if `instance` is not an instantiation, has no such
    /// import module, or the bundle has no such member.
    pub fn supplier(&self, instance: u32, module: &str, name: &str) -> Option<Supplier> {
        let WiredInstance::Module { args, .. } = self.instance(instance)? else {
            return None;
        };
        let (_, given) = args.iter().find(|(m, _)| m == module)?;
        match self.instance(*given)? {
            WiredInstance::Module { .. } => Some(Supplier::export(*given, name)),
            WiredInstance::Exports(members) => members.get(name).cloned(),
        }
    }

    fn instance(&self, at: u32) -> Option<&WiredInstance> {
        self.instances.get(usize::try_from(at).ok()?)
    }
}
