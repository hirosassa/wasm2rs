//! Rendering the typed host trait (`pub trait Host` and the types it uses) from the
//! functions read by [`read`](super::read).

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use super::read::{unsupported, wit_functions};
use super::*;
use crate::TranspileError;
use crate::codegen::{assign_export_names, assign_import_names, interface_ident, plain_ident};

/// How many type nodes rendering one trait may visit. A component can share a type
/// between many others, so a small component can expand to Rust text of exponential size;
/// past this the trait is refused instead.
pub(crate) const NODE_BUDGET: usize = 200_000;

/// A named compound type the trait declares once.
pub(super) enum Decl {
    Record(WitRecord),
    Variant(WitVariant),
    Enum(WitEnum),
    Flags(WitFlags),
}

impl Decl {
    /// The WIT name of the declared type.
    fn name(&self) -> &str {
        match self {
            Decl::Record(r) => &r.name,
            Decl::Variant(v) => &v.name,
            Decl::Enum(e) => &e.name,
            Decl::Flags(f) => &f.name,
        }
    }

    /// The interface that defines the declared type.
    fn interface(&self) -> Option<&str> {
        match self {
            Decl::Record(r) => r.interface.as_deref(),
            Decl::Variant(v) => v.interface.as_deref(),
            Decl::Enum(e) => e.interface.as_deref(),
            Decl::Flags(f) => f.interface.as_deref(),
        }
    }
}

/// What the Rust name of a declared type or resource is, and where it lives.
#[derive(Clone)]
pub(super) struct Placed {
    /// The `pub mod` that holds it (`Qualified` naming); `None` when all types are top-level.
    module: Option<String>,
    /// The type's name inside that scope.
    name: String,
}

/// The records, variants, enums, flags and resources the trait's functions use, with the
/// Rust names they were given (one namespace, or one per defining interface when qualified).
#[derive(Default)]
pub(super) struct Records {
    /// Types live in one `pub mod` per defining interface and are named by interface and WIT
    /// name only, never by a counter.
    qualified: bool,
    /// What goes before `<module>::<Type>` where the trait and the glue spell a type: empty,
    /// or the path of the library that defines the types.
    prefix: String,
    /// The module of every defining interface seen (`None` = no interface defines it).
    modules: HashMap<Option<String>, String>,
    /// Resource ids and where they are named.
    resource_names: HashMap<u32, Placed>,
    /// The resources in the order they must be emitted.
    resources: Vec<Placed>,
    /// Each distinct declared type (by kind, name and rendered members) and its Rust name.
    names: HashMap<String, Placed>,
    /// The declared types in the order they must be emitted, with their Rust names.
    list: Vec<(Decl, Placed)>,
    /// Type nodes visited so far, against [`NODE_BUDGET`].
    visited: Cell<usize>,
}

