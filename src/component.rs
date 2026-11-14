//! Transpile a WebAssembly component in one call: its program core module, the typed import
//! glue and (optionally) the typed export side, as one Rust source.

mod link;

use std::collections::{BTreeMap, BTreeSet};

use wasmparser::{
    ComponentAlias, ComponentExternalKind, ComponentOuterAliasKind, ComponentTypeRef, DataKind,
    ElementKind, Encoding, ExternalKind, Instance, Parser, Payload, TypeRef,
};

use crate::parse::parse_module;
use crate::wit::{
    CoreWiring, Supplier, core_wiring, is_wit_function, wit_functions, wit_host_glue_for_module,
};
use crate::{
    SourceFile, SplitOptions, TranspileError, TranspileOptions, WitExport, WitOptions,
    transpile_ir_split_with_glue, wit_exports,
};

/// Options of [`transpile_component`].
///
/// The default is the recipe that runs a `wasm32-wasip2` component on the `wasm2rs-wasi`
/// library: the glue refers to that library's `Host`, and the typed `Exports` trait is
/// generated.
#[derive(Debug, Clone)]
pub struct ComponentOptions {
    /// Options of the core module's transpilation. `named_imports`, `named_exports` and
    /// `reentrant_imports` are **always switched on**, whatever they are set to here: the
    /// generated glue calls the module through those names and shapes, and does not build
    /// without them. Every other field is used as given.
    pub transpile: TranspileOptions,
    /// The Rust path of a library that declares `Host` and the interface types, which the
    /// glue then refers to (see [`WitOptions::external_types`]); types only the exports use are
    /// declared by the glue. `None` makes the glue declare its own `Host` trait and types. Default: `Some("wasm2rs_wasi")`.
    pub external_types: Option<String>,
    /// Also generate the typed `Exports` trait (see [`WitOptions::exports`]). Default: `true`.
    pub exports: bool,
}

impl Default for ComponentOptions {
    fn default() -> Self {
        Self {
            transpile: TranspileOptions::default(),
            external_types: Some("wasm2rs_wasi".to_string()),
            exports: true,
        }
    }
}

/// Transpile `component` into one Rust source: the program core module, then the `Glue` that
/// lowers the library's `Host` to the module's imports and, with
/// [`exports`](ComponentOptions::exports), the typed `Exports`.
///
/// The program module is the one that lifts the component's exports
/// ([`wit_exports`](crate::wit_exports)); a component without exports uses the module that is
/// instantiated with the lowered imports (a module that exports no function and has no start
/// function is wiring, as `wit-component`'s fixup module, and is skipped). On this single-module
/// path the other core modules (shims) are not emitted; see below for components that need them.
///
/// A component whose core modules are wired to each other (a preview1 adapter, or exports
/// lifted from several modules) is transpiled as one program: its modules are linked into a
/// single `Instance`, and the adapter's calls into the host become `Host` methods of the WIT
/// functions they lower.
///
/// # Errors
///
/// [`TranspileError::InvalidInput`] when the input is a core module (use
/// [`transpile`](crate::transpile)), has no core module, or has several candidate modules and
/// nothing tells them apart; plus every error of
/// [`wit_exports`](crate::wit_exports), [`transpile_with_options`](crate::transpile_with_options) and
/// [`wit_host_glue_with_options`](crate::wit_host_glue_with_options) (an export of a nested component, `compact-utf16` strings, an import
/// lowered both as UTF-8 and as UTF-16, a shape the glue cannot lower). A malformed input is a [`TranspileError::Parse`].
pub fn transpile_component(
    component: &[u8],
    options: &ComponentOptions,
) -> Result<String, TranspileError> {
    transpile_component_with_exports(component, options).map(|(code, _)| code)
}

