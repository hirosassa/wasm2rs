//! Procedural macro that embeds a WebAssembly module as Rust source, using
//! [`wasm2rs`].
//!
//! ```ignore
//! wasm2rs_macros::include_wasm!("assets/foo.wasm");
//! wasm2rs_macros::include_wasm!("assets/foo.wasm", named_exports);
//! ```
//!
//! The path is a plain or raw string literal, relative to the crate's `CARGO_MANIFEST_DIR`; a leading
//! `$OUT_DIR/` is replaced by the `OUT_DIR` of the crate being compiled, for a
//! wasm file a build script produced. The options are `named_exports`,
//! `unsafe_memory`, `simd_intrinsics`, `named_imports`, `import_memory` and `reentrant_imports` (see
//! `wasm2rs::TranspileOptions`). The invocation expands to
//! the generated items, so wrap it in a `mod` to scope them.
//!
//! `include_component!("name")` includes the `$OUT_DIR/name.rs` that a build script wrote with
//! `wasm2rs::build::transpile_component_to_out_dir("assets/name.wasm", &options)`.
//! Pass the name without the `.wasm` extension: `include_component!("name.wasm")` is a compile error.
//!
//! The generated source is parsed into a token stream, which costs time and
//! memory on a very large module; for those prefer a build script
//! (`wasm2rs::build`) and `include!`.

use proc_macro::{TokenStream, TokenTree};
use std::path::{Path, PathBuf};

use wasm2rs::{TranspileOptions, transpile_with_options};

/// Include the Rust that `wasm2rs::build::transpile_component_to_out_dir` wrote for a component.
///
/// `include_component!("run")` expands to `include!` of `$OUT_DIR/run.rs`: the name is the input
/// file's name without its extension (a name ending in `.wasm` is a compile error).
#[proc_macro]
pub fn include_component(input: TokenStream) -> TokenStream {
    match component_file_name(input) {
        Ok(file) => format!("include!(concat!(env!(\"OUT_DIR\"), \"/\", {file:?}));")
            .parse()
            .unwrap_or_default(),
        Err(msg) => compile_error(&msg),
    }
}

/// The `{name}.rs` file of an `include_component!("name")` call.
fn component_file_name(input: TokenStream) -> Result<String, String> {
    let usage = "expected `include_component!(\"name\")`";
    let mut tokens = input.into_iter();
    let Some(TokenTree::Literal(lit)) = tokens.next() else {
        return Err(usage.into());
    };
    if tokens.next().is_some() {
        return Err(usage.into());
    }
    let name = string_literal(&lit.to_string()).ok_or(usage)?;
    component_rs_file(&name)
}

/// The `{name}.rs` that `transpile_component_to_out_dir` writes for the component `name`.
///
/// The builder names the file after the input's *file stem*, so a name ending in `.wasm` would
/// make the macro look for `run.wasm.rs` and fail with a confusing "file not found"; refuse it
/// here with the fix instead. Any other dot (`foo.bar`) belongs to the stem and is kept.
fn component_rs_file(name: &str) -> Result<String, String> {
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err(format!(
            "`{name}` is not a file name; pass the component's name without directory or extension"
        ));
    }
    if let Some(stem) = strip_wasm_extension(name) {
        return Err(format!(
            "`{name}` has the `.wasm` extension; pass the name without it: `include_component!({stem:?})`"
        ));
    }
    Ok(format!("{name}.rs"))
}

/// `name` without a trailing `.wasm` (any case, as `file_stem` would drop it), if it has one.
fn strip_wasm_extension(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    ext.eq_ignore_ascii_case("wasm").then_some(stem)
}

/// Transpile a `.wasm` file at compile time and expand to the generated Rust.
#[proc_macro]
pub fn include_wasm(input: TokenStream) -> TokenStream {
    match expand(input) {
        Ok(ts) => ts,
        Err(msg) => compile_error(&msg),
    }
}

fn expand(input: TokenStream) -> Result<TokenStream, String> {
    let (path, options) = parse_args(input)?;
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| "CARGO_MANIFEST_DIR is not set".to_string())?;
    let out_dir = std::env::var("OUT_DIR").ok();
    let resolved = resolve_path(&path, Path::new(&manifest_dir), out_dir.as_deref())?;
    let code = generate(&resolved, &options)?;
    // Re-expand when the wasm file changes: `include_bytes!` makes rustc track it.
    let track = format!(
        "const _: &[u8] = include_bytes!({:?});\n",
        resolved.to_string_lossy()
    );
    format!("{track}{code}")
        .parse()
        .map_err(|e| format!("the generated Rust could not be tokenised: {e}"))
}

