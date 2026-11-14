//! Integration tests for Phase 6b: integer division, remainder, shift and
//! rotate for i32 and i64. These carry trap semantics (division/remainder by
//! zero, and `i32::MIN / -1`), so behaviour tests assert results while trap
//! tests assert the generated program panics. All modules are stateless.

use std::process::Command;

fn compile(test: &str, wat: &str, main_body: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_intarith_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    std::fs::write(&src, &program).expect("write generated source");

    let out = Command::new("rustc")
        // Isolate each parallel rustc's codegen-unit temp objects per test dir.
        .current_dir(&dir)
        .arg(&src)
        .arg("--edition")
        .arg("2024")
        .arg("-D")
        .arg("warnings")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "generated code failed to compile:\n{program}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    bin
}

fn expect_ok(test: &str, wat: &str, main_body: &str) {
    let bin = compile(test, wat, main_body);
    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "generated program did not succeed: {test}");
}

fn expect_trap(test: &str, wat: &str, main_body: &str) {
    let bin = compile(test, wat, main_body);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert!(
        !run.status.success(),
        "expected a trap but the program exited cleanly: {test}",
    );
}

const I32_OPS: &str = r#"
    (module
      (func (param i32 i32) (result i32) (i32.div_s (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.div_u (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.rem_s (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.rem_u (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.shl (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.shr_s (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.shr_u (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.rotl (local.get 0) (local.get 1)))
      (func (param i32 i32) (result i32) (i32.rotr (local.get 0) (local.get 1))))
    "#;

#[test]
fn i32_div_rem_shift_rotate() {
    expect_ok(
        "i32ops",
        I32_OPS,
        "assert_eq!(func0(-7, 2), -3);\n    \
         assert_eq!(func1(-1, 2), 0x7FFF_FFFF);\n    \
         assert_eq!(func2(-7, 2), -1);\n    \
         assert_eq!(func2(i32::MIN, -1), 0);\n    \
         assert_eq!(func3(7, 3), 1);\n    \
         assert_eq!(func4(1, 33), 2);\n    \
         assert_eq!(func5(-8, 1), -4);\n    \
         assert_eq!(func6(-1, 28), 0xF);\n    \
         assert_eq!(func7(0x1234_5678, 8), 0x3456_7812);\n    \
         assert_eq!(func8(0x1234_5678, 8), 0x7812_3456);",
    );
}

#[test]
fn i32_div_by_zero_traps() {
    expect_trap("i32_divz", I32_OPS, "func0(1, 0);");
}

#[test]
fn i32_div_signed_overflow_traps() {
    expect_trap("i32_divov", I32_OPS, "func0(i32::MIN, -1);");
}

#[test]
fn i32_rem_by_zero_traps() {
    expect_trap("i32_remz", I32_OPS, "func3(1, 0);");
}

#[test]
fn dropped_div_by_zero_still_traps() {
    // The div result is discarded, but wasm evaluates (and traps on) the divide
    // before the drop, so the generated code must still perform the division.
    expect_trap(
        "drop_divz",
        r#"
        (module
          (func (param i32 i32)
            (drop (i32.div_s (local.get 0) (local.get 1)))))
        "#,
        "func0(1, 0);",
    );
}

#[test]
fn i64_div_shift_rotate_widths() {
    expect_ok(
        "i64ops",
        r#"
        (module
          (func (param i64 i64) (result i64) (i64.div_u (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.shr_u (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.rotl (local.get 0) (local.get 1)))
          (func (param i64 i64) (result i64) (i64.rem_s (local.get 0) (local.get 1))))
        "#,
        "assert_eq!(func0(-1, 2), i64::MAX);\n    \
         assert_eq!(func1(-1, 60), 0xF);\n    \
         assert_eq!(func2(0x0123_4567_89AB_CDEF, 8), 0x2345_6789_ABCD_EF01);\n    \
         assert_eq!(func3(i64::MIN, -1), 0);",
    );
}

/// Constant operands must still compile (rustc's deny-by-default
/// `unconditional_panic` would otherwise reject the generated program) and
/// trap at run time exactly like the non-constant case.
#[test]
fn constant_operand_traps_compile_and_trap_at_runtime() {
    let cases = [
        (
            "div_s_overflow_i32",
            "i32",
            "div_s",
            "i32.const -2147483648",
            "i32.const -1",
            "overflow",
        ),
        (
            "div_s_overflow_i64",
            "i64",
            "div_s",
            "i64.const -9223372036854775808",
            "i64.const -1",
            "overflow",
        ),
        (
            "div_s_zero",
            "i32",
            "div_s",
            "i32.const 7",
            "i32.const 0",
            "divide by zero",
        ),
        (
            "div_u_zero",
            "i64",
            "div_u",
            "i64.const 7",
            "i64.const 0",
            "divide by zero",
        ),
        (
            "rem_s_zero",
            "i32",
            "rem_s",
            "i32.const 7",
            "i32.const 0",
            "remainder",
        ),
        (
            "rem_u_zero",
            "i64",
            "rem_u",
            "i64.const 7",
            "i64.const 0",
            "remainder",
        ),
    ];
    for (name, ty, op, a, b, needle) in cases {
        let wat = format!(r#"(module (func (export "f") (result {ty}) {a} {b} {ty}.{op}))"#);
        crate::common::expect_trap_with(&format!("constdiv_{name}"), &wat, "func0();", needle);
    }
}

#[test]
fn constant_signed_remainder_of_min_by_minus_one_is_zero() {
    crate::common::compile_run(
        "constdiv_rem_min",
        r#"(module (func (export "f") (result i32) i32.const -2147483648 i32.const -1 i32.rem_s))"#,
        "assert_eq!(func0(), 0);",
    );
}

const DIV_REM_OPS: [&str; 4] = ["div_s", "div_u", "rem_s", "rem_u"];

/// A constant divisor of 0 (any op) or -1 (signed div overflow) must still
/// trap when the dividend is a runtime value, for every width/signedness.
#[test]
fn constant_divisor_traps_with_runtime_dividend() {
    for ty in ["i32", "i64"] {
        for op in DIV_REM_OPS {
            let zero = format!(
                r#"(module (func (export "f") (param {ty}) (result {ty}) local.get 0 {ty}.const 0 {ty}.{op}))"#
            );
            let message = if op.starts_with("div") {
                "divide by zero"
            } else {
                "remainder"
            };
            crate::common::expect_trap_with(
                &format!("cdiv_rt_zero_{ty}_{op}"),
                &zero,
                "func0(7);",
                message,
            );
        }
        let overflow = format!(
            r#"(module (func (export "f") (param {ty}) (result {ty}) local.get 0 {ty}.const -1 {ty}.div_s))"#
        );
        crate::common::expect_trap_with(
            &format!("cdiv_rt_ovf_{ty}"),
            &overflow,
            &format!("func0({ty}::MIN);"),
            "overflow",
        );
        // Same instruction, a dividend that is not MIN: no trap.
        crate::common::compile_run(
            &format!("cdiv_rt_ovf_ok_{ty}"),
            &overflow,
            &format!("assert_eq!(func0(6), -6);\n    assert_eq!(func0({ty}::MAX), -{ty}::MAX);"),
        );
    }
}

/// A dropped division by a constant zero must still trap (it is not pure).
#[test]
fn dropped_constant_zero_divisor_still_traps() {
    crate::common::expect_trap_with(
        "cdiv_drop_zero",
        r#"(module (func (export "f") (param i32) local.get 0 i32.const 0 i32.div_u drop))"#,
        "func0(1);",
        "divide by zero",
    );
    crate::common::expect_trap_with(
        "cdiv_drop_minus1",
        r#"(module (func (export "f") (param i64) local.get 0 i64.const -1 i64.div_s drop))"#,
        "func0(i64::MIN);",
        "overflow",
    );
}

/// Non-trapping constant divisors compute the right value for all 8 ops, and
/// unsigned ops treat -1 as the largest divisor rather than a trap.
#[test]
fn constant_nonzero_divisor_results() {
    crate::common::compile_run(
        "cdiv_results",
        r#"(module
          (func (export "a") (param i32) (result i32) local.get 0 i32.const 3 i32.div_s)
          (func (export "b") (param i32) (result i32) local.get 0 i32.const 3 i32.div_u)
          (func (export "c") (param i32) (result i32) local.get 0 i32.const 3 i32.rem_s)
          (func (export "d") (param i32) (result i32) local.get 0 i32.const 3 i32.rem_u)
          (func (export "e") (param i64) (result i64) local.get 0 i64.const 3 i64.div_s)
          (func (export "f") (param i64) (result i64) local.get 0 i64.const 3 i64.div_u)
          (func (export "g") (param i64) (result i64) local.get 0 i64.const 3 i64.rem_s)
          (func (export "h") (param i64) (result i64) local.get 0 i64.const 3 i64.rem_u)
          (func (export "i") (param i32) (result i32) local.get 0 i32.const -1 i32.div_u)
          (func (export "j") (param i64) (result i64) local.get 0 i64.const -1 i64.rem_u)
          (func (export "k") (param i32) (result i32) local.get 0 i32.const -1 i32.rem_s))"#,
        "assert_eq!(func0(-7), -2);\n    \
         assert_eq!(func1(-7), 1431655763);\n    \
         assert_eq!(func2(-7), -1);\n    \
         assert_eq!(func3(-7), 0);\n    \
         assert_eq!(func4(-7), -2);\n    \
         assert_eq!(func5(-7), 6148914691236517203);\n    \
         assert_eq!(func6(-7), -1);\n    \
         assert_eq!(func7(-7), 0);\n    \
         assert_eq!(func8(-1), 1);\n    \
         assert_eq!(func9(5), 5);\n    \
         assert_eq!(func10(i32::MIN), 0);",
    );
}

/// A non-zero (and, for signed ops, non-`-1`) constant divisor cannot trap, so
/// it must stay an inline expression rather than being pinned in a `let`.
#[test]
fn constant_nonzero_divisor_is_not_materialized() {
    let gen_for = |wat: &str| wasm2rs::transpile(&wat::parse_str(wat).expect("wat")).expect("ok");
    let pure = gen_for(
        r#"(module (func (param i32) (result i32) local.get 0 i32.const 3 i32.div_s i32.const 5 i32.rem_u))"#,
    );
    assert!(!pure.contains("let v"), "expected no temporaries:\n{pure}");
    for body in [
        "i32.const 0 i32.div_s",
        "i32.const -1 i32.div_s",
        "i32.const -1 i32.rem_s",
        "i32.const 0 i32.rem_u",
    ] {
        let wat = format!("(module (func (param i32) (result i32) local.get 0 {body}))");
        assert!(
            gen_for(&wat).contains("let v"),
            "{body} must stay materialized"
        );
    }
}

/// An inlined constant-divisor quotient must be parenthesised so a following
/// `as`-based consumer (unsigned compare, shift count, extend, wrap) casts the
/// whole quotient rather than just the divisor.
#[test]
fn inlined_constant_quotient_feeds_cast_consumers() {
    crate::common::compile_run(
        "cdiv_chain",
        r#"(module
          (func (export "a") (param i32) (result i32) local.get 0 i32.const 3 i32.div_s i32.const 5 i32.rem_u)
          (func (export "b") (param i32) (result i32) local.get 0 i32.const 3 i32.div_s i32.const 100 i32.lt_u)
          (func (export "c") (param i32) (result i64) local.get 0 i32.const 3 i32.div_s i64.extend_i32_u)
          (func (export "d") (param i32) (result i32) i32.const 1 local.get 0 i32.const 3 i32.div_s i32.shl)
          (func (export "e") (param i64) (result i32) local.get 0 i64.const 3 i64.div_s i32.wrap_i64)
          (func (export "f") (param i32) (result i32) local.get 0 i32.const 3 i32.div_s i32.extend8_s))"#,
        "assert_eq!(func0(-7), (-2i32 as u32 % 5) as i32);\n    \
         assert_eq!(func1(-7), 0);\n    \
         assert_eq!(func1(30), 1);\n    \
         assert_eq!(func2(-7), 0xFFFF_FFFE);\n    \
         assert_eq!(func3(9), 8);\n    \
         assert_eq!(func4(-9), -3);\n    \
         assert_eq!(func5(768), 0);",
    );
}