/// [`transpile_component`], also returning the component's lifted exports.
///
/// The exports are the [`wit_exports`](crate::wit_exports) result the transpilation needs
/// anyway, so a caller that wants to inspect them (to check the component exports what it is
/// about to call, say) does not need its own [`wit_exports`](crate::wit_exports) pass.
///
/// # Errors
///
/// As [`transpile_component`].
pub fn transpile_component_with_exports(
    component: &[u8],
    options: &ComponentOptions,
) -> Result<(String, Vec<WitExport>), TranspileError> {
    let mut code = String::new();
    let exports =
        transpile_component_split(component, options, &SplitOptions::single_file(), |file| {
            code.push_str(&file.code);
            Ok(())
        })?;
    Ok((code, exports))
}

/// [`transpile_component_with_exports`], handing each source file to `sink` as it completes
/// (see [`transpile_split`](crate::transpile_split)) so the peak memory stays around one file.
///
/// The glue (and the typed `Exports`) is appended to the root `lib.rs`, which is emitted last;
/// the `funcs_{n}.rs` chunks are the program's own. With [`SplitOptions::single_file`] the one
/// `lib.rs` is byte-identical to [`transpile_component`].
///
/// # Errors
///
/// As [`transpile_component`], plus whatever `sink` returns.
pub fn transpile_component_split<F>(
    component: &[u8],
    options: &ComponentOptions,
    split: &SplitOptions,
    sink: F,
) -> Result<Vec<WitExport>, TranspileError>
where
    F: FnMut(SourceFile) -> Result<(), TranspileError>,
{
    let layout = scan(component)?;
    let exports = wit_exports(component)?;
    // Without `exports` nothing generates the callback loop that drives an async lift, and its
    // built-ins would come out as plain imports.
    let has_async = exports.iter().any(|e| e.async_abi.is_some());
    if !options.exports && has_async {
        return Err(TranspileError::Unsupported(
            "an async export (WASI 0.3) needs `exports` (the callback entry is only generated \
             with the exports glue)"
                .into(),
        ));
    }
    let transpile = TranspileOptions {
        named_imports: true,
        named_exports: true,
        reentrant_imports: true,
        ..options.transpile.clone()
    };
    let wit = WitOptions {
        external_types: options.external_types.clone(),
        exports: options.exports,
        ..WitOptions::default()
    };
    match select_program(component, &layout, &exports)? {
        Program::Module(module) => transpile_ir_split_with_glue(
            parse_module(module, &transpile)?,
            &transpile,
            split,
            |core_imports, imports_trait| {
                wit_host_glue_for_module(component, &wit, &core_imports, None, imports_trait)
            },
            sink,
        )?,
        Program::Linked => {
            link::transpile_linked(component, &layout, &exports, &transpile, &wit, split, sink)?;
        }
    }
    Ok(exports)
}

/// What the component's program is.
enum Program<'a> {
    /// One core module, transpiled on its own.
    Module(&'a [u8]),
    /// Several core modules wired to each other, linked into one unit (see [`link`]).
    Linked,
}

/// Picks the program. The linker is used only where a single module cannot be: the exports are
/// lifted from several modules, or the module that lifts them (or the only candidate) takes an
/// item from another core module, which a host import cannot stand for. Every other component
/// keeps the single-module path, so its output does not change.
fn select_program<'a>(
    component: &[u8],
    layout: &'a Layout,
    exports: &[WitExport],
) -> Result<Program<'a>, TranspileError> {
    reject_unserved_builtins(component, layout)?;
    if lifts_span_modules(exports) {
        return Ok(Program::Linked);
    }
    let index = select_index(layout, exports)?;
    let module = layout
        .modules
        .get(index)
        .map(Vec::as_slice)
        .ok_or_else(|| {
            TranspileError::InvalidInput(format!("core module {index} is not in the component"))
        })?;
    if imports_state_from_core_instances(module, layout.core_linked.get(&index))?
        || takes_functions_from_core_instances(component, layout, index, module)?
    {
        return Ok(Program::Linked);
    }
    if drops_initialization(component, layout, index)? {
        return Ok(Program::Linked);
    }
    Ok(Program::Module(module))
}

