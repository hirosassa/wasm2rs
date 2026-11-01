//! Print the Rust generated for a module with a `call_indirect` dispatch.
//!
//! Run with `cargo run --example show_dispatch`.

fn main() {
    let wat = r#"
    (module
      (type $sig (func (param i32) (result i32)))
      (table 2 funcref)
      (elem (i32.const 0) 1 0)
      (func $inc (param i32) (result i32) (i32.add (local.get 0) (i32.const 1)))
      (func $call (param i32 i32) (result i32) (call_indirect (type $sig) (local.get 1) (local.get 0)))
    )
    "#;

    let wasm = wat::parse_str(wat).expect("valid wat");
    let generated = wasm2rs::transpile(&wasm).expect("transpile ok");
    println!("{}", generated);
}