/// `kebab-case` as a `CamelCase` Rust type name that is not one of the std names the
/// generated trait uses.
pub(super) fn type_name(name: &str) -> String {
    let mut out = String::new();
    for word in name.split(|c: char| !c.is_ascii_alphanumeric()) {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() {
        out.push_str("Record");
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'T');
    }
    // The std names the generated code uses, and the prelude items a `struct Some(..)` or
    // `trait Default` would shadow in a module that also holds the user's `impl Host`.
    const RESERVED: [&str; 28] = [
        "Host",
        // Top-level names of the transpiled code and the glue it is placed beside.
        "Glue",
        "Instance",
        "Imports",
        "GcRef",
        "ContObj",
        "StepResult",
        "ContFrame",
        "ExternFuncs",
        "Wasm2RsException",
        "Wasm2RsDelegate",
        "String",
        "Vec",
        "Option",
        "Result",
        "Self",
        "Box",
        "Some",
        "None",
        "Ok",
        "Err",
        "Default",
        "Clone",
        "Copy",
        "Eq",
        "PartialEq",
        "Drop",
        "Send",
    ];
    if RESERVED.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

/// The module (`pub mod`) of the types defined by `interface`: the interface name as a
/// method-name prefix (`wasi:filesystem/types@0.2.0` is `wasi_filesystem_types`), made not to
/// collide with a keyword or a name the glue puts at the top level.
pub(super) fn module_ident(interface: Option<&str>) -> String {
    let Some(interface) = interface else {
        return "component".to_string();
    };
    let ident = interface_ident(interface);
    const TOP_LEVEL: [&str; 9] = [
        "glue_abi",
        "glue_mem",
        "glue_alloc",
        "std",
        "core",
        "alloc",
        "crate",
        "self",
        "super",
    ];
    if TOP_LEVEL.contains(&ident.as_str()) || plain_ident(&ident) != ident {
        format!("{ident}_")
    } else {
        ident
    }
}

impl Records {
    pub(super) fn new(naming: WitNaming, prefix: String) -> Self {
        Records {
            qualified: naming == WitNaming::Qualified,
            prefix,
            ..Records::default()
        }
    }

    pub(super) fn visit(&self) -> Result<(), TranspileError> {
        let n = self.visited.get().saturating_add(1);
        self.visited.set(n);
        if n > NODE_BUDGET {
            return Err(unsupported(
                "type too large (it expands to more than 200000 type nodes)",
            ));
        }
        Ok(())
    }

    /// What identifies a declared type: its kind, name and members as rendered, and (when
    /// qualified) the interface that defines it. A type shared by several interfaces has one
    /// definer, so it is still one type; two interfaces that each define a type of the same
    /// name and shape have two.
    pub(super) fn key(&self, decl: &Decl) -> Result<String, TranspileError> {
        let mut key = String::new();
        if self.qualified {
            let _ = write!(key, "{:?}|", decl.interface());
        }
        match decl {
            Decl::Record(r) => {
                let _ = write!(key, "record|{}", r.name);
                for (name, ty) in &r.fields {
                    let _ = write!(key, "|{name}:{}", self.rust(ty)?);
                }
            }
            Decl::Variant(v) => {
                let _ = write!(key, "variant|{}", v.name);
                for (name, ty) in &v.cases {
                    let payload = match ty {
                        Some(t) => self.rust(t)?,
                        None => String::new(),
                    };
                    let _ = write!(key, "|{name}:{payload}");
                }
            }
            Decl::Enum(e) => {
                let _ = write!(key, "enum|{}", e.name);
                for name in &e.cases {
                    let _ = write!(key, "|{name}");
                }
            }
            Decl::Flags(f) => {
                let _ = write!(key, "flags|{}", f.name);
                for name in &f.flags {
                    let _ = write!(key, "|{name}");
                }
            }
        }
        Ok(key)
    }

    /// Registers `decl` (after the types its members contain), giving it a Rust name
    /// that no other, different declared type has.
    pub(super) fn add(&mut self, decl: Decl) -> Result<(), TranspileError> {
        match &decl {
            Decl::Record(r) => r.fields.iter().try_for_each(|(_, ty)| self.add_in(ty))?,
            Decl::Variant(v) => v
                .cases
                .iter()
                .filter_map(|(_, t)| t.as_ref())
                .try_for_each(|ty| self.add_in(ty))?,
            Decl::Enum(_) | Decl::Flags(_) => {}
        }
        let key = self.key(&decl)?;
        if self.names.contains_key(&key) {
            return Ok(());
        }
        let placed = self.place(decl.interface(), decl.name());
        self.names.insert(key, placed.clone());
        self.list.push((decl, placed));
        Ok(())
    }

    /// The module that holds the types `interface` defines. Two interfaces whose names make
    /// the same module name get distinct ones (the later gets a number).
    pub(super) fn module_for(&mut self, interface: Option<&str>) -> String {
        let key = interface.map(str::to_string);
        if let Some(module) = self.modules.get(&key) {
            return module.clone();
        }
        let base = module_ident(interface);
        let mut module = base.clone();
        let mut n = 1usize;
        while self.modules.values().any(|taken| *taken == module) {
            n = n.saturating_add(1);
            module = format!("{base}_{n}");
        }
        self.modules.insert(key, module.clone());
        module
    }

    /// Where a type named `wit_name`, defined by `interface`, goes.
    pub(super) fn place(&mut self, interface: Option<&str>, wit_name: &str) -> Placed {
        let module = self.qualified.then(|| self.module_for(interface));
        Placed {
            name: self.fresh_name(module.as_deref(), wit_name),
            module,
        }
    }

    /// A Rust type name for `wit_name` that no declared type or resource of `module` has yet.
    pub(super) fn fresh_name(&self, module: Option<&str>, wit_name: &str) -> String {
        let base = type_name(wit_name);
        let mut name = base.clone();
        let mut n = 1usize;
        let taken = |name: &str| {
            let same = |p: &Placed| p.module.as_deref() == module && p.name == name;
            self.list.iter().any(|(_, p)| same(p)) || self.resources.iter().any(same)
        };
        while taken(&name) {
            n = n.saturating_add(1);
            name = format!("{base}{n}");
        }
        name
    }

    pub(super) fn add_resource(&mut self, r: &Arc<WitResource>) {
        if !self.resource_names.contains_key(&r.id) {
            let placed = self.place(r.interface.as_deref(), &r.name);
            self.resource_names.insert(r.id, placed.clone());
            self.resources.push(placed);
        }
    }

    pub(super) fn add_in(&mut self, ty: &WitType) -> Result<(), TranspileError> {
        self.visit()?;
        match ty {
            WitType::List(t) | WitType::Option(t) => self.add_in(t),
            WitType::Result { ok, err } => {
                for t in [ok, err].into_iter().flatten() {
                    self.add_in(t)?;
                }
                Ok(())
            }
            WitType::Tuple(ts) => ts.iter().try_for_each(|t| self.add_in(t)),
            WitType::Record(r) => self.add(Decl::Record(r.clone())),
            WitType::Variant(v) => self.add(Decl::Variant(v.clone())),
            WitType::Enum(e) => self.add(Decl::Enum(e.clone())),
            WitType::Flags(f) => self.add(Decl::Flags(f.clone())),
            WitType::Own(r) | WitType::Borrow(r) => {
                self.add_resource(r);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// How `placed` is spelled after `prefix`.
    pub(super) fn path(placed: &Placed, prefix: &str) -> String {
        match &placed.module {
            Some(module) => format!("{prefix}{module}::{}", placed.name),
            None => placed.name.clone(),
        }
    }

    /// The Rust name of an already registered declared type, spelled after `prefix`.
    pub(super) fn declared(&self, decl: Decl, prefix: &str) -> Result<String, TranspileError> {
        Ok(match self.names.get(&self.key(&decl)?) {
            Some(placed) => Self::path(placed, prefix),
            None => type_name(decl.name()),
        })
    }

    /// The type as the trait and the glue spell it.
    pub(super) fn rust(&self, ty: &WitType) -> Result<String, TranspileError> {
        self.rust_in(ty, &self.prefix)
    }

    /// The type as spelled from inside a type's own module (`super::` reaches the others).
    pub(super) fn rust_in_module(&self, ty: &WitType) -> Result<String, TranspileError> {
        self.rust_in(ty, "super::")
    }

    pub(super) fn rust_in(&self, ty: &WitType, prefix: &str) -> Result<String, TranspileError> {
        self.visit()?;
        let inner = |t: &Option<Arc<WitType>>| match t {
            Some(t) => self.rust_in(t, prefix),
            None => Ok("()".to_string()),
        };
        Ok(match ty {
            WitType::U8 => "u8".into(),
            WitType::U16 => "u16".into(),
            WitType::U32 => "u32".into(),
            WitType::U64 => "u64".into(),
            WitType::S8 => "i8".into(),
            WitType::S16 => "i16".into(),
            WitType::S32 => "i32".into(),
            WitType::S64 => "i64".into(),
            WitType::F32 => "f32".into(),
            WitType::F64 => "f64".into(),
            WitType::Bool => "bool".into(),
            WitType::Char => "char".into(),
            WitType::String => "String".into(),
            WitType::List(t) => format!("Vec<{}>", self.rust_in(t, prefix)?),
            WitType::Option(t) => format!("Option<{}>", self.rust_in(t, prefix)?),
            WitType::Result { ok, err } => format!("Result<{}, {}>", inner(ok)?, inner(err)?),
            WitType::Tuple(ts) => {
                let parts = ts
                    .iter()
                    .map(|t| self.rust_in(t, prefix))
                    .collect::<Result<Vec<_>, _>>()?;
                match parts.as_slice() {
                    [one] => format!("({one},)"),
                    many => format!("({})", many.join(", ")),
                }
            }
            WitType::Record(r) => self.declared(Decl::Record(r.clone()), prefix)?,
            WitType::Variant(v) => self.declared(Decl::Variant(v.clone()), prefix)?,
            WitType::Enum(e) => self.declared(Decl::Enum(e.clone()), prefix)?,
            WitType::Flags(f) => self.declared(Decl::Flags(f.clone()), prefix)?,
            WitType::Own(r) | WitType::Borrow(r) => match self.resource_names.get(&r.id) {
                Some(placed) => Self::path(placed, prefix),
                None => type_name(&r.name),
            },
        })
    }
}

/// One identifier per name from `ident`, made unique by appending `_{n}` to a repeat.
pub(super) fn dedup_idents<'a>(
    names: impl Iterator<Item = &'a str>,
    ident: impl Fn(&str) -> String,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, name) in names.enumerate() {
        let mut id = ident(name);
        while out.contains(&id) {
            id = format!("{id}_{i}");
        }
        out.push(id);
    }
    out
}

/// Field and parameter identifiers.
pub(super) fn unique_idents<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    dedup_idents(names, plain_ident)
}

/// `kebab-case` as `CamelCase` enum case names that are valid, distinct and not `Self`.
pub(super) fn case_idents<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    dedup_idents(names, |name| {
        let ident = type_name(name);
        if ident == "Record" && !name.eq_ignore_ascii_case("record") {
            "Case".to_string()
        } else {
            ident
        }
    })
}

