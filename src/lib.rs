//! wasm2rs: convert a WebAssembly binary into standalone Rust source code.
//!
//! Supported scope: functions whose values are `i32` (params, results, locals),
//! using `i32.const`, `local.get`/`set`/`tee`, `i32` arithmetic/bitwise/compare
//! operators and the structured control-flow instructions `block`, `loop`,
//! `if`/`else`, `br`, `br_if`, `br_table`, `return`, `drop` and `select`.

mod codegen;

use wasmparser::{
    CompositeInnerType, DataKind, ElementItems, ElementKind, FuncType, Parser, Payload, TableInit,
    TypeRef, ValType,
};

/// An error that can occur while transpiling a wasm module.
#[derive(Debug)]
pub enum TranspileError {
    /// The wasm binary could not be parsed.
    Parse(wasmparser::BinaryReaderError),
    /// The module used a feature that is not supported yet.
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
    let mut bodies: Vec<wasmparser::FunctionBody> = Vec::new();
    let mut globals: Vec<codegen::GlobalInfo> = Vec::new();
    let mut memory: Option<codegen::MemInfo> = None;
    let mut table: Option<codegen::TableInfo> = None;
    let mut elements: Vec<codegen::ElemSegment> = Vec::new();
    let mut data: Vec<codegen::DataSegment> = Vec::new();
    let mut imports: Vec<codegen::ImportInfo> = Vec::new();
    let mut imported_globals: Vec<codegen::ImportedGlobalInfo> = Vec::new();

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
                // Function imports occupy the low end of the function index
                // space (dispatched through an injected host trait) and imported
                // globals the low end of the global index space (host getters/
                // setters). Imported memories/tables/tags are still rejected.
                // Imports may be grouped in the compact encodings, so each group
                // is expanded.
                for group in reader {
                    match group? {
                        wasmparser::Imports::Single(_, import) => {
                            classify_import(
                                import.ty,
                                &signatures,
                                &mut imports,
                                &mut imported_globals,
                            )?;
                        }
                        wasmparser::Imports::Compact1 { items, .. } => {
                            for item in items {
                                classify_import(
                                    item?.ty,
                                    &signatures,
                                    &mut imports,
                                    &mut imported_globals,
                                )?;
                            }
                        }
                        wasmparser::Imports::Compact2 { ty, names, .. } => {
                            for name in names {
                                name?;
                                classify_import(
                                    ty,
                                    &signatures,
                                    &mut imports,
                                    &mut imported_globals,
                                )?;
                            }
                        }
                    }
                }
            }
            Payload::FunctionSection(reader) => {
                for type_index in reader {
                    func_type_indices.push(type_index?);
                }
            }
            Payload::MemorySection(reader) => {
                for mem in reader {
                    let mem = mem?;
                    if mem.memory64 || mem.shared {
                        return Err(TranspileError::Unsupported(
                            "64-bit or shared memory".into(),
                        ));
                    }
                    if memory.is_some() {
                        return Err(TranspileError::Unsupported("multiple memories".into()));
                    }
                    memory = Some(codegen::MemInfo {
                        min_pages: mem.initial,
                    });
                }
            }
            Payload::TableSection(reader) => {
                for table_entry in reader {
                    let table_entry = table_entry?;
                    let ty = table_entry.ty;
                    if !ty.element_type.is_func_ref() {
                        return Err(TranspileError::Unsupported("non-funcref table".into()));
                    }
                    if ty.table64 || ty.shared {
                        return Err(TranspileError::Unsupported("64-bit or shared table".into()));
                    }
                    if table.is_some() {
                        return Err(TranspileError::Unsupported("multiple tables".into()));
                    }
                    // Only a null-initialised table is supported; its entries
                    // come from active element segments.
                    if !matches!(table_entry.init, TableInit::RefNull) {
                        return Err(TranspileError::Unsupported(
                            "table with an initializer expression".into(),
                        ));
                    }
                    let min = u32::try_from(ty.initial)
                        .map_err(|_| TranspileError::Unsupported("table too large".into()))?;
                    table = Some(codegen::TableInfo { min });
                }
            }
            Payload::ElementSection(reader) => {
                for element in reader {
                    let element = element?;
                    let offset = match &element.kind {
                        ElementKind::Active {
                            table_index,
                            offset_expr,
                        } => {
                            if table_index.unwrap_or(0) != 0 {
                                return Err(TranspileError::Unsupported(
                                    "element segment for a non-zero table".into(),
                                ));
                            }
                            Some(codegen::const_expr_u32(offset_expr)?)
                        }
                        ElementKind::Passive => None,
                        ElementKind::Declared => {
                            return Err(TranspileError::Unsupported(
                                "declared element segment".into(),
                            ));
                        }
                    };
                    let funcs = match element.items {
                        ElementItems::Functions(reader) => {
                            let mut indices = Vec::new();
                            for index in reader {
                                indices.push(index?);
                            }
                            indices
                        }
                        ElementItems::Expressions(..) => {
                            return Err(TranspileError::Unsupported(
                                "element segment with expression items".into(),
                            ));
                        }
                    };
                    elements.push(codegen::ElemSegment { offset, funcs });
                }
            }
            Payload::GlobalSection(reader) => {
                for global in reader {
                    let global = global?;
                    let init = codegen::const_expr_to_rust(&global.init_expr)?;
                    globals.push(codegen::GlobalInfo {
                        ty: global.ty.content_type,
                        mutable: global.ty.mutable,
                        init,
                    });
                }
            }
            Payload::DataSection(reader) => {
                for segment in reader {
                    let segment = segment?;
                    let offset = match &segment.kind {
                        DataKind::Active {
                            memory_index,
                            offset_expr,
                        } => {
                            if *memory_index != 0 {
                                return Err(TranspileError::Unsupported(
                                    "data segment for a non-zero memory".into(),
                                ));
                            }
                            Some(codegen::const_expr_u32(offset_expr)?)
                        }
                        DataKind::Passive => None,
                    };
                    data.push(codegen::DataSegment {
                        offset,
                        bytes: segment.data.to_vec(),
                    });
                }
            }
            Payload::CodeSectionEntry(body) => {
                bodies.push(body);
            }
            _ => {}
        }
    }

    // Resolve each function body against its declared signature.
    let mut funcs: Vec<codegen::FuncInput> = Vec::with_capacity(bodies.len());
    for (i, body) in bodies.iter().enumerate() {
        let type_index = func_type_indices
            .get(i)
            .copied()
            .ok_or_else(|| TranspileError::Unsupported("missing function type".into()))?;
        let sig = signatures.get(type_index as usize).ok_or_else(|| {
            TranspileError::Unsupported("function type index out of range".into())
        })?;
        funcs.push(codegen::FuncInput {
            params: &sig.params,
            results: &sig.results,
            body,
        });
    }

    // The module's function types, so `call_indirect` can resolve its declared
    // type index back to a signature.
    let types: Vec<codegen::TypeSig> = signatures
        .iter()
        .map(|s| codegen::TypeSig {
            params: s.params.clone(),
            results: s.results.clone(),
        })
        .collect();

    codegen::generate_module(&codegen::ModuleParts {
        imports: &imports,
        imported_globals: &imported_globals,
        funcs: &funcs,
        types: &types,
        globals: &globals,
        memory: memory.as_ref(),
        data: &data,
        table: table.as_ref(),
        elements: &elements,
    })
}