fn compile_error(msg: &str) -> TokenStream {
    format!("compile_error!({msg:?});")
        .parse()
        .unwrap_or_default()
}

/// The path literal and the option identifiers of an `include_wasm!` call.
fn parse_args(input: TokenStream) -> Result<(String, Vec<String>), String> {
    let mut tokens = input.into_iter();
    let usage = "expected `include_wasm!(\"path.wasm\" [, option]*)`";
    let Some(TokenTree::Literal(lit)) = tokens.next() else {
        return Err(usage.into());
    };
    let path = string_literal(&lit.to_string()).ok_or(usage)?;
    let mut options = Vec::new();
    while let Some(tree) = tokens.next() {
        match tree {
            TokenTree::Punct(p) if p.as_char() == ',' => {}
            _ => return Err(usage.into()),
        }
        match tokens.next() {
            Some(TokenTree::Ident(id)) => options.push(id.to_string()),
            None => break, // trailing comma
            Some(_) => return Err(usage.into()),
        }
    }
    Ok((path, options))
}

/// The contents of a plain `"..."` literal with no escapes, or of a raw string
/// literal (`r"..."`, `r#"..."#`), which needs no unescaping.
fn string_literal(lit: &str) -> Option<String> {
    if let Some(raw) = lit.strip_prefix('r') {
        let hashes = raw.len() - raw.trim_start_matches('#').len();
        let inner = raw
            .get(hashes..)?
            .strip_prefix('"')?
            .strip_suffix(&format!("\"{}", "#".repeat(hashes)))?;
        return Some(inner.to_string());
    }
    let inner = lit.strip_prefix('"')?.strip_suffix('"')?;
    (!inner.contains('\\')).then(|| inner.to_string())
}

/// Resolve `path` against the manifest directory, or `$OUT_DIR/`.
fn resolve_path(path: &str, manifest_dir: &Path, out_dir: Option<&str>) -> Result<PathBuf, String> {
    if let Some(rest) = path.strip_prefix("$OUT_DIR/") {
        let out = out_dir.ok_or("OUT_DIR is not set (the crate has no build script)")?;
        return Ok(Path::new(out).join(rest));
    }
    Ok(manifest_dir.join(path))
}

/// The option names `include_wasm!` accepts, for the error message (keep in step with
/// the `match` in `generate` and the crate docs).
const OPTIONS: &str = "`named_exports`, `unsafe_memory`, `simd_intrinsics`, `named_imports`, \
                       `import_memory` or `reentrant_imports`";

