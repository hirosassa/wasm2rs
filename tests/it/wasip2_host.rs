//! The minimal WASI preview2 host in `examples/wasip2_host/host.rs`, driven by a
//! small module that imports the same lowered functions a `wasm32-wasip2` hello
//! world does. The host is compiled together with the generated code; the module
//! writes to stdout through the canonical-ABI result layouts and exits.

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

const HOST: &str = include_str!("../../examples/wasip2_host/host.rs");

/// The 21 imports of a `println!("hello")` program, with their lowered types
/// (`(i32)`: resource handle or return pointer; `check-write` and `flush` take the
/// stream and a return pointer; `write` also a pointer and a length), plus a
/// `run` that checks the `check-write` and `write` result layouts and writes
/// "hi\n", and a `bye` that exits with an error status.
const HELLO: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func (param i32)))
  (import "wasi:cli/terminal-input@0.2.0" "[resource-drop]terminal-input" (func (param i32)))
  (import "wasi:cli/terminal-output@0.2.0" "[resource-drop]terminal-output" (func (param i32)))
  (import "wasi:cli/environment@0.2.0" "get-environment" (func (param i32)))
  (import "wasi:cli/exit@0.2.0" "exit" (func $exit (param i32)))
  (import "wasi:io/poll@0.2.0" "[method]pollable.block" (func (param i32)))
  (import "wasi:io/poll@0.2.0" "poll" (func (param i32 i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.check-write" (func $check (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.blocking-flush" (func $flush (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.subscribe" (func (param i32) (result i32)))
  (import "wasi:cli/stdin@0.2.0" "get-stdin" (func (result i32)))
  (import "wasi:cli/stdout@0.2.0" "get-stdout" (func $stdout (result i32)))
  (import "wasi:cli/stderr@0.2.0" "get-stderr" (func (result i32)))
  (import "wasi:cli/terminal-stdin@0.2.0" "get-terminal-stdin" (func (param i32)))
  (import "wasi:cli/terminal-stdout@0.2.0" "get-terminal-stdout" (func (param i32)))
  (import "wasi:cli/terminal-stderr@0.2.0" "get-terminal-stderr" (func (param i32)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-duration" (func (param i64) (result i32)))
  (memory 1)
  (data (i32.const 64) "hi\n")
  (func (export "run") (local $out i32)
    (local.set $out (call $stdout))
    ;; result<u64, stream-error> at 16: ok tag at 16, the budget at 24
    (call $check (local.get $out) (i32.const 16))
    (if (i32.load8_u (i32.const 16)) (then unreachable))
    (if (i64.ne (i64.load (i32.const 24)) (i64.const 4096)) (then unreachable))
    ;; result<_, stream-error> at 32
    (i32.store8 (i32.const 32) (i32.const 9))
    (call $write (local.get $out) (i32.const 64) (i32.const 3) (i32.const 32))
    (if (i32.load8_u (i32.const 32)) (then unreachable))
    (call $flush (local.get $out) (i32.const 40))
    (if (i32.load8_u (i32.const 40)) (then unreachable)))
  (func (export "bye") (call $exit (i32.const 1))))
"#;

/// The host with the `// optional: <method>` blocks dropped unless the generated
/// trait declares that method (the same filter `examples/wasip2_run.rs` applies).
fn keep_declared_methods(host: &str, generated: &str) -> String {
    let mut out = Vec::new();
    let mut skipping = false;
    for line in host.lines() {
        if let Some(method) = line.trim().strip_prefix("// optional: ") {
            skipping = !generated.contains(&format!("fn {method}("));
        } else if line.trim() == "// end optional" {
            skipping = false;
        } else if !skipping {
            out.push(line);
        }
    }
    out.join("\n")
}

fn compile(test: &str, wat: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let opts = wasm2rs::TranspileOptions {
        named_exports: true,
        named_imports: true,
        import_memory: true,
        ..wasm2rs::TranspileOptions::default()
    };
    let generated = wasm2rs::transpile_with_options(&wasm, &opts).expect("transpile ok");

    let dir = std::env::temp_dir().join(format!("wasm2rs_wasip2_host_{test}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join(if cfg!(windows) { "gen.exe" } else { "gen" });
    let main = "fn main() {\n    \
        let mut inst = Instance::new(P2Host::new(1));\n    \
        inst.run();\n    \
        inst.bye();\n    \
        }";
    let host = keep_declared_methods(HOST, &generated);
    std::fs::write(&src, format!("{generated}\n{host}\n{main}\n")).expect("write source");

    let out = Command::new("rustc")
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
        "generated code failed to compile:\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr),
    );
    bin
}

#[test]
fn a_preview2_module_writes_to_stdout_and_exits_through_the_host() {
    let bin = compile("hello", HELLO);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hi\n");
    assert_eq!(run.status.code(), Some(1), "exit(error) should exit with 1");
}

#[test]
fn a_module_importing_get_arguments_gets_the_optional_host_method() {
    // A std program also imports `get-arguments`; the host provides it only then.
    let wat = HELLO.replacen(
        "(memory 1)",
        "(import \"wasi:cli/environment@0.2.0\" \"get-arguments\" (func (param i32)))\n  (memory 1)",
        1,
    );
    let bin = compile("with_arguments", &wat);
    let run = Command::new(&bin).output().expect("run generated binary");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hi\n");
}
