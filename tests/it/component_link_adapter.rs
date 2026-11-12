//! A component made with a preview1 adapter (`wit-component`'s main + adapter + shim + fixup
//! shape) transpiles, builds and runs: its core modules are linked into one `Instance`, and
//! the host sees only the WIT functions the adapter calls.

use std::process::Command;

use wasm2rs::{ComponentOptions, TranspileOptions, transpile_component, transpile_with_options};
use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
use wit_parser::Resolve;

use crate::common;
use crate::wasip2_run_example::{CLI, IO};

const MAIN: &str = r#"
(module
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "args_count" (func $argc (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 16) "hi")
  (data (i32.const 32) "\10\00\00\00\02\00\00\00")
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) unreachable)
  (func (export "run") (result i32)
    (drop (call $fd_write (i32.const 1) (i32.const 32) (i32.const 1) (i32.const 48)))
    (call $argc)))
"#;

/// The adapter writes through `get-stdout` / `write`, and counts `get-arguments` (whose list
/// the host allocates through the adapter's own `cabi_import_realloc`, not the main's).
const ADAPTER: &str = r#"
(module
  (import "env" "memory" (memory 1))
  (import "wasi:cli/stdout@0.2.0" "get-stdout" (func $stdout (result i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (import "wasi:cli/environment@0.2.0" "get-arguments" (func $args (param i32)))
  (global $brk (mut i32) (i32.const 2048))
  (func (export "cabi_import_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (func (export "fd_write") (param i32 i32 i32 i32) (result i32)
    (call $write (call $stdout)
      (i32.load (local.get 1)) (i32.load offset=4 (local.get 1)) (i32.const 512))
    (i32.store (local.get 3) (i32.load offset=4 (local.get 1)))
    (i32.const 0))
  (func (export "args_count") (result i32)
    (call $args (i32.const 520))
    (i32.load offset=4 (i32.const 520))))
"#;

fn embed(world_wit: &str, world: &str, wat: &str) -> Vec<u8> {
    let mut resolve = Resolve::default();
    resolve.push_str("io.wit", IO).unwrap();
    resolve.push_str("cli.wit", CLI).unwrap();
    let package = resolve.push_str("test.wit", world_wit).unwrap();
    let world = resolve.select_world(&[package], Some(world)).unwrap();
    let mut module = wat::parse_str(wat).unwrap();
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8, false).unwrap();
    module
}

/// The `wasm-tools component new --adapt` shape, assembled by `wit-component` itself.
fn adapter_component() -> Vec<u8> {
    let adapter = embed(
        "package test:adapter;
world adapter { import wasi:cli/stdout@0.2.0; import wasi:cli/environment@0.2.0; }",
        "adapter",
        ADAPTER,
    );
    let main = embed(
        "package test:p;
world w { export run: func() -> u32; }",
        "w",
        MAIN,
    );
    ComponentEncoder::default()
        .module(&main)
        .unwrap()
        .adapter("wasi_snapshot_preview1", &adapter)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap()
}

fn own_host() -> ComponentOptions {
    ComponentOptions {
        external_types: None,
        ..ComponentOptions::default()
    }
}

#[test]
fn an_adapter_shaped_component_transpiles() {
    let code = transpile_component(&adapter_component(), &own_host()).unwrap();
    assert!(code.contains("pub struct Glue"), "{code}");
    // The adapter's preview1 functions are internal now: the host only sees WIT functions.
    assert!(!code.contains("fn wasi_snapshot_preview1"), "{code}");
}

#[test]
fn a_single_module_component_keeps_its_module_text() {
    let core = r#"(module
  (import "t:t/i" "f" (func (param i32) (result i32)))
  (func (export "run") (result i32) (call 0 (i32.const 1))))"#;
    let component = wat::parse_str(
        r#"(component
  (import "t:t/i" (instance $i (export "f" (func (param "n" u32) (result u32)))))
  (alias export $i "f" (func $f))
  (core module $m
    (import "t:t/i" "f" (func (param i32) (result i32)))
    (func (export "run") (result i32) (call 0 (i32.const 1))))
  (core func $l (canon lower (func $f)))
  (core instance $env (export "f" (func $l)))
  (core instance (instantiate $m (with "t:t/i" (instance $env)))))"#,
    )
    .unwrap();
    let options = own_host();
    let code = transpile_component(&component, &options).unwrap();
    let module = transpile_with_options(
        &wat::parse_str(core).unwrap(),
        &TranspileOptions::default()
            .with_named_imports(true)
            .with_named_exports(true)
            .with_reentrant_imports(true),
    )
    .unwrap();
    assert!(code.starts_with(&module), "the module text changed");
}

/// The test host records the calls the adapter makes through the WIT functions.
#[test]
fn an_adapter_shaped_component_runs_against_a_recording_host() {
    let code = transpile_component(&adapter_component(), &own_host()).unwrap();
    let program = format!(
        "{code}\n\
         struct H {{ log: std::rc::Rc<std::cell::RefCell<Vec<String>>> }}\n\
         impl Host for H {{\n\
         fn wasi_cli_environment_get_arguments(&mut self) -> Vec<String> {{\n\
             self.log.borrow_mut().push(\"get-arguments\".into());\n\
             vec![\"a\".into(), \"b\".into()]\n\
         }}\n\
         fn wasi_cli_stdout_get_stdout(&mut self) -> OutputStream {{\n\
             self.log.borrow_mut().push(\"get-stdout\".into());\n\
             OutputStream(7)\n\
         }}\n\
         fn wasi_io_streams_method_output_stream_write(&mut self, this: OutputStream, contents: Vec<u8>) -> Result<(), StreamError> {{\n\
             self.log.borrow_mut().push(format!(\"write:{{}}:{{}}\", this.0, String::from_utf8_lossy(&contents)));\n\
             Ok(())\n\
         }}\n\
         }}\n\
         fn main() {{\n\
             let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));\n\
             let mut inst = Instance::new(Glue(H {{ log: log.clone() }}));\n\
             let n = Exports::run(&mut inst);\n\
             println!(\"{{n}} {{}}\", log.borrow().join(\",\"));\n\
         }}\n"
    );
    let bin = common::build("component_link_adapter_run", &program);
    let out = Command::new(bin).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "2 get-stdout,write:7:hi,get-arguments"
    );
}

