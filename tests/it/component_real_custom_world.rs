//! A component `cargo component` made from a custom world (an exported interface with a
//! record, strings and integers, no `wasi:cli/run`): the typed `Exports` glue calls it. The
//! file is not in the repository: see the reproduction in `docs/component-toolchain-survey.md`.

use wasm2rs::{ComponentOptions, transpile_component};

use crate::common;

/// Set `WASM2RS_CARGO_COMPONENT_CALC` to the `calc` component (`world calc { export ops; }`
/// with `add`, `greet` and `shift`) and run with `--ignored`.
#[test]
#[ignore = "needs WASM2RS_CARGO_COMPONENT_CALC (a cargo-component component of the calc world)"]
fn a_real_cargo_component_custom_world_is_called_through_the_typed_exports() {
    let path = std::env::var_os("WASM2RS_CARGO_COMPONENT_CALC")
        .expect("WASM2RS_CARGO_COMPONENT_CALC names the component");
    let component = std::fs::read(path).unwrap();
    let code = transpile_component(&component, &ComponentOptions::default()).unwrap();
    // The glue names the record by its type index (the exported name is not carried here), and
    // the index depends on the toolchain; accept the WIT name too, should the glue use it.
    let record = code
        .split("pub struct ")
        .filter_map(|s| s.split_whitespace().next())
        .find(|name| *name == "Point" || name.starts_with("Record"))
        .expect("the glue declares the point record");
    let program = format!(
        "{code}\n\
         fn main() {{\n\
             let host = wasm2rs_wasi::WasiHost::new(Vec::new());\n\
             let mut inst = Instance::new(Glue(host));\n\
             println!(\"{{}}\", Exports::test_calc_ops_add(&mut inst, 40, 2));\n\
             println!(\"{{}}\", Exports::test_calc_ops_greet(&mut inst, \"wasm2rs\".to_string()));\n\
             let p = Exports::test_calc_ops_shift(&mut inst, component::{record} {{ x: 1, y: 2 }}, 10);\n\
             println!(\"{{}} {{}}\", p.x, p.y);\n\
         }}\n"
    );
    let bin = common::build_with_wasi("real_cargo_component_calc", &program);
    let out = std::process::Command::new(bin).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "42\nhello, wasm2rs\n11 2\n"
    );
}