/// `kebab-case` as `UPPER_SNAKE_CASE` associated constant names, distinct from each other.
pub(super) fn flag_idents<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    dedup_idents(names, |name| {
        let words: Vec<String> = name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_ascii_uppercase)
            .collect();
        let mut ident = if words.is_empty() {
            "FLAG".to_string()
        } else {
            words.join("_")
        };
        if ident.starts_with(|c: char| c.is_ascii_digit()) {
            ident.insert(0, 'F');
        }
        ident
    })
}

/// A flags type as `pub struct Name(pub u8|u16|u32)` (the canonical ABI's size: flag `i`
/// is bit `i`) with one associated constant per flag, `contains` and `union`.
/// The Rust integer holding a flags type: the canonical ABI's size.
pub(super) fn flags_int(flags: &WitFlags) -> &'static str {
    match flags.flags.len() {
        0..=8 => "u8",
        9..=16 => "u16",
        _ => "u32",
    }
}

pub(super) fn write_flags(out: &mut String, name: &str, flags: &WitFlags) {
    let int = flags_int(flags);
    let _ = writeln!(
        out,
        "\n#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]\npub struct {name}(pub {int});\n\nimpl {name} {{"
    );
    for (i, ident) in flag_idents(flags.flags.iter().map(String::as_str))
        .iter()
        .enumerate()
    {
        let _ = writeln!(out, "    pub const {ident}: Self = Self(1 << {i});");
    }
    out.push_str(
        "    pub fn contains(self, other: Self) -> bool {\n        self.0 & other.0 == other.0\n    }\n\n    pub fn union(self, other: Self) -> Self {\n        Self(self.0 | other.0)\n    }\n}\n",
    );
}