/// The hand-written shape #305 used to refuse: the module that lifts `run` (the adapter)
/// imports `env.memory` and `main._start` from the user's module.
#[test]
fn a_lifting_module_that_imports_the_programs_memory_links_and_runs() {
    let component = wat::parse_str(
        r#"(component
            (import "t:t/i" (instance $i (export "f" (func (result u32)))))
            (alias export $i "f" (func $hf))
            (core func $lowered (canon lower (func $hf)))
            (core instance $host (export "f" (func $lowered)))
            (core module $main
                (memory (export "memory") 1)
                (func (export "_start") (i32.store (i32.const 8) (i32.const 41))))
            (core module $adapter
                (import "env" "memory" (memory 1))
                (import "main" "_start" (func $start))
                (import "host" "f" (func $f (result i32)))
                (func (export "run") (result i32)
                    call $start
                    (i32.add (i32.load (i32.const 8)) (call $f))))
            (core instance $m (instantiate $main))
            (alias core export $m "memory" (core memory $mem))
            (alias core export $m "_start" (core func $start))
            (core instance $env (export "memory" (memory $mem)))
            (core instance $mainfns (export "_start" (func $start)))
            (core instance $a (instantiate $adapter
                (with "env" (instance $env))
                (with "host" (instance $host))
                (with "main" (instance $mainfns))))
            (alias core export $a "run" (core func $run))
            (func (export "run") (result u32) (canon lift (core func $run))))"#,
    )
    .unwrap();
    let options = ComponentOptions {
        external_types: None,
        ..ComponentOptions::default()
    };
    let code = transpile_component(&component, &options).unwrap();
    let program = format!(
        "{code}\n\
         struct H;\n\
         impl Host for H {{\n\
             fn t_t_i_f(&mut self) -> u32 {{ 1 }}\n\
         }}\n\
         fn main() {{\n\
             let mut inst = Instance::new(Glue(H));\n\
             println!(\"{{}}\", Exports::run(&mut inst));\n\
         }}\n"
    );
    let bin = common::build("component_link_memory_from_main", &program);
    let out = Command::new(bin).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");
}

/// Two modules that share nothing each lift a function: the unit exports both.
#[test]
fn exports_lifted_from_two_modules_are_both_callable() {
    let component = wat::parse_str(
        r#"(component
            (import "t:t/i" (instance $i (export "f" (func (result u32)))))
            (alias export $i "f" (func $hf))
            (core func $lowered (canon lower (func $hf)))
            (core instance $host (export "f" (func $lowered)))
            (core module $a
                (import "t:t/i" "f" (func $f (result i32)))
                (func (export "fa") (result i32) call $f))
            (core module $b (func (export "fb") (result i32) i32.const 2))
            (core instance $ia (instantiate $a (with "t:t/i" (instance $host))))
            (core instance $ib (instantiate $b))
            (func $fa (result u32) (canon lift (core func $ia "fa")))
            (func $fb (result u32) (canon lift (core func $ib "fb")))
            (export "fa" (func $fa))
            (export "fb" (func $fb)))"#,
    )
    .unwrap();
    let code = transpile_component(&component, &own_host()).unwrap();
    let program = format!(
        "{code}\n\
         struct H;\n\
         impl Host for H {{\n\
             fn t_t_i_f(&mut self) -> u32 {{ 1 }}\n\
         }}\n\
         fn main() {{\n\
             let mut inst = Instance::new(Glue(H));\n\
             println!(\"{{}} {{}}\", Exports::fa(&mut inst), Exports::fb(&mut inst));\n\
         }}\n"
    );
    let bin = common::build("component_link_two_lifts", &program);
    let out = Command::new(bin).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "1 2");
}

/// The glue addresses memory 0 only, so a second module with a memory of its own is refused.
#[test]
fn an_appended_module_with_its_own_memory_is_refused() {
    let component = wat::parse_str(
        r#"(component
            (core module $a (memory (export "memory") 1) (func (export "fa") (result i32) i32.const 1))
            (core module $b (memory (export "memory") 1) (func (export "fb") (result i32) i32.const 2))
            (core instance $ia (instantiate $a))
            (core instance $ib (instantiate $b))
            (func $fa (result u32) (canon lift (core func $ia "fa")))
            (func $fb (result u32) (canon lift (core func $ib "fb")))
            (export "fa" (func $fa))
            (export "fb" (func $fb)))"#,
    )
    .unwrap();
    let err = transpile_component(&component, &own_host()).unwrap_err();
    assert!(err.to_string().contains("own memory"), "{err}");
}