/// Classify an import by its type, pushing a function import onto `imports` or a
/// global import onto `imported_globals`. Imported memories, tables and tags are
/// rejected as not supported yet.
fn classify_import(
    ty: TypeRef,
    signatures: &[Signature],
    imports: &mut Vec<codegen::ImportInfo>,
    imported_globals: &mut Vec<codegen::ImportedGlobalInfo>,
) -> Result<(), TranspileError> {
    match ty {
        TypeRef::Func(type_index) | TypeRef::FuncExact(type_index) => {
            let sig = signatures.get(type_index as usize).ok_or_else(|| {
                TranspileError::Unsupported("import type index out of range".into())
            })?;
            imports.push(codegen::ImportInfo {
                params: sig.params.clone(),
                results: sig.results.clone(),
            });
            Ok(())
        }
        TypeRef::Global(global_ty) => {
            imported_globals.push(codegen::ImportedGlobalInfo {
                ty: global_ty.content_type,
                mutable: global_ty.mutable,
            });
            Ok(())
        }
        _ => Err(TranspileError::Unsupported(
            "imported memory, table or tag".into(),
        )),
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
        "#[allow(dead_code, unused_variables, unused_assignments, unused_mut, unused_parens)]\n";

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
    fn imported_function_generates_trait_and_generic_instance() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (import "env" "ext" (func (param i32) (result i32)))
              (func (param i32) (result i32) (call 0 (local.get 0))))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        // The import becomes a trait method and `Instance` is generic over the
        // host; the defined function is index 1 and dispatches into the host.
        assert!(
            rust.contains("pub trait Imports {")
                && rust.contains("fn import0(&mut self, a0: i32) -> i32;"),
            "{rust}"
        );
        assert!(rust.contains("pub struct Instance<H: Imports> {"), "{rust}");
        assert!(rust.contains("imports: H,"), "{rust}");
        assert!(
            rust.contains("pub fn func1(&mut self, l0: i32) -> i32")
                && rust.contains("self.imports.import0(l0)"),
            "{rust}"
        );
    }

    #[test]
    fn imported_memory_is_rejected() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (import "env" "mem" (memory 1))
              (func (param i32) (result i32) local.get 0))
            "#,
        );

        let err = transpile(&wasm).expect_err("imported memory must be rejected");
        assert!(matches!(err, TranspileError::Unsupported(_)));
    }

    #[test]
    fn result_declared_but_empty_stack_is_rejected() {
        // A function that declares a result but falls off its end with nothing
        // on the stack must be an error, rather than silently emitting an
        // empty-bodied `-> i32` function that does not compile.
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (result i32) (nop)))
            "#,
        );

        assert!(transpile(&wasm).is_err());
    }

    #[test]
    fn unreachable_body_transpiles_to_a_trap() {
        // `unreachable` terminates the function, so a declared result needs no
        // trailing value; the body is a panic (a wasm trap).
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (result i32) (unreachable)))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");
        assert!(rust.contains("panic!(\"unreachable\")"), "{rust}");
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
    fn module_with_global_emits_instance_struct() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (global i32 (i32.const 42))
              (func (result i32) (global.get 0)))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        let expected = "\
#[allow(dead_code)]
pub struct Instance {
    g0: i32,
}

