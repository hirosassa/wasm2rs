//! Transpile a WebAssembly component in one call: its program core module, the typed import
//! glue and (optionally) the typed export side, as one Rust source.

use std::collections::BTreeSet;

use wasmparser::{
    ComponentAlias, ComponentExternalKind, ComponentOuterAliasKind, ComponentTypeRef, Encoding,
    ExternalKind, Instance, Parser, Payload,
};

use crate::wit::wit_host_glue_for_module;
use crate::{
    TranspileError, TranspileOptions, WitExport, WitOptions, transpile_with_host_imports,
    wit_exports,
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
    /// glue then refers to (see [`WitOptions::external_types`]). `None` makes the glue
    /// declare its own `Host` trait and types. Default: `Some("wasm2rs_wasi")`.
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
/// function is wiring, as `wit-component`'s fixup module, and is skipped). Other core modules
/// (shims, adapters) are not emitted.
///
/// # Errors
///
/// [`TranspileError::InvalidInput`] when the input is a core module (use
/// [`transpile`](crate::transpile)), has no core module, has exports lifted from more than one
/// module, or has several candidate modules and nothing tells them apart; plus every error of
/// [`wit_exports`](crate::wit_exports), [`transpile_with_options`] and
/// [`wit_host_glue_with_options`] (an export of a nested component, a string encoding other
/// than UTF-8, a shape the glue cannot lower). A malformed input is a [`TranspileError::Parse`].
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
    let layout = scan(component)?;
    let exports = wit_exports(component)?;
    let module = select_module(&layout, &exports)?;
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
    let (mut out, core_imports) = transpile_with_host_imports(module, &transpile)?;
    out.push('\n');
    out.push_str(&wit_host_glue_for_module(component, &wit, &core_imports)?);
    Ok((out, exports))
}

fn select_module<'a>(
    layout: &'a Layout,
    exports: &[WitExport],
) -> Result<&'a [u8], TranspileError> {
    let index = select_index(layout, exports)?;
    layout.modules.get(index).map(Vec::as_slice).ok_or_else(|| {
        TranspileError::InvalidInput(format!("core module {index} is not in the component"))
    })
}

fn select_index(layout: &Layout, exports: &[WitExport]) -> Result<usize, TranspileError> {
    if let Some(first) = exports.first() {
        if let Some(other) = exports.iter().find(|e| e.core_module != first.core_module) {
            return Err(TranspileError::InvalidInput(format!(
                "the exports come from more than one core module (`{}` and `{}`)",
                first.core_export, other.core_export
            )));
        }
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
}

/// Reads the top-level structure of `component`, skipping the content of nested modules and
/// components (their payloads are inline, so a depth counter tells them apart).
fn scan(component: &[u8]) -> Result<Layout, TranspileError> {
    let mut layout = Layout {
        modules: Vec::new(),
        receivers: BTreeSet::new(),
    };
    // The module index space: the ordinal among top-level modules, or `None` for a module
    // that came from an alias or an import (not read).
    let mut module_space: Vec<Option<usize>> = Vec::new();
    // The core instance space: whether the instance exports a function.
    let mut instances: Vec<bool> = Vec::new();
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
                            instances.push(false);
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
