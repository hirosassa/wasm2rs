//! Helper: emit a small sample `.wasm` file for trying out the CLI.
//! Usage: `cargo run --example emit_sample_wasm -- <out.wasm>`

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sample.wasm".into());
    let wasm = wat::parse_str(
        r#"
        (module
          (func (param i32 i32) (result i32)
            local.get 0
            local.get 1
            i32.add)
          (func (result i32)
            i32.const 7
            i32.const 6
            i32.mul))
        "#,
    )
    .expect("valid wat");
    std::fs::write(&out, wasm).expect("write wasm");
    eprintln!("wrote {out}");
}
