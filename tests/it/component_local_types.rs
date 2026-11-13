//! A component whose exports use records the `wasm2rs-wasi` library does not declare builds
//! with the default options: the glue declares the component's own types and refers to the
//! library for the WASI ones only.

use wasm2rs::{ComponentOptions, transpile_component};
use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
use wit_parser::Resolve;

use crate::common;

const WIT: &str = "package t:t;
interface api {
  record point { x: s32, y: s32 }
  swap: func(p: point) -> point;
}
world w { export api; }";

/// `swap(x, y)` returns the pointer of a `(y, x)` record.
const CORE: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) unreachable)
  (func (export "t:t/api#swap") (param i32 i32) (result i32)
    (i32.store (i32.const 16) (local.get 1))
    (i32.store (i32.const 20) (local.get 0))
    (i32.const 16)))"#;

fn component() -> Vec<u8> {
    let mut resolve = Resolve::default();
    let package = resolve.push_str("test.wit", WIT).unwrap();
    let world = resolve.select_world(&[package], Some("w")).unwrap();
    let mut module = wat::parse_str(CORE).unwrap();
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8, false).unwrap();
    ComponentEncoder::default()
        .module(&module)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap()
}

/// The glue declares the record (the library has no such type) and a point survives the
/// round trip.
#[test]
fn a_record_of_a_non_wasi_interface_round_trips_with_the_default_options() {
    let code = transpile_component(&component(), &ComponentOptions::default()).unwrap();
    assert!(code.contains("pub struct Record0"), "{code}");
    let program = format!(
        "{code}\n\
         fn main() {{\n\
             let mut inst = Instance::new();\n\
             let p = Exports::t_t_api_swap(&mut inst, component::Record0 {{ x: 3, y: 4 }});\n\
             println!(\"{{}} {{}}\", p.x, p.y);\n\
         }}\n"
    );
    let bin = common::build_with_wasi("component_local_record", &program);
    let out = std::process::Command::new(bin).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "4 3");
}
