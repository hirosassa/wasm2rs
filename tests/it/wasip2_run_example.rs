//! `examples/wasip2_run.rs` end to end: a real component (a core module wrapped with
//! `wit-component`, importing and exporting what a `wasm32-wasip2` command does) goes in,
//! the example transpiles its core module, adds the glue and the typed host, builds and
//! runs it.

use std::process::{Command, Output};

use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
use wit_parser::Resolve;

pub(crate) const IO: &str = r#"
package wasi:io@0.2.0;
interface error {
  resource error;
}
interface streams {
  use error.{error};
  resource input-stream;
  resource output-stream {
    write: func(contents: list<u8>) -> result<_, stream-error>;
  }
  variant stream-error {
    last-operation-failed(error),
    closed,
  }
}
"#;

pub(crate) const CLI: &str = r#"
package wasi:cli@0.2.0;
interface environment {
  get-arguments: func() -> list<string>;
}
interface stdout {
  use wasi:io/streams@0.2.0.{output-stream};
  get-stdout: func() -> output-stream;
}
interface run {
  run: func() -> result;
}
"#;

pub(crate) const WORLD: &str = r#"
package test:t;
world w {
  import wasi:cli/environment@0.2.0;
  import wasi:cli/stdout@0.2.0;
  export wasi:cli/run@0.2.0;
}
"#;

/// `run` writes its second argument to stdout (nothing without one) and returns the
/// error result when it got a third.
pub(crate) const CORE: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func $drop_out (param i32)))
  (import "wasi:cli/environment@0.2.0" "get-arguments" (func $args (param i32)))
  (import "wasi:cli/stdout@0.2.0" "get-stdout" (func $stdout (result i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 1024))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (func (export "wasi:cli/run@0.2.0#run") (result i32) (local $argc i32) (local $list i32) (local $out i32)
    (call $args (i32.const 72))
    (local.set $list (i32.load (i32.const 72)))
    (local.set $argc (i32.load (i32.const 76)))
    (if (i32.ge_u (local.get $argc) (i32.const 2))
      (then
        (local.set $out (call $stdout))
        (call $write (local.get $out)
          (i32.load offset=8 (local.get $list))
          (i32.load offset=12 (local.get $list))
          (i32.const 100))
        (call $drop_out (local.get $out))))
    (i32.gt_u (local.get $argc) (i32.const 2))))
"#;

pub(crate) fn component() -> Vec<u8> {
    component_of(WORLD, CORE)
}

/// A component of `world` (in the package `world_wit`, after `IO` and the `cli` package)
/// around the core module `core`.
pub(crate) fn component_of(world_wit: &str, core: &str) -> Vec<u8> {
    let mut resolve = Resolve::default();
    resolve.push_str("io.wit", IO).unwrap();
    resolve.push_str("cli.wit", CLI).unwrap();
    let package = resolve.push_str("test.wit", world_wit).unwrap();
    let world = resolve.select_world(&[package], Some("w")).unwrap();
    let mut module = wat::parse_str(core).unwrap();
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8, false).unwrap();
    ComponentEncoder::default()
        .module(&module)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap()
}

