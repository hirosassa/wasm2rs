//! Transpile a WebAssembly component in one call: its program core module, the typed import
//! glue and (optionally) the typed export side, as one Rust source.

use std::collections::BTreeSet;

use wasmparser::{
    ComponentAlias, ComponentExternalKind, ComponentOuterAliasKind, ComponentTypeRef, Encoding,
    ExternalKind, Instance, InstantiationArgKind, Parser, Payload,
};

use crate::{
    TranspileError, TranspileOptions, WitOptions, transpile_with_options, wit_exports,
    wit_host_glue_with_options,
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
/// [`TranspileError::Unsupported`] when the input is a core module (use
/// [`transpile`](crate::transpile)), has no core module, has exports lifted from more than one
/// module, or has several candidate modules and nothing tells them apart; plus every error of
/// [`wit_exports`](crate::wit_exports), [`transpile_with_options`] and
/// [`wit_host_glue_with_options`] (an export of a nested component, a string encoding other
/// than UTF-8, a shape the glue cannot lower). A malformed input is a [`TranspileError::Parse`].
pub fn transpile_component(
    component: &[u8],
    options: &ComponentOptions,
) -> Result<String, TranspileError> {
    let layout = scan(component)?;
    let exports = wit_exports(component)?;
    let index = select_module(&layout, &exports)?;
    let module = layout.modules.get(index).ok_or_else(|| {
        TranspileError::Unsupported(format!("core module {index} is not in the component"))
    })?;
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
    let mut out = transpile_with_options(module, &transpile)?;
    out.push('\n');
    out.push_str(&wit_host_glue_with_options(component, &wit)?);
    Ok(out)
}

fn select_module(layout: &Layout, exports: &[crate::WitExport]) -> Result<usize, TranspileError> {
    if let Some(first) = exports.first() {
        if let Some(other) = exports.iter().find(|e| e.core_module != first.core_module) {
            return Err(TranspileError::Unsupported(format!(
                "the exports come from more than one core module (`{}` and `{}`)",
                first.core_export, other.core_export
            )));
        }
        return Ok(first.core_module);
    }
    match layout.modules.len() {
        0 => Err(TranspileError::Unsupported(
            "the component has no core module".into(),
        )),
        1 => Ok(0),
        _ => {
            let mut candidates = Vec::new();
            for &i in &layout.receivers {
                if layout.modules.get(i).is_some_and(|m| is_drivable(m)) {
                    candidates.push(i);
                }
            }
            match candidates.as_slice() {
                [only] => Ok(*only),
                [] => Err(TranspileError::Unsupported(
                    "the component exports nothing and no core module is instantiated with \
                     lowered imports, so there is no program to pick among its core modules"
                        .into(),
                )),
                many => Err(TranspileError::Unsupported(format!(
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
    let mut first = true;
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
            Payload::Version { encoding, .. } if first => {
                first = false;
                if encoding == Encoding::Module {
                    return Err(TranspileError::Unsupported(
                        "the input is a core module, not a component: use `transpile`".into(),
                    ));
                }
            }
            Payload::ModuleSection {
                unchecked_range, ..
            } => {
                depth = 1;
                let bytes = usize::try_from(unchecked_range.start)
                    .ok()
                    .zip(usize::try_from(unchecked_range.end).ok())
                    .and_then(|(start, end)| component.get(start..end))
                    .ok_or_else(|| {
                        TranspileError::Unsupported("module range out of bounds".into())
                    })?;
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
            Payload::ComponentAliasSection(reader) => {
                for alias in reader {
                    match alias? {
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
                            let module = usize::try_from(module_index)
                                .ok()
                                .and_then(|i| module_space.get(i))
                                .copied()
                                .flatten();
                            let gets_functions = args.iter().any(|arg| {
                                arg.kind == InstantiationArgKind::Instance
                                    && usize::try_from(arg.index)
                                        .ok()
                                        .and_then(|i| instances.get(i))
                                        .copied()
                                        .unwrap_or(false)
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