/// Read and transpile the wasm file at `path` with the named options.
fn generate(path: &Path, options: &[String]) -> Result<String, String> {
    let mut opts = TranspileOptions::default();
    for name in options {
        match name.as_str() {
            "named_exports" => opts.named_exports = true,
            "unsafe_memory" => opts.unsafe_memory = true,
            "simd_intrinsics" => opts.simd_intrinsics = true,
            "named_imports" => opts.named_imports = true,
            "import_memory" => opts.import_memory = true,
            "reentrant_imports" => opts.reentrant_imports = true,
            other => {
                return Err(format!(
                    "unknown option `{other}` (expected one of {OPTIONS})"
                ));
            }
        }
    }
    let wasm = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    transpile_with_options(&wasm, &opts).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_literal_accepts_plain_and_raw_strings() {
        assert_eq!(string_literal("\"a/b.wasm\""), Some("a/b.wasm".into()));
        assert_eq!(string_literal("\"a\\\\b\""), None);
        assert_eq!(
            string_literal("r\"C:\\a\\b.wasm\""),
            Some("C:\\a\\b.wasm".into())
        );
        assert_eq!(string_literal("r#\"a\"b\"#"), Some("a\"b".into()));
        assert_eq!(string_literal("r#\"a\""), None);
        assert_eq!(string_literal("12"), None);
    }

    #[test]
    fn a_wasm_extension_is_refused_with_the_fix() {
        let err = component_rs_file("run.wasm").unwrap_err();
        assert!(err.contains("without it"), "{err}");
        assert!(err.contains("include_component!(\"run\")"), "{err}");
        assert!(component_rs_file("run.WASM").is_err());
        assert!(
            component_rs_file("foo.bar.wasm")
                .unwrap_err()
                .contains("\"foo.bar\"")
        );
        assert!(component_rs_file(".wasm").is_err());
    }

    #[test]
    fn other_dots_belong_to_the_name() {
        assert_eq!(component_rs_file("run"), Ok("run.rs".into()));
        assert_eq!(component_rs_file("foo.bar"), Ok("foo.bar.rs".into()));
        assert_eq!(component_rs_file("wasm"), Ok("wasm.rs".into()));
        assert!(component_rs_file("a/b").is_err());
        assert!(component_rs_file("").is_err());
    }

    #[test]
    fn paths_resolve_against_manifest_or_out_dir() {
        let m = Path::new("/m");
        assert_eq!(
            resolve_path("a.wasm", m, None),
            Ok(PathBuf::from("/m/a.wasm"))
        );
        assert_eq!(
            resolve_path("$OUT_DIR/a.wasm", m, Some("/o")),
            Ok(PathBuf::from("/o/a.wasm"))
        );
        assert!(resolve_path("$OUT_DIR/a.wasm", m, None).is_err());
    }

    #[test]
    fn generate_reports_bad_options_and_missing_files() {
        let missing = Path::new("/definitely/not/here.wasm");
        let err = generate(missing, &["bogus".into()]).unwrap_err();
        assert!(err.contains("unknown option `bogus`"), "{err}");
        let err = generate(missing, &[]).unwrap_err();
        assert!(err.contains("here.wasm"), "{err}");
    }

    /// Writes `wasm` to a temp `.wasm` file unique to this process and `name`.
    fn write_temp_wasm(name: &str, wasm: &[u8]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wasm2rs_macros_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.wasm");
        std::fs::write(&path, wasm).unwrap();
        path
    }

    #[test]
    fn generate_honours_named_exports() {
        let wasm: &[u8] = &[
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x07, 0x01, 0x60, 0x02, 0x7f,
            0x7f, 0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64,
            0x00, 0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b,
        ];
        let path = write_temp_wasm("add", wasm);
        assert!(!generate(&path, &[]).unwrap().contains("pub fn add("));
        assert!(
            generate(&path, &["named_exports".into()])
                .unwrap()
                .contains("pub fn add(")
        );
    }

    /// A module importing `env.f` and owning a memory, written to a temp `.wasm`.
    fn module_with_an_import(name: &str) -> PathBuf {
        let wasm: &[u8] = &[
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type: () -> ()
            0x02, 0x09, 0x01, 0x03, 0x65, 0x6e, 0x76, 0x01, 0x66, 0x00, 0x00, // import env.f
            0x03, 0x02, 0x01, 0x00, // one function
            0x05, 0x03, 0x01, 0x00, 0x01, // memory, one page
            0x07, 0x07, 0x01, 0x03, 0x72, 0x75, 0x6e, 0x00, 0x01, // export run
            0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b, // run calls the import
        ];
        write_temp_wasm(name, wasm)
    }

    #[test]
    fn generate_honours_the_import_options() {
        let path = module_with_an_import("options");
        let plain = generate(&path, &[]).unwrap();
        assert!(plain.contains("fn import0(&mut self);"), "{plain}");
        assert!(!plain.contains("env_f"), "{plain}");
        assert!(!plain.contains("memory_mut"), "{plain}");
        assert!(!plain.contains("g: &mut Instance<Self>"), "{plain}");
        let named = generate(&path, &["named_imports".into()]).unwrap();
        assert!(named.contains("fn env_f(&mut self);"), "{named}");
        let memory = generate(&path, &["import_memory".into()]).unwrap();
        assert!(memory.contains("fn memory_mut(&mut self)"), "{memory}");
        let reentrant = generate(&path, &["reentrant_imports".into()]).unwrap();
        assert!(reentrant.contains("g: &mut Instance<Self>"), "{reentrant}");
    }

    #[test]
    fn generate_reports_options_that_cannot_be_combined() {
        let path = module_with_an_import("conflict");
        let both = ["import_memory".to_string(), "reentrant_imports".to_string()];
        let err = generate(&path, &both).unwrap_err();
        assert!(err.contains("reentrant_imports"), "{err}");
    }
}
