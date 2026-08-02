//! Integration test: the Rust source produced by `transpile` must actually
//! compile with `rustc` and compute the same result as the source wasm.
//!
//! This spawns a real `rustc` process (no mocking) so it genuinely proves the
//! generated code is valid, runnable Rust.

use std::process::Command;

#[test]
fn generated_add_compiles_and_produces_correct_result() {
    let wasm = wat::parse_str(
        r#"
        (module
          (func (param i32 i32) (result i32)
            local.get 0
            local.get 1
            i32.add))
        "#,
    )
    .expect("valid wat");

    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");

    let dir = std::env::temp_dir().join("wasm2rs_it_add");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });

    // Wrap the generated function with a `main` that exercises it, including a
    // wrapping-overflow case to confirm wasm's modular arithmetic is preserved.
    let program = format!(
        "{generated}\n\
         fn main() {{\n\
         \x20   assert_eq!(func0(2, 3), 5);\n\
         \x20   assert_eq!(func0(-1, 1), 0);\n\
         \x20   assert_eq!(func0(i32::MAX, 1), i32::MIN);\n\
         }}\n"
    );
    std::fs::write(&src, program).expect("write generated source");

    let compile = Command::new("rustc")
        // Isolate each parallel rustc's codegen-unit temp objects per test dir.
        .current_dir(&dir)
        .arg(&src)
        .arg("--edition")
        .arg("2021")
        // Deny warnings so the generated code is proven to be warning-free.
        .arg("-D")
        .arg("warnings")
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        compile.status.success(),
        "generated code failed to compile:\n{}",
        String::from_utf8_lossy(&compile.stderr),
    );

    let run = Command::new(&bin).status().expect("run generated binary");
    assert!(run.success(), "generated program assertions failed");
}