/// Rust source for a `pub trait Host` with one method per function of every interface
/// the component imports, plus the types of the records, variants, enums and flags they use (`std` only).
///
/// A method `fn <name>(&mut self, <params>) -> <result>;` is named as the
/// [`named_imports`](crate::TranspileOptions::named_imports) method of the core import
/// with the interface as module and the function name as field, so a glue can map
/// one to the other. Types map as `string` to `String`, `list<T>` to `Vec<T>`,
/// `option<T>` to `Option<T>`, `result<T, E>` to `Result<T, E>` (an absent side is
/// `()`), `tuple` to a Rust tuple, `record` to a struct (named after the record, its
/// fields snake_case, emitted once however many functions use it) and the integers,
/// floats, `bool` and `char` to their Rust types.
///
/// A `variant` becomes a Rust `enum` (`Case(T)` for a case with a payload, `Case` without;
/// `#[derive(Debug, Clone, PartialEq)]`), an `enum` a fieldless Rust `enum` (also `Copy`, `Eq`),
/// and `flags` a `#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] pub struct F(pub u8)`
/// (`u16` for 9 to 16 flags, `u32` for 17 to 32; more than 32 is an error) whose flag `i` is bit
/// `i`, with an associated constant per flag (`F::READ`) and `contains`/`union`. Case names are
/// `CamelCase`d; flag constants are `UPPER_SNAKE_CASE`d. Like records, each is emitted once.
///
/// A resource `r` becomes one `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct R(pub u32);`
/// handle (however many interfaces use it); `own<r>` and `borrow<r>` are both `R`, since the
/// host owns the handles. Its `[method]r.f`, `[static]r.f` and `[constructor]r` functions are
/// methods named like the core imports with those field names (a method's `self` parameter
/// is `self_`). The interface that defines the resource also gets
/// `fn <module>_resource_drop_r(&mut self, handle: R);`, named like the core import
/// `[resource-drop]r` that the lowered `resource.drop` becomes, but only if the component
/// lowers a `resource.drop` of `r` (a program that never drops `r` has no such core import).
/// Every resource's drop is declared if the component has no core module at the top level
/// (only its types), has a nested component that lowers a `resource.drop` (it is not mapped), or has a
/// `resource.drop` that cannot be tied to a resource.
///
/// # Errors
///
/// A parse error, a core module or other non-component input, an import that is not an
/// interface instance, or a function using an unsupported type
/// ([`TranspileError::Unsupported`], naming the type, the function and the interface).
pub fn wit_host_trait(component: &[u8]) -> Result<String, TranspileError> {
    wit_host_trait_with_options(component, &WitOptions::default())
}