#[allow(dead_code, unused_variables, unused_assignments, unused_mut, unused_parens)]
impl Instance {
    pub fn new() -> Self {
        Self {
            g0: 42i32,
        }
    }

    pub fn func0(&mut self) -> i32 {
        self.g0
    }
}";
        assert_eq!(rust.trim(), expected);
    }

    #[test]
    fn direct_call_materialises_result_into_temp() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (func (param i32 i32) (result i32) (i32.add (local.get 0) (local.get 1)))
              (func (param i32) (result i32) (call 0 (local.get 0) (i32.const 10))))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        assert_eq!(
            rust.trim(),
            format!(
                "{ATTR}pub fn func0(l0: i32, l1: i32) -> i32 {{\n    l0.wrapping_add(l1)\n}}\n\n\
                 {ATTR}pub fn func1(l0: i32) -> i32 {{\n    \
                 let v0: i32 = func0(l0, 10i32);\n    v0\n}}"
            ),
        );
    }

    #[test]
    fn active_data_segment_copies_bytes_in_new() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (memory 1)
              (data (i32.const 4) "\01\02")
              (func (param i32) (result i32) (i32.load8_u (local.get 0))))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        // `new()` zeroes the memory then copies the segment bytes into place.
        assert!(
            rust.contains("let mut m: Vec<u8> = vec![0u8; 65536];"),
            "{rust}"
        );
        assert!(
            rust.contains("m[4..6].copy_from_slice(&[1u8, 2u8]);"),
            "{rust}"
        );
    }

    #[test]
    fn call_indirect_dispatches_through_table_match() {
        let wasm = wat_to_wasm(
            r#"
            (module
              (type $unary (func (param i32) (result i32)))
              (table 1 funcref)
              (elem (i32.const 0) $id)
              (func $id (param i32) (result i32) (local.get 0))
              (func $go (param i32 i32) (result i32)
                (call_indirect (type $unary) (local.get 1) (local.get 0))))
            "#,
        );

        let rust = transpile(&wasm).expect("transpile ok");

        // The table is a `Vec<u32>` of function indices seeded from the element
        // segment, and dispatch is a `match` that traps on a null/wrong entry.
        assert!(rust.contains("table: Vec<u32>,"), "{rust}");
        assert!(rust.contains("t[0] = 0u32;"), "{rust}");
        assert!(
            rust.contains("match self.table[") && rust.contains("0u32 => self.func0("),
            "{rust}"
        );
        assert!(
            rust.contains("_ => panic!(\"indirect call type mismatch\")"),
            "{rust}"
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
