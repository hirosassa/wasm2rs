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
//! wasm file a build script produced. The options are `named_exports` and
//! `unsafe_memory` (see `wasm2rs::TranspileOptions`). The invocation expands to
//! the generated items, so wrap it in a `mod` to scope them.
//!
//! The generated source is parsed into a token stream, which costs time and
//! memory on a very large module; for those prefer a build script
//! (`wasm2rs::build`) and `include!`.

use proc_macro::{TokenStream, TokenTree};
use std::path::{Path, PathBuf};

use wasm2rs::{TranspileOptions, transpile_with_options};

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

/// Read and transpile the wasm file at `path` with the named options.
fn generate(path: &Path, options: &[String]) -> Result<String, String> {
    let mut opts = TranspileOptions::default();
    for name in options {
        match name.as_str() {
            "named_exports" => opts.named_exports = true,
            "unsafe_memory" => opts.unsafe_memory = true,
            other => {
                return Err(format!(
                    "unknown option `{other}` (expected `named_exports` or `unsafe_memory`)"
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

    #[test]
    fn generate_honours_named_exports() {
        let dir = std::env::temp_dir().join(format!("wasm2rs_macros_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("add.wasm");
        let wasm: &[u8] = &[
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x07, 0x01, 0x60, 0x02, 0x7f,
            0x7f, 0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64,
            0x00, 0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b,
        ];
        std::fs::write(&path, wasm).unwrap();
        assert!(!generate(&path, &[]).unwrap().contains("pub fn add("));
        assert!(
            generate(&path, &["named_exports".into()])
                .unwrap()
                .contains("pub fn add(")
        );
    }
}