/// How the generated types are named.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WitNaming {
    /// All types are top-level, named after the WIT type; a name used again by another type
    /// gets a number (`ErrorCode`, `ErrorCode2`), so a name depends on what else the
    /// component imports.
    #[default]
    Default,
    /// Each type is in a `pub mod` named after the interface that defines it (the prefix of
    /// the interface's method names: `wasi_filesystem_types::ErrorCode`,
    /// `wasi_sockets_network::ErrorCode`) and keeps its WIT name; the path of a type depends
    /// only on its defining interface and its name, never on the other imports or their order.
    /// A type used by several interfaces is declared once, in the module of its definer.
    ///
    /// The version of an interface is not part of the module name (nor of the method names),
    /// so a component importing two versions of one interface gets a numbered module for the
    /// later one, which depends on import order; this is not supported with
    /// [`WitOptions::external_types`].
    Qualified,
}

/// Options of [`wit_host_trait_with_options`] and [`wit_host_glue_with_options`]. The default
/// is what [`wit_host_trait`] and [`wit_host_glue`] produce.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WitOptions {
    /// How the types are named.
    pub naming: WitNaming,
    /// Glue only: the Rust path of a library that already declares the types and `trait Host`
    /// (the output of [`WitNaming::Qualified`] for a component with at least these imports).
    /// The glue then declares neither, and refers to `<path>::Host` and
    /// `<path>::<interface module>::<Type>`. Implies [`WitNaming::Qualified`].
    pub external_types: Option<String>,
    /// Trait only: every method gets a default body that traps (a panic naming the method,
    /// like the glue's other traps), so a host implements just the methods it supports and a
    /// guest calling another one fails at the call, not at build time.
    pub default_trap: bool,
    /// Glue only: also generate `pub trait Exports` and its impl for `Instance<Glue<H>>`,
    /// one typed method per function the component exports ([`wit_exports`]), named like the
    /// [`named_exports`](crate::TranspileOptions::named_exports) method of the core export
    /// it lifts. Params are lowered and the result is lifted by the host (canonical ABI, a
    /// `string` or `list` through the lift's `cabi_realloc`), then the `post-return` runs if
    /// the lift has one. Off by default, so the output is unchanged.
    pub exports: bool,
}

impl WitOptions {
    fn naming(&self) -> WitNaming {
        if self.external_types.is_some() {
            WitNaming::Qualified
        } else {
            self.naming
        }
    }

    fn records(&self) -> Records {
        let prefix = self
            .external_types
            .as_ref()
            .map(|path| format!("{path}::"))
            .unwrap_or_default();
        Records::new(self.naming(), prefix)
    }
}

/// [`wit_host_trait`] with [`WitOptions`].
///
/// # Errors
///
/// Those of [`wit_host_trait`]; and [`WitOptions::external_types`] is an
/// [`Unsupported`](TranspileError::Unsupported) error, since only the glue refers to a library's
/// trait (the library declares it).
pub fn wit_host_trait_with_options(
    component: &[u8],
    options: &WitOptions,
) -> Result<String, TranspileError> {
    if options.external_types.is_some() {
        return Err(unsupported(
            "external types apply to the glue only (the trait is what the library declares)",
        ));
    }
    if options.exports {
        return Err(unsupported(
            "exports apply to the glue only (they are methods of `Instance<Glue<H>>`)",
        ));
    }
    let funcs = wit_functions(component)?;
    let (records, names) = register(&funcs, &[], options)?;
    write_declarations(&funcs, &records, &names, options.default_trap)
}

/// Registers the types of `funcs` and of the `exports` and assigns the method names
/// (parallel to `funcs`): what the trait and the glue must agree on.
pub(super) fn register(
    funcs: &[WitFunc],
    exports: &[WitExport],
    options: &WitOptions,
) -> Result<(Records, Vec<String>), TranspileError> {
    let mut records = options.records();
    for f in funcs.iter().chain(exports.iter().map(|e| &e.func)) {
        for (_, ty) in &f.params {
            records.add_in(ty)?;
        }
        if let Some(ty) = &f.result {
            records.add_in(ty)?;
        }
    }
    let mut indexed = Vec::new();
    for (i, f) in funcs.iter().enumerate() {
        let index = u32::try_from(i).map_err(|_| unsupported("too many imported functions"))?;
        indexed.push((f.interface.as_str(), f.name.as_str(), index));
    }
    Ok((records, assign_import_names(indexed)))
}