/// Runs `cargo run --example wasip2_run -- <component> <args>` with the work dir `tag`.
fn run_example(component: &[u8], tag: &str, args: &[&str]) -> Output {
    let dir =
        std::env::temp_dir().join(format!("wasm2rs-run-example-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wasm = dir.join("component.wasm");
    std::fs::write(&wasm, component).unwrap();
    Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["run", "--quiet", "--example", "wasip2_run", "--"])
        .arg(&wasm)
        .args(args)
        .env("WASIP2_WORK_DIR", dir.join("work"))
        .output()
        .unwrap()
}

#[test]
fn a_component_prints_its_argument_through_the_typed_host() {
    let out = run_example(&component(), "hello", &["hello"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hello",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success());
}

#[test]
fn an_error_result_from_run_is_a_nonzero_exit_code() {
    let out = run_example(&component(), "fail", &["a", "b"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a");
}

#[test]
fn an_import_outside_the_wasi_surface_fails_with_a_hint() {
    let world = r#"
package test:t;
interface unknown {
  ping: func();
}
world w {
  import unknown;
  export wasi:cli/run@0.2.0;
}
"#;
    let core = r#"
(module
  (import "test:t/unknown" "ping" (func $ping))
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 0))
  (func (export "wasi:cli/run@0.2.0#run") (result i32) (call $ping) (i32.const 0)))
"#;
    let out = run_example(&component_of(world, core), "unknown", &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("E0407"), "{stderr}");
    assert!(
        stderr.contains("outside the WASI 0.2.0 surface"),
        "{stderr}"
    );
}

/// `run` writes its first argument (argv[0]) to stdout.
const CORE_ARGV0: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func $drop_out (param i32)))
  (import "wasi:cli/environment@0.2.0" "get-arguments" (func $args (param i32)))
  (import "wasi:cli/stdout@0.2.0" "get-stdout" (func $stdout (result i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 1024))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (func (export "wasi:cli/run@0.2.0#run") (result i32) (local $list i32) (local $out i32)
    (call $args (i32.const 72))
    (local.set $list (i32.load (i32.const 72)))
    (local.set $out (call $stdout))
    (call $write (local.get $out) (i32.load (local.get $list)) (i32.load offset=4 (local.get $list)) (i32.const 100))
    (call $drop_out (local.get $out))
    (i32.const 0)))
"#;

#[test]
fn argv0_is_the_component_path_given_on_the_command_line() {
    let out = run_example(&component_of(WORLD, CORE_ARGV0), "argv0", &["x"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.ends_with("component.wasm") && !stdout.ends_with("/program"),
        "{stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("wasm2rs-run-example"), "{stdout}");
}

#[test]
fn a_guest_that_traps_exits_134_after_its_own_message() {
    let core = r#"
(module
  (import "wasi:cli/environment@0.2.0" "get-arguments" (func $args (param i32)))
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 0))
  (func (export "wasi:cli/run@0.2.0#run") (result i32) unreachable))
"#;
    let world = r#"
package test:t;
world w {
  import wasi:cli/environment@0.2.0;
  export wasi:cli/run@0.2.0;
}
"#;
    let out = run_example(&component_of(world, core), "trap", &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(134), "{stderr}");
    assert!(stderr.contains("unreachable"), "{stderr}");
    assert!(
        stderr.contains("wasip2_run: the program trapped"),
        "{stderr}"
    );
    assert_eq!(stderr.matches("panicked").count(), 1, "{stderr}");
}

#[test]
fn a_component_without_a_run_export_exits_2_with_a_message() {
    let world = r#"
package test:t;
world w {
  export wasi:cli/environment@0.2.0;
}
"#;
    let core = r#"
(module
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 0))
  (func (export "wasi:cli/environment@0.2.0#get-arguments") (result i32) (i32.const 0)))
"#;
    let out = run_example(&component_of(world, core), "no_run", &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("does not export `wasi:cli/run@0.2.0#run`"),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn a_run_that_is_not_lifted_directly_exits_2_with_the_unsupported_message() {
    // A component whose export is an instance of a nested component with its own core
    // module is not the shape wit-component's shim produces.
    let component = wat::parse_str(
        r#"
(component
  (component $inner
    (core module $m (func (export "f") (result i32) i32.const 0))
    (core instance $i (instantiate $m))
    (func (export "run") (result (result)) (canon lift (core func $i "f"))))
  (instance $x (instantiate $inner))
  (export "wasi:cli/run@0.2.0" (instance $x)))
"#,
    )
    .unwrap();
    let out = run_example(&component, "indirect", &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("unsupported"), "{stderr}");
    assert!(!stderr.contains("does not export"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}
