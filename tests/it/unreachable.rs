//! Integration tests for Phase 6e: the `unreachable` operator, which always
//! traps. A reached `unreachable` must panic; code after it is dead. Each
//! module is compiled with `rustc -D warnings`; the behaviour test asserts a
//! live path returns normally, the trap tests assert the program panics.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::float_cmp,
    clippy::lossy_float_literal,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::unwrap_in_result,
    reason = "test code"
)]

use std::process::Command;

fn compile(test: &str, wat: &str, main_body: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_unreach_{test}"));
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

const M: &str = r#"
    (module
      (func (result i32) (unreachable))
      (func (param i32) (result i32)
        (if (result i32) (local.get 0)
          (then (i32.const 42))
          (else (unreachable)))))
    "#;

#[test]
fn live_path_returns_normally() {
    let bin = compile("live", M, "assert_eq!(func1(1), 42);");
    let run = std::process::Command::new(&bin)
        .status()
        .expect("run generated binary");
    assert!(run.success(), "expected normal exit");
}

#[test]
fn bare_unreachable_traps() {
    let bin = compile("bare", M, "func0();");
    let run = std::process::Command::new(&bin)
        .output()
        .expect("run generated binary");
    assert!(!run.status.success(), "expected a trap");
}

#[test]
fn unreachable_in_branch_traps() {
    let bin = compile("branch", M, "func1(0);");
    let run = std::process::Command::new(&bin)
        .output()
        .expect("run generated binary");
    assert!(!run.status.success(), "expected a trap");
}

/// The trap for `unreachable` is emitted once as a `#[cold] #[inline(never)]`
/// module-scope helper, and every `unreachable` site calls it instead of
/// expanding `panic!` inline. Keeping the trap out-of-line shrinks the hot
/// function bodies (fewer inlined panic setups) and lets the optimiser lay the
/// cold path away from the hot instructions.
#[test]
fn unreachable_routed_through_cold_helper() {
    let wasm = wat::parse_str(M).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    assert!(
        generated.contains("#[cold]") && generated.contains("#[inline(never)]"),
        "expected a cold, never-inlined trap helper:\n{generated}"
    );
    assert!(
        generated.contains("fn trap_unreachable() -> ! {"),
        "expected the module-scope trap_unreachable helper:\n{generated}"
    );
    // The trap message expands exactly once — inside the helper — rather than at
    // each of the module's two `unreachable` sites.
    let inline_traps = generated.matches(r#"panic!("unreachable")"#).count();
    assert_eq!(
        inline_traps, 1,
        "the trap message should appear once (in the helper), not inline:\n{generated}"
    );
    // Both `unreachable` sites route through the helper (2 calls + 1 definition).
    let refs = generated.matches("trap_unreachable()").count();
    assert!(
        refs >= 3,
        "expected each unreachable site to call the helper, saw {refs} refs:\n{generated}"
    );
}