/// The declarations of the registered types and the `Host` trait.
pub(super) fn write_declarations(
    funcs: &[WitFunc],
    records: &Records,
    names: &[String],
    default_trap: bool,
) -> Result<String, TranspileError> {
    let mut out = String::from("// Generated by wasm2rs --wit-host.\n");
    write_types(&mut out, records)?;
    out.push_str("\npub trait Host {\n");
    for (f, name) in funcs.iter().zip(names) {
        let _ = writeln!(out, "    /// {}", doc_text(&f.interface, &f.name));
        let idents = unique_idents(f.params.iter().map(|(n, _)| n.as_str()));
        let sig = typed_signature(records, name, f, &idents)?;
        if default_trap {
            // Binding the parameters keeps `-D warnings` quiet about the unused ones.
            let mut bound = idents.join(", ");
            if !bound.is_empty() {
                bound.push(',');
            }
            let _ = writeln!(
                out,
                "{sig} {{\n        let _ = ({bound});\n        panic!(\"wasm2rs wit host: not implemented: {name}\")\n    }}"
            );
        } else {
            let _ = writeln!(out, "{sig};");
        }
    }
    out.push_str("}\n");
    Ok(out)
}

/// `    fn {name}(&mut self, {ident}: {type}, ..) -> {result}`: the typed method of `f`, its
/// params named `idents` (the `Host` and `Exports` methods share it).
pub(super) fn typed_signature(
    records: &Records,
    name: &str,
    f: &WitFunc,
    idents: &[String],
) -> Result<String, TranspileError> {
    let mut sig = format!("    fn {name}(&mut self");
    for (ident, (_, ty)) in idents.iter().zip(&f.params) {
        let _ = write!(sig, ", {ident}: {}", records.rust(ty)?);
    }
    sig.push(')');
    if let Some(ty) = &f.result {
        let _ = write!(sig, " -> {}", records.rust(ty)?);
    }
    Ok(sig)
}

/// The doc comment text naming a function. Debug-escaped: an interface or function name may
/// hold any characters, and `[method]x` would read as an intra-doc link.
pub(super) fn doc_text(interface: &str, name: &str) -> String {
    format!("{interface:?} {name:?}")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

/// The type declarations: top-level, or (qualified) one `pub mod` per defining interface,
/// ordered by module and name so that the text does not depend on the order of the functions.
pub(super) fn write_types(out: &mut String, records: &Records) -> Result<(), TranspileError> {
    if !records.qualified {
        for placed in &records.resources {
            write_resource(out, &placed.name);
        }
        for (decl, placed) in &records.list {
            write_decl(out, records, decl, &placed.name)?;
        }
        return Ok(());
    }
    let mut modules: Vec<&String> = records.modules.values().collect();
    modules.sort();
    for module in modules {
        let in_module = |p: &Placed| p.module.as_deref() == Some(module.as_str());
        let mut resources: Vec<&Placed> =
            records.resources.iter().filter(|p| in_module(p)).collect();
        resources.sort_by(|a, b| a.name.cmp(&b.name));
        let mut decls: Vec<&(Decl, Placed)> =
            records.list.iter().filter(|(_, p)| in_module(p)).collect();
        decls.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        if resources.is_empty() && decls.is_empty() {
            continue;
        }
        let _ = writeln!(out, "\npub mod {module} {{");
        let mut body = String::new();
        for placed in resources {
            write_resource(&mut body, &placed.name);
        }
        for (decl, placed) in decls {
            write_decl(&mut body, records, decl, &placed.name)?;
        }
        for line in body.lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                let _ = writeln!(out, "    {line}");
            }
        }
        out.push_str("}\n");
    }
    Ok(())
}

pub(super) fn write_resource(out: &mut String, name: &str) {
    let _ = writeln!(
        out,
        "\n#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub struct {name}(pub u32);"
    );
}

