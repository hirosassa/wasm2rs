//! wasm2rs: convert a WebAssembly binary into standalone Rust source code.
//!
//! Supported scope: functions whose values are `i32` (params, results, locals),
//! using `i32.const`, `local.get`/`set`/`tee`, `i32` arithmetic/bitwise/compare
//! operators and the structured control-flow instructions `block`, `loop`,
//! `if`/`else`, `br`, `br_if`, `br_table`, `return`, `drop` and `select`.

mod codegen;

use wasmparser::{CompositeInnerType, FuncType, Imports, Parser, Payload, TypeRef, ValType};

/// An error that can occur while transpiling a wasm module.
#[derive(Debug)]
pub enum TranspileError {
    /// The wasm binary could not be parsed.
    Parse(wasmparser::BinaryReaderError),
    /// The module used a feature that Phase 1 does not support yet.
    Unsupported(String),
    /// The operand stack was empty when a value was required, indicating a
    /// malformed module (or a bug in the transpiler).
    StackUnderflow,
}

impl std::fmt::Display for TranspileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "failed to parse wasm: {e}"),
            Self::Unsupported(what) => write!(f, "unsupported: {what}"),
            Self::StackUnderflow => write!(f, "operand stack underflow"),
        }
    }
}

impl std::error::Error for TranspileError {}

impl From<wasmparser::BinaryReaderError> for TranspileError {
    fn from(e: wasmparser::BinaryReaderError) -> Self {
        Self::Parse(e)
    }
}

impl From<std::fmt::Error> for TranspileError {
    fn from(_: std::fmt::Error) -> Self {
        // Writing into an in-memory `String` is infallible in practice; this
        // conversion exists only to satisfy `write!`'s `Result` contract.
        Self::Unsupported("string formatting failed".into())
    }
}

/// The signature of a wasm function: parameter and result value types.
struct Signature {
    params: Vec<ValType>,
    results: Vec<ValType>,
}

/// Transpile a wasm binary into Rust source code.
pub fn transpile(wasm: &[u8]) -> Result<String, TranspileError> {
    let mut signatures: Vec<Signature> = Vec::new();
    let mut func_type_indices: Vec<u32> = Vec::new();
    let mut out = String::new();
    let mut func_index = 0usize;

    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::TypeSection(reader) => {
                for group in reader {
                    for sub in group?.types() {
                        match &sub.composite_type.inner {
                            CompositeInnerType::Func(func_ty) => {
                                signatures.push(signature_from(func_ty));
                            }
                            _ => {
                                return Err(TranspileError::Unsupported(
                                    "non-function composite type".into(),
                                ));
                            }
                        }
                    }
                }
            }
            Payload::ImportSection(reader) => {
                // An imported function occupies a function index ahead of the
                // locally-defined ones, which would desynchronise the `func{n}`
                // naming from the wasm function index space. Phase 1 does not
                // model imports, so reject imported functions outright.
                for group in reader {
                    match group? {
                        Imports::Single(_, import) => reject_func_import(import.ty)?,
                        Imports::Compact1 { items, .. } => {
                            for item in items {
                                reject_func_import(item?.ty)?;
                            }
                        }
                        Imports::Compact2 { ty, .. } => reject_func_import(ty)?,
                    }
                }
            }
            Payload::FunctionSection(reader) => {
                for type_index in reader {
                    func_type_indices.push(type_index?);
                }
            }
            Payload::CodeSectionEntry(body) => {
                let type_index = func_type_indices
                    .get(func_index)
                    .copied()
                    .ok_or_else(|| TranspileError::Unsupported("missing function type".into()))?;
                let sig = signatures.get(type_index as usize).ok_or_else(|| {
                    TranspileError::Unsupported("function type index out of range".into())
                })?;

                let code =
                    codegen::generate_function(func_index, &sig.params, &sig.results, &body)?;
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&code);
                func_index += 1;
            }
            _ => {}
        }
    }

    Ok(out)
}

