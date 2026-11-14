//! Golden tests: the generated Rust must match the expected text exactly.

use wasm2rs::transpile;

fn wat_to_wasm(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).expect("valid wat")
}

/// The lint-suppression attribute prefixed to every generated function. It is a
/// literal because the tests below splice it into `format!` strings by name; the
/// exact-match assertions fail if it drifts from the generator.
const ATTR: &str = "#[allow(dead_code, unused_variables, unused_assignments, unused_mut, unused_parens, unconditional_recursion, unconditional_panic)]\n";

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
        format!("{ATTR}pub fn func0(l0: i32, l1: i32) -> i32 {{\n    (l0).wrapping_add(l1)\n}}"),
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
        format!("{ATTR}pub fn func0() -> i32 {{\n    (2i32).wrapping_mul(3i32)\n}}"),
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
        format!("{ATTR}pub fn func0(l0: i32, l1: i32) -> i32 {{\n    l0 & l1\n}}"),
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
             {ATTR}pub fn func1(l0: i32, l1: i32) -> i32 {{\n    (l0).wrapping_sub(l1)\n}}"
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
fn imported_memory_is_supported() {
    // Imported memory is host-owned; the module accesses it through the
    // injected `Imports` trait (see tests/imported_memory.rs for behaviour).
    let wasm = wat_to_wasm(
        r#"
        (module
          (import "env" "mem" (memory 1))
          (func (param i32) (result i32) (i32.load8_u (local.get 0))))
        "#,
    );

    let rust = transpile(&wasm).expect("imported memory should transpile");
    assert!(
        rust.contains("fn memory(&self) -> &[u8];") && rust.contains("self.imports.memory()"),
        "{rust}"
    );
}

#[test]
fn imported_table_lends_storage_through_the_host() {
    // An imported table is host-owned: the trait declares `table`/`table_mut`
    // accessors and the instance routes through them (no `table` field).
    let wasm = wat_to_wasm(
        r#"
        (module
          (import "env" "t" (table 1 funcref))
          (func (param i32) (result i32) local.get 0))
        "#,
    );

    let rust = transpile(&wasm).expect("imported table should transpile");
    assert!(
        rust.contains("fn table(&self) -> &[u32];") && rust.contains("self.imports.table()"),
        "{rust}"
    );
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

{ALLOW}
impl Instance {
    pub fn new() -> Self {
        Self {
            g0: 42i32,
        }
    }

    pub fn func0(&mut self) -> i32 {
        self.g0
    }
}"
    .replace("{ALLOW}", ATTR.trim_end());
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
            "{ATTR}pub fn func0(l0: i32, l1: i32) -> i32 {{\n    (l0).wrapping_add(l1)\n}}\n\n\
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
    // The bytes live in a module-scope `static` (a single string literal),
    // and `new()` copies from it.
    assert!(
        rust.contains("static DATA0: [u8; 2] = *b\"\\x01\\x02\";"),
        "{rust}"
    );
    assert!(rust.contains("m[4..6].copy_from_slice(&DATA0);"), "{rust}");
}

#[test]
fn active_data_source_stays_compact() {
    let segment = "\\ff".repeat(4096);
    let wasm = wat_to_wasm(&format!(
        r#"(module (memory 1) (data (i32.const 0) "{segment}"))"#
    ));

    let rust = transpile(&wasm).expect("transpile ok");

    // One escaped byte costs 4 characters, so the segment's static stays
    // under 5 per byte (measured on its own line, not the whole module).
    let start = rust.find("static DATA0").expect("DATA0 static");
    let len = rust[start..].find("\";").expect("end of DATA0 static");
    assert!(len < 4096 * 5, "{len} bytes for the static");
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
    // segment; dispatch reads the entry through the `table()` accessor into a
    // local, then a `match` traps on a null/wrong entry.
    assert!(rust.contains("table: Vec<u32>,"), "{rust}");
    assert!(rust.contains("t[0] = 0u32;"), "{rust}");
    assert!(
        rust.contains("= self.table()[") && rust.contains("0u32 => self.func0("),
        "{rust}"
    );
    assert!(
        rust.contains("_ => trap_indirect_type_mismatch(),")
            && rust.contains("fn trap_indirect_type_mismatch() -> ! {"),
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