/// One record, variant, enum or flags declaration named `name`, whose member types are spelled
/// as from inside the module that holds it (when not qualified no type has a module, so the
/// `super::` prefix is never written and the spelling is the top-level one).
pub(super) fn write_decl(
    out: &mut String,
    records: &Records,
    decl: &Decl,
    name: &str,
) -> Result<(), TranspileError> {
    let rust = |ty: &WitType| records.rust_in_module(ty);
    match decl {
        Decl::Record(record) => {
            let idents = unique_idents(record.fields.iter().map(|(n, _)| n.as_str()));
            let _ = writeln!(
                out,
                "\n#[derive(Debug, Clone, PartialEq)]\npub struct {name} {{"
            );
            for (ident, (_, ty)) in idents.iter().zip(&record.fields) {
                let _ = writeln!(out, "    pub {ident}: {},", rust(ty)?);
            }
            out.push_str("}\n");
        }
        Decl::Variant(variant) => {
            let idents = case_idents(variant.cases.iter().map(|(n, _)| n.as_str()));
            let _ = writeln!(
                out,
                "\n#[derive(Debug, Clone, PartialEq)]\npub enum {name} {{"
            );
            for (ident, (_, ty)) in idents.iter().zip(&variant.cases) {
                match ty {
                    Some(t) => {
                        let _ = writeln!(out, "    {ident}({}),", rust(t)?);
                    }
                    None => {
                        let _ = writeln!(out, "    {ident},");
                    }
                }
            }
            out.push_str("}\n");
        }
        Decl::Enum(e) => {
            let idents = case_idents(e.cases.iter().map(String::as_str));
            let _ = writeln!(
                out,
                "\n#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub enum {name} {{"
            );
            for ident in &idents {
                let _ = writeln!(out, "    {ident},");
            }
            out.push_str("}\n");
        }
        Decl::Flags(f) => write_flags(out, name, f),
    }
    Ok(())
}

/// [`wit_host_trait`] plus the glue that lowers it to the flat reentrant `Imports` trait of
/// the component's core module: the same `Host` trait and types, then a
/// `pub struct Glue<H: Host>(pub H);` with `impl<H: Host> Imports for Glue<H>`.
///
/// Transpile the core module with
/// [`named_imports`](crate::TranspileOptions::named_imports) and
/// [`reentrant_imports`](crate::TranspileOptions::reentrant_imports), put the generated
/// text in the same module as the transpiled code, implement `Host` and build the guest as
/// `Instance::new(Glue(host))`. Each `Imports` method has the name of the `Host` method it
/// forwards to (both come from the same naming), lifts the flat core parameters into the
/// typed ones, calls the host and lowers the typed result to the flat core result.
///
/// Covered: functions whose parameters and result are made only of integers, floats,
/// `bool`, `char`, `enum`, `flags`, resource handles (so `[resource-drop]` too) and
/// `variant`/`option`/`result`/`record`/`tuple` of those, when the parameters flatten to at
/// most 16 core values and the result to at most one. Lifting follows the canonical ABI: a
/// `bool` is any non-zero value, a narrow integer keeps its low bits, flag bits beyond the
/// declared ones are ignored; an invalid `char` or an unknown discriminant panics.
///
/// # Errors
///
/// Everything [`wit_host_trait`] reports, and [`TranspileError::Unsupported`] naming the
/// function when it needs linear memory (a `string` or `list` anywhere in its parameters or
/// result, more than 16 flat parameters, or a result of more than one flat value): not
/// supported yet, so no glue is returned at all rather than a partial one.
pub fn wit_host_glue(component: &[u8]) -> Result<String, TranspileError> {
    wit_host_glue_with_options(component, &WitOptions::default())
}

/// [`wit_host_glue`] with [`WitOptions`]. With
/// [`external_types`](WitOptions::external_types) the text holds only the glue (and a
/// `use <path>::Host;`), which names the library's trait and types by path.
///
/// # Errors
///
/// Those of [`wit_host_glue`].
pub fn wit_host_glue_with_options(
    component: &[u8],
    options: &WitOptions,
) -> Result<String, TranspileError> {
    glue_text(component, options, None)
}

/// A function import of the program core module that its `Imports` trait declares.
pub(crate) struct CoreImport {
    pub module: String,
    pub field: String,
    /// The name of its `Imports` method (`named_imports`).
    pub ident: String,
}

/// [`wit_host_glue_with_options`] for a component whose program module has the function
/// imports `core_imports`: the glue implements exactly those `Imports` methods, in that
/// order, each delegating to the `Host` method of the WIT function it lowers. The module may
/// name the interface by another compatible version than the component's own import, or import
/// one function under several versions; the glue then still has one method per module import.
pub(crate) fn wit_host_glue_for_module(
    component: &[u8],
    options: &WitOptions,
    core_imports: &[CoreImport],
) -> Result<String, TranspileError> {
    glue_text(component, options, Some(core_imports))
}

