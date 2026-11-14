//! A component MoonBit made from the custom `calc` world (#344). MoonBit's `String` is UTF-16, so
//! its exports lift strings as `string-encoding=utf16`: the glue transcodes at the boundary. The
//! file is not in the repository: see the reproduction in `docs/component-toolchain-survey.md`.

use wasm2rs::{ComponentOptions, transpile_component};

use crate::common;

/// Set `WASM2RS_MOONBIT_CALC` to the `calc` component (`world calc { export ops; }` with `add`,
/// `greet` and `shift`) and run with `--ignored`.
#[test]
#[ignore = "needs WASM2RS_MOONBIT_CALC (a MoonBit component of the calc world)"]
fn a_real_moonbit_component_passes_utf16_strings_through_the_typed_exports() {
    let path =
        std::env::var_os("WASM2RS_MOONBIT_CALC").expect("WASM2RS_MOONBIT_CALC names the component");
    let component = std::fs::read(path).unwrap();
    let code = transpile_component(&component, &ComponentOptions::default()).unwrap();
    let record = code
        .split("pub struct ")
        .filter_map(|s| s.split_whitespace().next())
        .find(|name| *name == "Point" || name.starts_with("Record"))
        .expect("the glue declares the point record");
    // The component imports nothing, so its `Instance` takes no host.
    let program = format!(
        "{code}\n\
         fn main() {{\n\
             let mut inst = Instance::new();\n\
             println!(\"{{}}\", Exports::test_calc_ops_add(&mut inst, 40, 2));\n\
             println!(\"{{}}\", Exports::test_calc_ops_greet(&mut inst, \"wasm2rs\".to_string()));\n\
             println!(\"{{}}\", Exports::test_calc_ops_greet(&mut inst, \"日本語😀\".to_string()));\n\
             println!(\"{{}}\", Exports::test_calc_ops_greet(&mut inst, String::new()));\n\
             let p = Exports::test_calc_ops_shift(&mut inst, component::{record} {{ x: 1, y: 2 }}, 10);\n\
             println!(\"{{}} {{}}\", p.x, p.y);\n\
         }}\n"
    );
    let bin = common::build("real_moonbit_calc", &program);
    let out = std::process::Command::new(bin).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "42\nhello, wasm2rs\nhello, 日本語😀\nhello, \n11 2\n"
    );
}