/// Reject an imported function; other kinds of imports are ignored in Phase 1.
fn reject_func_import(ty: TypeRef) -> Result<(), TranspileError> {
    match ty {
        TypeRef::Func(_) | TypeRef::FuncExact(_) => {
            Err(TranspileError::Unsupported("imported function".into()))
        }
        _ => Ok(()),
    }
}

fn signature_from(func_ty: &FuncType) -> Signature {
    Signature {
        params: func_ty.params().to_vec(),
        results: func_ty.results().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wat_to_wasm(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).expect("valid wat")
    }

    /// The lint-suppression attribute prefixed to every generated function.
    const ATTR: &str =
        "#[allow(unused_variables, unused_assignments, unused_mut, unused_parens)]\n";

    #[test]
    fn i32_add_becomes_wrapping_add() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.add))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!("{ATTR}pub fn func0(l0: i32, l1: i32) -> i32 {{\n    l0.wrapping_add(l1)\n}}"),
        );
    }

    #[test]
    fn i32_const_and_mul() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (result i32)
                i32.const 2
                i32.const 3
                i32.mul))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!("{ATTR}pub fn func0() -> i32 {{\n    2i32.wrapping_mul(3i32)\n}}"),
        );
    }

    #[test]
    fn bitwise_and_is_parenthesized_infix() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.and))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!("{ATTR}pub fn func0(l0: i32, l1: i32) -> i32 {{\n    (l0 & l1)\n}}"),
        );
    }

    #[test]
    fn multiple_functions_get_sequential_names() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32) (result i32) local.get 0)
              (func (param i32 i32) (result i32)
                local.get 0
                local.get 1
                i32.sub))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!(
                "{ATTR}pub fn func0(l0: i32) -> i32 {{\n    l0\n}}\n\n\
                 {ATTR}pub fn func1(l0: i32, l1: i32) -> i32 {{\n    l0.wrapping_sub(l1)\n}}"
            ),
        );
    }

    #[test]
    fn i32_min_const_uses_valid_literal() {
        // `-2147483648i32` is a range error in Rust (parsed as unary minus on
        // an out-of-range literal), so `i32::MIN` must be emitted specially.
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (result i32)
                i32.const -2147483648))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!("{ATTR}pub fn func0() -> i32 {{\n    i32::MIN\n}}"),
        );
    }

    #[test]
    fn imported_function_is_rejected() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (import "env" "ext" (func (param i32) (result i32)))
              (func (param i32) (result i32) local.get 0))
            "#,
        );

        let err = transpile(&wasm).expect_err("imported function must be rejected");
        assert!(matches!(err, TranspileError::Unsupported(_)));
    }

    #[test]
    fn result_declared_but_empty_stack_is_rejected() {
        // A function that declares a result but leaves nothing on the stack
        // would otherwise produce a body that does not compile.
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (result i32) (unreachable)))
            "#,
        );

        // `unreachable` is not supported yet, so this must be an error rather
        // than silently emitting an empty-bodied `-> i32` function.
        assert!(transpile(&wasm).is_err());
    }

    #[test]
    fn i32_eq_uses_i32_from() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32 i32) (result i32)
                (i32.eq (local.get 0) (local.get 1))))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!("{ATTR}pub fn func0(l0: i32, l1: i32) -> i32 {{\n    i32::from(l0 == l1)\n}}"),
        );
    }

    #[test]
    fn i32_lt_u_casts_operands_to_u32() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32 i32) (result i32)
                (i32.lt_u (local.get 0) (local.get 1))))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!(
                "{ATTR}pub fn func0(l0: i32, l1: i32) -> i32 {{\n    i32::from((l0 as u32) < (l1 as u32))\n}}"
            ),
        );
    }

    #[test]
    fn declared_local_is_initialized_and_assigned() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32) (result i32) (local i32)
                (local.set 1 (local.get 0))
                (local.get 1)))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!(
                "{ATTR}pub fn func0(l0: i32) -> i32 {{\n    let mut l1: i32 = 0;\n    l1 = l0;\n    l1\n}}"
            ),
        );
    }

    #[test]
    fn function_without_result_has_no_return_type() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32)))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(rust.trim(), format!("{ATTR}pub fn func0(l0: i32) {{\n}}"));
    }
}