fn glue_text(
    component: &[u8],
    options: &WitOptions,
    core_imports: Option<&[CoreImport]>,
) -> Result<String, TranspileError> {
    let funcs = wit_functions(component)?;
    let exports = if options.exports {
        wit_exports(component)?
    } else {
        Vec::new()
    };
    let (records, names) = register(&funcs, &exports, options)?;
    let mut out = match &options.external_types {
        Some(path) => format!("// Generated by wasm2rs --wit-host.\nuse {path}::Host;\n"),
        None => write_declarations(&funcs, &records, &names, false)?,
    };
    let mut export_names = Vec::new();
    for (i, e) in exports.iter().enumerate() {
        let index = u32::try_from(i).map_err(|_| unsupported("too many exported functions"))?;
        export_names.push((e.core_export.as_str(), index));
    }
    let export_names = assign_export_names(export_names);
    // The core method of an export is named by `named_exports` over the module's whole export
    // list, which is not known here: a name that depends on the other exports (a clash, or the
    // same core export lifted twice) could not be told apart, so it is refused.
    for (e, name) in exports.iter().zip(&export_names) {
        let alone = assign_export_names([(e.core_export.as_str(), 0)]);
        if alone.first() != Some(name) {
            return Err(unsupported(format!(
                "export `{}`: its method name clashes with another export's",
                e.core_export
            )));
        }
    }
    if let Some(other) = exports
        .iter()
        .find(|e| e.core_module != exports[0].core_module)
    {
        return Err(unsupported(format!(
            "export `{}`: the exports come from more than one core module",
            other.core_export
        )));
    }
    let exported = options
        .exports
        .then_some((exports.as_slice(), export_names.as_slice()));
    let methods = match core_imports {
        Some(core) => core_methods(&funcs, &names, core)?,
        None => funcs
            .iter()
            .zip(&names)
            .map(|(func, name)| glue::Method {
                func,
                name,
                host: name,
            })
            .collect(),
    };
    glue::write(&mut out, &methods, &records, exported)?;
    Ok(out)
}

/// One glue method per core import, in import order.
fn core_methods<'a>(
    funcs: &'a [WitFunc],
    host_names: &'a [String],
    core: &'a [CoreImport],
) -> Result<Vec<glue::Method<'a>>, TranspileError> {
    let mut methods = Vec::new();
    for import in core {
        let Some(index) = resolve_core_import(funcs, &import.module, &import.field)? else {
            continue;
        };
        let (Some(func), Some(host)) = (funcs.get(index), host_names.get(index)) else {
            return Err(TranspileError::Internal(
                "a resolved core import is not a component function".into(),
            ));
        };
        methods.push(glue::Method {
            func,
            name: &import.ident,
            host,
        });
    }
    Ok(methods)
}

/// The index in `funcs` of the WIT function the core import `module` `field` lowers: the one
/// of the same interface and name; failing that, of a version of the interface that the
/// module's version stands for (`wasi:cli/environment@0.2.0` is satisfied by a component
/// importing `@0.2.12`, as `wit-component` names them). `None` when no function does: the
/// import is not a WIT one, so the glue has no method for it (the module's `Imports` then
/// stays unimplemented, which the compiler reports).
fn resolve_core_import(
    funcs: &[WitFunc],
    module: &str,
    field: &str,
) -> Result<Option<usize>, TranspileError> {
    if let Some(i) = funcs
        .iter()
        .position(|f| f.interface == module && f.name == field)
    {
        return Ok(Some(i));
    }
    let mut compatible = funcs
        .iter()
        .enumerate()
        .filter(|(_, f)| f.name == field && compatible_versions(&f.interface, module))
        .map(|(i, _)| i);
    match (compatible.next(), compatible.next()) {
        (found, None) => Ok(found),
        (Some(_), Some(_)) => Err(unsupported(format!(
            "the core module imports `{field}` from `{module}`, which the component imports at \
             several compatible versions: the name alone does not say which one it lowers"
        ))),
        (None, Some(_)) => Ok(None),
    }
}

/// Whether the interface names `a` and `b` are the same interface at semver-compatible
/// versions, as `wit-parser` links them: `0.2.10` and `0.2.0` (same `0.minor`), `1.2.0` and
/// `1.3.1` (same major). A pre-release, a `0.0.x` or a version without all three numbers
/// is compatible with itself only.
fn compatible_versions(a: &str, b: &str) -> bool {
    fn track(name: &str) -> Option<(&str, u64, u64)> {
        let (base, version) = name.split_once('@')?;
        let mut parts = version.split('.');
        let major: u64 = parts.next()?.parse().ok()?;
        let minor: u64 = parts.next()?.parse().ok()?;
        let _patch: u64 = parts.next()?.parse().ok()?;
        if parts.next().is_some() || (major == 0 && minor == 0) {
            return None;
        }
        Some((base, major, if major == 0 { minor } else { 0 }))
    }
    matches!((track(a), track(b)), (Some(x), Some(y)) if x == y)
}
