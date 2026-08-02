//! wasm2rs: convert a WebAssembly binary into standalone Rust source code.
//!
//! Phase 1 scope: functions whose parameters, results and locals are all `i32`,
//! whose bodies use only `i32.const`, `local.get` and a handful of `i32` binary
//! arithmetic/bitwise operators. Each such function is emitted as a standalone
//! `pub fn`.

use std::fmt::Write as _;

use wasmparser::{
    CompositeInnerType, FuncType, Imports, Operator, Parser, Payload, TypeRef, ValType,
};

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

                let code = emit_function(func_index, sig, &body)?;
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

/// Render a wasm value type as the corresponding Rust type name.
fn rust_type(ty: ValType) -> Result<&'static str, TranspileError> {
    match ty {
        ValType::I32 => Ok("i32"),
        ValType::I64 => Ok("i64"),
        ValType::F32 => Ok("f32"),
        ValType::F64 => Ok("f64"),
        other => Err(TranspileError::Unsupported(format!("value type {other:?}"))),
    }
}

/// Emit a single standalone Rust function for one wasm function body.
fn emit_function(
    index: usize,
    sig: &Signature,
    body: &wasmparser::FunctionBody<'_>,
) -> Result<String, TranspileError> {
    // Validate the local declarations even though Phase 1 references locals
    // purely by index; this surfaces a malformed locals section as an error.
    for local in body.get_locals_reader()? {
        local?;
    }

    let expr = emit_body_expr(body)?;

    let mut params = String::new();
    for (i, ty) in sig.params.iter().enumerate() {
        if i > 0 {
            params.push_str(", ");
        }
        write!(params, "l{i}: {}", rust_type(*ty)?)?;
    }

    // The number of values left on the operand stack must match the declared
    // results, otherwise the emitted function body would not compile.
    let ret = match (sig.results.as_slice(), &expr) {
        ([], None) => String::new(),
        ([ty], Some(_)) => format!(" -> {}", rust_type(*ty)?),
        ([], Some(_)) => {
            return Err(TranspileError::Unsupported(
                "value left on stack in a function with no result".into(),
            ));
        }
        ([_], None) => return Err(TranspileError::StackUnderflow),
        _ => return Err(TranspileError::Unsupported("multi-value results".into())),
    };

    let signature = format!("pub fn func{index}({params}){ret}");
    match expr {
        Some(e) => Ok(format!("{signature} {{\n    {e}\n}}\n")),
        None => Ok(format!("{signature} {{\n}}\n")),
    }
}

/// Simulate the operand stack over a function body and return the single
/// expression left on the stack at `End`, if any.
fn emit_body_expr(body: &wasmparser::FunctionBody<'_>) -> Result<Option<String>, TranspileError> {
    let mut stack: Vec<String> = Vec::new();

    for op in body.get_operators_reader()? {
        match op? {
            Operator::LocalGet { local_index } => {
                stack.push(format!("l{local_index}"));
            }
            Operator::I32Const { value } => {
                stack.push(i32_literal(value));
            }
            Operator::I32Add => binop_method(&mut stack, "wrapping_add")?,
            Operator::I32Sub => binop_method(&mut stack, "wrapping_sub")?,
            Operator::I32Mul => binop_method(&mut stack, "wrapping_mul")?,
            Operator::I32And => binop_infix(&mut stack, "&")?,
            Operator::I32Or => binop_infix(&mut stack, "|")?,
            Operator::I32Xor => binop_infix(&mut stack, "^")?,
            Operator::End => {}
            other => {
                return Err(TranspileError::Unsupported(format!("operator {other:?}")));
            }
        }
    }

    Ok(stack.pop())
}

/// Render an `i32` constant as a valid Rust expression.
///
/// `i32::MIN` cannot be written as the literal `-2147483648i32` because Rust
/// parses that as unary negation of the out-of-range literal `2147483648i32`,
/// so it is emitted using the associated constant instead.
fn i32_literal(value: i32) -> String {
    if value == i32::MIN {
        "i32::MIN".to_string()
    } else {
        format!("{value}i32")
    }
}

/// Pop two operands and push a method-call expression `lhs.method(rhs)`.
fn binop_method(stack: &mut Vec<String>, method: &str) -> Result<(), TranspileError> {
    let rhs = stack.pop().ok_or(TranspileError::StackUnderflow)?;
    let lhs = stack.pop().ok_or(TranspileError::StackUnderflow)?;
    stack.push(format!("{lhs}.{method}({rhs})"));
    Ok(())
}

/// Pop two operands and push a parenthesized infix expression `(lhs op rhs)`.
fn binop_infix(stack: &mut Vec<String>, op: &str) -> Result<(), TranspileError> {
    let rhs = stack.pop().ok_or(TranspileError::StackUnderflow)?;
    let lhs = stack.pop().ok_or(TranspileError::StackUnderflow)?;
    stack.push(format!("({lhs} {op} {rhs})"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wat_to_wasm(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).expect("valid wat")
    }

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
            "pub fn func0(l0: i32, l1: i32) -> i32 {\n    l0.wrapping_add(l1)\n}",
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
            "pub fn func0() -> i32 {\n    2i32.wrapping_mul(3i32)\n}",
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
            "pub fn func0(l0: i32, l1: i32) -> i32 {\n    (l0 & l1)\n}",
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
            concat!(
                "pub fn func0(l0: i32) -> i32 {\n    l0\n}\n\n",
                "pub fn func1(l0: i32, l1: i32) -> i32 {\n    l0.wrapping_sub(l1)\n}",
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

        assert_eq!(rust.trim(), "pub fn func0() -> i32 {\n    i32::MIN\n}",);
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
    fn function_without_result_has_no_return_type() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32)))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(rust.trim(), "pub fn func0(l0: i32) {\n}");
    }
}