/// Refuses a component whose core modules import a canonical built-in the glue does not serve
/// (WASI 0.3's `task.return`, `stream.*`, ...: they need the async runtime, M2). Without this
/// they would come out as plain host imports that nothing implements.
fn reject_unserved_builtins(component: &[u8], layout: &Layout) -> Result<(), TranspileError> {
    let wiring = core_wiring(component)?;
    for &index in &layout.instantiated {
        let (Some(module), Some(instance)) =
            (layout.modules.get(index), wiring.module_instance(index))
        else {
            continue;
        };
        for payload in Parser::new(0).parse_all(module) {
            let Payload::ImportSection(reader) = payload? else {
                continue;
            };
            for import in reader.into_imports() {
                let import = import?;
                if let Some(Supplier::Builtin(builtin)) =
                    wiring.supplier(instance, import.module, import.name)
                    && let Some(feature) = builtin.unsupported_feature()
                {
                    return Err(TranspileError::Unsupported(format!(
                        "the core import `{}` `{}` is the canonical built-in {feature}, which \
                         needs the WASI 0.3 async runtime (not generated yet)",
                        import.module, import.name
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Whether the single-module path would drop what another instantiated module initializes: a
/// start function, or an active data or element segment in a memory or table the module at
/// `index` shares (a dylink library filling the table its main module calls through). A
/// segment in a table of its own, such as `wit-component`'s fixup filling the shim table, is
/// left out: the glue stands in for what that table serves.
fn drops_initialization(
    component: &[u8],
    layout: &Layout,
    index: usize,
) -> Result<bool, TranspileError> {
    // Built on the first segment found: reading the wiring is only needed then.
    let mut shared = None;
    for &other in layout.instantiated.iter().filter(|&&i| i != index) {
        let Some(module) = layout.modules.get(other) else {
            continue;
        };
        match initializer(module)? {
            None => {}
            Some(Initializer::Start) => return Ok(true),
            Some(Initializer::Segments) => {
                let shared = match &mut shared {
                    Some(shared) => shared,
                    None => shared.insert(SharedState::of(component, layout, index)?),
                };
                if shared.is_shared_with(layout, other)? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

enum Initializer {
    Start,
    Segments,
}

/// What a module initializes on instantiation: a start function (which wins), or an active
/// data or element segment.
fn initializer(module: &[u8]) -> Result<Option<Initializer>, TranspileError> {
    let mut found = None;
    for payload in Parser::new(0).parse_all(module) {
        match payload? {
            Payload::StartSection { .. } => return Ok(Some(Initializer::Start)),
            Payload::DataSection(reader) => {
                for data in reader {
                    if matches!(data?.kind, DataKind::Active { .. }) {
                        found = Some(Initializer::Segments);
                    }
                }
            }
            Payload::ElementSection(reader) => {
                for element in reader {
                    if matches!(element?.kind, ElementKind::Active { .. }) {
                        found = Some(Initializer::Segments);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(found)
}

/// The core instances that provide a memory, table or global to the lifted module, and the
/// lifted module's own instance: where another module's segment can be seen from it.
struct SharedState {
    wiring: CoreWiring,
    instances: BTreeSet<u32>,
}

impl SharedState {
    fn of(component: &[u8], layout: &Layout, index: usize) -> Result<Self, TranspileError> {
        let wiring = core_wiring(component)?;
        let mut instances = BTreeSet::new();
        if let (Some(instance), Some(module)) =
            (wiring.module_instance(index), layout.modules.get(index))
        {
            instances.insert(instance);
            instances.extend(state_suppliers(&wiring, instance, module)?);
        }
        Ok(Self { wiring, instances })
    }

    fn is_shared_with(&self, layout: &Layout, other: usize) -> Result<bool, TranspileError> {
        let (Some(instance), Some(module)) = (
            self.wiring.module_instance(other),
            layout.modules.get(other),
        ) else {
            return Ok(false);
        };
        Ok(state_suppliers(&self.wiring, instance, module)?
            .iter()
            .any(|supplier| self.instances.contains(supplier)))
    }
}

/// The core instances that supply the module's memory, table and global imports.
fn state_suppliers(
    wiring: &CoreWiring,
    instance: u32,
    module: &[u8],
) -> Result<BTreeSet<u32>, TranspileError> {
    let mut suppliers = BTreeSet::new();
    for payload in Parser::new(0).parse_all(module) {
        let Payload::ImportSection(reader) = payload? else {
            continue;
        };
        for import in reader.into_imports() {
            let import = import?;
            if !matches!(
                import.ty,
                TypeRef::Memory(_) | TypeRef::Table(_) | TypeRef::Global(_)
            ) {
                continue;
            }
            if let Some(Supplier::Export { instance, .. }) =
                wiring.supplier(instance, import.module, import.name)
            {
                suppliers.insert(instance);
            }
        }
    }
    Ok(suppliers)
}

/// Whether the lifted exports come from more than one core module.
fn lifts_span_modules(exports: &[WitExport]) -> bool {
    exports
        .first()
        .is_some_and(|first| exports.iter().any(|e| e.core_module != first.core_module))
}

/// Whether the module takes a function from another core module that its own import name does
/// not say how to implement: the shape of a program made with a preview1 adapter, whose
/// `wasi_snapshot_preview1` imports are answered through `wit-component`'s shim by the adapter.
/// An import whose name is a WIT function of the component is a lowered import (reached through
/// a shim, as `wit-component` does for a function that needs memory), which the glue implements.
fn takes_functions_from_core_instances(
    component: &[u8],
    layout: &Layout,
    index: usize,
    module: &[u8],
) -> Result<bool, TranspileError> {
    // A lone module has nothing to take a function from.
    if layout.modules.len() < 2 {
        return Ok(false);
    }
    let wiring = core_wiring(component)?;
    let Some(instance) = wiring.module_instance(index) else {
        return Ok(false);
    };
    let funcs = wit_functions(component)?;
    for payload in Parser::new(0).parse_all(module) {
        let Payload::ImportSection(reader) = payload? else {
            continue;
        };
        for import in reader.into_imports() {
            let import = import?;
            if !matches!(import.ty, TypeRef::Func(_) | TypeRef::FuncExact(_)) {
                continue;
            }
            let fed_by_a_module = matches!(
                wiring.supplier(instance, import.module, import.name),
                Some(Supplier::Export { .. })
            );
            if fed_by_a_module && !is_wit_function(&funcs, import.module, import.name)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Whether the module imports a memory, table or global from another core module: an
/// adapter's `env.memory`, in a component made with `wasm-tools component new --adapt` whose
/// adapter lifts the exports. `reentrant_imports` cannot take such a module on its own.
fn imports_state_from_core_instances(
    module: &[u8],
    linked: Option<&BTreeSet<String>>,
) -> Result<bool, TranspileError> {
    let Some(linked) = linked else {
        return Ok(false);
    };
    for payload in Parser::new(0).parse_all(module) {
        let Payload::ImportSection(reader) = payload? else {
            continue;
        };
        for import in reader.into_imports() {
            let import = import?;
            if state_import(import.module, import.ty, linked).is_some() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// The kind of a memory, table or global import that `linked` (the import module names given
/// another core instance) provides.
fn state_import(module: &str, ty: TypeRef, linked: &BTreeSet<String>) -> Option<&'static str> {
    if !linked.contains(module) {
        return None;
    }
    match ty {
        TypeRef::Memory(_) => Some("memory"),
        TypeRef::Table(_) => Some("table"),
        TypeRef::Global(_) => Some("global"),
        _ => None,
    }
}

fn select_index(layout: &Layout, exports: &[WitExport]) -> Result<usize, TranspileError> {
    if let Some(first) = exports.first() {
        return Ok(first.core_module);
    }
    match layout.modules.len() {
        0 => Err(TranspileError::InvalidInput(
            "the component has no core module".into(),
        )),
        1 => Ok(0),
        _ => {
            let candidates: Vec<usize> = layout
                .receivers
                .iter()
                .copied()
                .filter(|&i| layout.modules.get(i).is_some_and(|m| is_drivable(m)))
                .collect();
            match candidates.as_slice() {
                [only] => Ok(*only),
                [] => Err(TranspileError::InvalidInput(
                    "the component exports nothing and no core module is instantiated with \
                     lowered imports, so there is no program to pick among its core modules"
                        .into(),
                )),
                many => Err(TranspileError::InvalidInput(format!(
                    "the component exports nothing and more than one core module \
                     ({many:?}, by position) is instantiated with lowered imports: \
                     it cannot tell which is the program"
                ))),
            }
        }
    }
}

/// The top-level core modules of a component, and which of them are instantiated with core
/// functions (lowered imports, resource drops, aliased shim functions).
struct Layout {
    modules: Vec<Vec<u8>>,
    receivers: BTreeSet<usize>,
    /// Per module: the import module names that an instantiation fills with a core instance
    /// that holds a memory, table or global (an instantiated module, or an export bundle
    /// with one).
    core_linked: BTreeMap<usize, BTreeSet<String>>,
    /// The modules the component instantiates (a module may be declared and never used).
    instantiated: BTreeSet<usize>,
}

/// Reads the top-level structure of `component`, skipping the content of nested modules and
/// components (their payloads are inline, so a depth counter tells them apart).
fn scan(component: &[u8]) -> Result<Layout, TranspileError> {
    let mut layout = Layout {
        modules: Vec::new(),
        receivers: BTreeSet::new(),
        core_linked: BTreeMap::new(),
        instantiated: BTreeSet::new(),
    };
    // The module index space: the ordinal among top-level modules, or `None` for a module
    // that came from an alias or an import (not read).
    let mut module_space: Vec<Option<usize>> = Vec::new();
    // The core instance space: whether the instance exports a function.
    let mut instances: Vec<bool> = Vec::new();
    // The core instance space: whether the instance holds a memory, table or global.
    let mut holds_state: Vec<bool> = Vec::new();
    let mut depth = 0usize;
    for payload in Parser::new(0).parse_all(component) {
        let payload = payload?;
        if depth > 0 {
            match payload {
                Payload::ModuleSection { .. } | Payload::ComponentSection { .. } => {
                    depth = depth.saturating_add(1);
                }
                Payload::End(_) => depth = depth.saturating_sub(1),
                _ => {}
            }
            continue;
        }
        match payload {
            // Nested versions arrive at depth > 0, so this is the input's own.
            Payload::Version {
                encoding: Encoding::Module,
                ..
            } => {
                return Err(TranspileError::InvalidInput(
                    "the input is a core module, not a component: use `transpile`".into(),
                ));
            }
            Payload::ModuleSection {
                unchecked_range, ..
            } => {
                depth = 1;
                let bytes = usize::try_from(unchecked_range.start)
                    .ok()
                    .zip(usize::try_from(unchecked_range.end).ok())
                    .and_then(|(start, end)| component.get(start..end))
                    .ok_or_else(|| TranspileError::Internal("module range out of bounds".into()))?;
                module_space.push(Some(layout.modules.len()));
                layout.modules.push(bytes.to_vec());
            }
            Payload::ComponentSection { .. } => depth = 1,
            Payload::ComponentImportSection(reader) => {
                for import in reader {
                    if matches!(import?.ty, ComponentTypeRef::Module(_)) {
                        module_space.push(None);
                    }
                }
            }
            // An export or an outer alias of a module gives it a new index (the same entry).
            Payload::ComponentExportSection(reader) => {
                for export in reader {
                    let export = export?;
                    if export.kind == ComponentExternalKind::Module {
                        let entry = at(&module_space, export.index).flatten();
                        module_space.push(entry);
                    }
                }
            }
            Payload::ComponentAliasSection(reader) => {
                for alias in reader {
                    match alias? {
                        ComponentAlias::Outer {
                            kind: ComponentOuterAliasKind::CoreModule,
                            count: 0,
                            index,
                        } => {
                            let entry = at(&module_space, index).flatten();
                            module_space.push(entry);
                        }
                        ComponentAlias::Outer {
                            kind: ComponentOuterAliasKind::CoreModule,
                            ..
                        }
                        | ComponentAlias::InstanceExport {
                            kind: ComponentExternalKind::Module,
                            ..
                        } => module_space.push(None),
                        _ => {}
                    }
                }
            }
            Payload::InstanceSection(reader) => {
                for instance in reader {
                    match instance? {
                        Instance::FromExports(exports) => {
                            instances.push(exports.iter().any(|e| e.kind == ExternalKind::Func));
                            holds_state.push(exports.iter().any(|e| {
                                matches!(
                                    e.kind,
                                    ExternalKind::Memory
                                        | ExternalKind::Table
                                        | ExternalKind::Global
                                )
                            }));
                        }
                        Instance::Instantiate { module_index, args } => {
                            let module = at(&module_space, module_index).flatten();
                            let gets_functions = args.iter().any(|arg| {
                                // An instantiation argument of a core module is always an instance.
                                at(&instances, arg.index).unwrap_or(false)
                            });
                            if let (Some(module), true) = (module, gets_functions) {
                                layout.receivers.insert(module);
                            }
                            if let Some(module) = module {
                                layout.instantiated.insert(module);
                                for arg in args
                                    .iter()
                                    .filter(|a| at(&holds_state, a.index).unwrap_or(false))
                                {
                                    layout
                                        .core_linked
                                        .entry(module)
                                        .or_default()
                                        .insert(arg.name.to_string());
                                }
                            }
                            instances.push(false);
                            // An instantiated module may export a memory, table or global.
                            holds_state.push(true);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(layout)
}

/// The entry of an index space at `index`.
fn at<T: Copy>(space: &[T], index: u32) -> Option<T> {
    usize::try_from(index)
        .ok()
        .and_then(|i| space.get(i))
        .copied()
}

/// Whether the host can drive the module: it exports a function or has a start function.
/// One that does neither only wires other modules together.
fn is_drivable(module: &[u8]) -> bool {
    Parser::new(0)
        .parse_all(module)
        .any(|payload| match payload {
            Ok(Payload::StartSection { .. }) => true,
            Ok(Payload::ExportSection(reader)) => reader
                .into_iter()
                .any(|e| e.is_ok_and(|e| e.kind == ExternalKind::Func)),
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A component whose only lift is `main`'s `call_at`; `lib` is a second instantiated core
    /// module that shares `main`'s memory and table, with `lib_body` as its items.
    fn component(lib_body: &str) -> Vec<u8> {
        wat::parse_str(format!(
            r#"(component
    (core module $main
        (memory (export "memory") 1)
        (table (export "table") 16 funcref)
        (type $t (func (result i32)))
        (func (export "call_at") (param i32) (result i32) (call_indirect (type $t) (local.get 0))))
    (core module $lib
        (import "env" "memory" (memory 1))
        (import "env" "table" (table 16 funcref))
        {lib_body})
    (core instance $m (instantiate $main))
    (alias core export $m "memory" (core memory $mem))
    (alias core export $m "table" (core table $tbl))
    (core instance $env (export "memory" (memory $mem)) (export "table" (table $tbl)))
    (core instance $l (instantiate $lib (with "env" (instance $env))))
    (func $call_at (param "i" s32) (result s32) (canon lift (core func $m "call_at")))
    (export "call-at" (func $call_at)))"#
        ))
        .unwrap()
    }

    fn is_linked(component: &[u8]) -> bool {
        let layout = scan(component).unwrap();
        let exports = wit_exports(component).unwrap();
        matches!(
            select_program(component, &layout, &exports).unwrap(),
            Program::Linked
        )
    }

    #[test]
    fn a_sibling_that_fills_the_shared_table_is_linked() {
        let lib = r#"(func $g (result i32) i32.const 77) (elem (i32.const 7) $g)"#;
        assert!(is_linked(&component(lib)));
    }

    #[test]
    fn a_sibling_with_an_active_data_segment_is_linked() {
        assert!(is_linked(&component(r#"(data (i32.const 8) "\2a")"#)));
    }

    #[test]
    fn a_sibling_with_a_start_function_is_linked() {
        let lib = r#"(func $s (i32.store8 (i32.const 0) (i32.const 1))) (start $s)"#;
        assert!(is_linked(&component(lib)));
    }

    #[test]
    fn a_sibling_without_segments_or_start_stays_a_single_module() {
        let lib = r#"(func (export "nop"))"#;
        assert!(!is_linked(&component(lib)));
    }

    #[test]
    fn a_passive_segment_does_not_force_linking() {
        let lib = r#"(data "\2a") (func $g) (elem func $g)"#;
        assert!(!is_linked(&component(lib)));
    }

    /// `wit-component`'s shim/fixup: a table of its own that a second module fills, which the
    /// lifted module never sees.
    #[test]
    fn a_fixup_filling_a_table_of_its_own_stays_a_single_module() {
        let component = wat::parse_str(
            r#"(component
    (core module $main (func (export "f") (result i32) i32.const 1))
    (core module $shim (table (export "$imports") 1 funcref))
    (core module $fixup
        (import "" "$imports" (table 1 funcref))
        (func $g)
        (elem (i32.const 0) $g))
    (core instance $shim_i (instantiate $shim))
    (alias core export $shim_i "$imports" (core table $t))
    (core instance $args (export "$imports" (table $t)))
    (core instance $fixup_i (instantiate $fixup (with "" (instance $args))))
    (core instance $m (instantiate $main))
    (func $f (result s32) (canon lift (core func $m "f")))
    (export "f" (func $f)))"#,
        )
        .unwrap();
        assert!(!is_linked(&component));
    }

    fn split_files(component: &[u8], split: &SplitOptions) -> Vec<SourceFile> {
        let mut files = Vec::new();
        transpile_component_split(component, &ComponentOptions::default(), split, |f| {
            files.push(f);
            Ok(())
        })
        .unwrap();
        files
    }

    const BODIES: [&str; 3] = [
        r#"(func (export "nop"))"#,
        r#"(func $g (result i32) i32.const 77) (elem (i32.const 7) $g)"#,
        r#"(data (i32.const 8) "\2a")"#,
    ];

    #[test]
    fn a_single_file_split_is_the_one_string_output() {
        for body in BODIES {
            let c = component(body);
            let files = split_files(&c, &SplitOptions::single_file());
            assert_eq!(files.len(), 1);
            let whole = transpile_component(&c, &ComponentOptions::default()).unwrap();
            assert_eq!(files[0].name, "lib.rs");
            assert_eq!(files[0].code, whole);
        }
    }

    #[test]
    fn a_split_appends_the_glue_to_the_root_only() {
        let mut split_seen = false;
        for body in BODIES {
            let c = component(body);
            let whole = transpile_component(&c, &ComponentOptions::default()).unwrap();
            assert!(whole.contains("pub trait Exports"));
            let files = split_files(
                &c,
                &SplitOptions {
                    funcs_per_file: 1,
                    max_bytes_per_file: 0,
                },
            );
            let (root, chunks) = files.split_last().unwrap();
            assert_eq!(root.name, "lib.rs");
            split_seen |= files.len() > 1;
            assert!(root.code.contains("pub trait Exports"));
            assert!(chunks.iter().all(|f| !f.code.contains("pub trait Exports")));
        }
        assert!(split_seen);
    }
}
