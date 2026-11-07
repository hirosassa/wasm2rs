//! `wit_host_trait` reads the types of a component's imports and writes a typed
//! `pub trait Host`; `--wit-host` does the same from the command line.

use std::process::Command;

use crate::common;
use wasm2rs::{TranspileOptions, transpile_with_options, wit_host_trait};

/// A component importing one interface whose functions are `funcs` (WAT `(export ...)`
/// items), after the `types` declarations placed in the component itself.
fn component(types: &str, interface: &str, funcs: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(component {types} (import "{interface}" (instance {funcs})))"#
    ))
    .unwrap()
}

fn host(wasm: &[u8]) -> String {
    wit_host_trait(wasm).unwrap()
}

#[test]
fn an_interface_function_becomes_a_method_named_like_a_named_import() {
    let wasm = component(
        "",
        "wasi:cli/environment@0.2.0",
        r#"(export "get-arguments" (func (result (list string))))"#,
    );
    let rust = host(&wasm);
    assert!(rust.contains("pub trait Host {"), "{rust}");
    assert!(
        rust.contains("fn wasi_cli_environment_get_arguments(&mut self) -> Vec<String>;"),
        "{rust}"
    );
}

#[test]
fn the_method_name_matches_the_named_imports_name_of_a_core_import() {
    let wasm = wat::parse_str(
        r#"(module (import "wasi:cli/environment@0.2.0" "get-arguments" (func (result i32))))"#,
    )
    .unwrap();
    let opts = TranspileOptions {
        named_imports: true,
        ..TranspileOptions::default()
    };
    let imports = transpile_with_options(&wasm, &opts).unwrap();
    let comp = component(
        "",
        "wasi:cli/environment@0.2.0",
        r#"(export "get-arguments" (func (result (list string))))"#,
    );
    let name = "wasi_cli_environment_get_arguments";
    assert!(imports.contains(&format!("fn {name}(")), "{imports}");
    assert!(host(&comp).contains(&format!("fn {name}(")));
}

#[test]
fn every_primitive_maps_to_its_rust_type() {
    let wasm = component(
        "",
        "a:b/c",
        r#"(export "f" (func
            (param "a" u8) (param "b" u16) (param "c" u32) (param "d" u64)
            (param "e" s8) (param "f" s16) (param "g" s32) (param "h" s64)
            (param "i" f32) (param "j" f64) (param "k" bool) (param "l" char)
            (param "m" string)))"#,
    );
    let rust = host(&wasm);
    assert!(
        rust.contains(
            "fn a_b_c_f(&mut self, a: u8, b: u16, c: u32, d: u64, e: i8, f: i16, g: i32, \
             h: i64, i: f32, j: f64, k: bool, l: char, m: String);"
        ),
        "{rust}"
    );
}

#[test]
fn compound_types_nest() {
    let wasm = component(
        "",
        "a:b/c",
        r#"(export "f" (func
            (param "xs" (list (option u8)))
            (param "t" (tuple u8 string bool))
            (result (result (list u32) (error string)))))
          (export "g" (func (result (result))))
          (export "h" (func (result (result u8))))
          (export "i" (func (result (result (error string)))))
          (export "j" (func (param "o" (option (tuple u8)))))"#,
    );
    let rust = host(&wasm);
    for sig in [
        "fn a_b_c_f(&mut self, xs: Vec<Option<u8>>, t: (u8, String, bool)) -> Result<Vec<u32>, String>;",
        "fn a_b_c_g(&mut self) -> Result<(), ()>;",
        "fn a_b_c_h(&mut self) -> Result<u8, ()>;",
        "fn a_b_c_i(&mut self) -> Result<(), String>;",
        "fn a_b_c_j(&mut self, o: Option<(u8,)>);",
    ] {
        assert!(rust.contains(sig), "missing {sig}:\n{rust}");
    }
}

#[test]
fn several_functions_and_interfaces_give_one_method_each() {
    let wasm = wat::parse_str(
        r#"(component
          (import "a:b/one@1.0.0" (instance
            (export "x" (func (result u32)))
            (export "y" (func (param "n" u32)))))
          (import "a:b/two" (instance
            (export "x" (func (result bool))))))"#,
    )
    .unwrap();
    let rust = host(&wasm);
    for sig in [
        "fn a_b_one_x(&mut self) -> u32;",
        "fn a_b_one_y(&mut self, n: u32);",
        "fn a_b_two_x(&mut self) -> bool;",
    ] {
        assert!(rust.contains(sig), "missing {sig}:\n{rust}");
    }
    assert_eq!(rust.matches("    fn ").count(), 3, "{rust}");
}

#[test]
fn a_record_used_by_two_functions_is_emitted_once() {
    let wasm = component(
        "",
        "a:b/c",
        r#"(type $r (record (field "x-pos" u32) (field "type" string)))
           (export "point" (type $p (eq $r)))
           (export "f" (func (param "p" $p) (result $p)))
           (export "g" (func (result (list $p))))"#,
    );
    let rust = host(&wasm);
    assert_eq!(rust.matches("pub struct Point {").count(), 1, "{rust}");
    assert!(rust.contains("pub x_pos: u32,"), "{rust}");
    assert!(rust.contains("pub type_: String,"), "{rust}");
    assert!(
        rust.contains("fn a_b_c_f(&mut self, p: Point) -> Point;"),
        "{rust}"
    );
    assert!(
        rust.contains("fn a_b_c_g(&mut self) -> Vec<Point>;"),
        "{rust}"
    );
}

#[test]
fn a_record_defined_in_the_component_is_reached_through_an_outer_alias() {
    let wasm = component(
        r#"(type $r (record (field "n" u8)))"#,
        "a:b/c",
        r#"(export "thing" (type $t (eq $r)))
           (export "f" (func (param "t" $t)))"#,
    );
    let rust = host(&wasm);
    assert!(rust.contains("pub struct Thing {"), "{rust}");
    assert!(rust.contains("fn a_b_c_f(&mut self, t: Thing);"), "{rust}");
}

#[test]
fn a_record_may_contain_other_records_and_collections() {
    let wasm = component(
        "",
        "a:b/c",
        r#"(type $in (record (field "v" u8)))
           (export "inner" (type $i (eq $in)))
           (type $out (record (field "items" (list $i)) (field "one" (option $i))))
           (export "outer" (type $o (eq $out)))
           (export "f" (func (param "o" $o)))"#,
    );
    let rust = host(&wasm);
    assert!(rust.contains("pub items: Vec<Inner>,"), "{rust}");
    assert!(rust.contains("pub one: Option<Inner>,"), "{rust}");
}

#[test]
fn a_named_alias_of_a_primitive_is_that_primitive() {
    let wasm = component(
        "",
        "a:b/c",
        r#"(type $u u64)
           (export "duration" (type $d (eq $u)))
           (export "f" (func (param "d" $d)))"#,
    );
    assert!(host(&wasm).contains("fn a_b_c_f(&mut self, d: u64);"));
}

#[test]
fn an_interface_without_functions_adds_nothing() {
    let wasm = wat::parse_str(r#"(component (import "a:b/c" (instance)))"#).unwrap();
    assert!(host(&wasm).contains("pub trait Host {"));
    assert!(!host(&wasm).contains("fn "));
}

#[test]
fn unsupported_types_are_errors_naming_the_type() {
    let cases = [
        (
            "variant",
            r#"(type $w (variant (case "a") (case "b" u8)))
               (export "v" (type $v (eq $w)))
               (export "f" (func (param "v" $v)))"#,
        ),
        (
            "enum",
            r#"(type $w (enum "a" "b"))
               (export "v" (type $v (eq $w)))
               (export "f" (func (param "v" $v)))"#,
        ),
        (
            "flags",
            r#"(type $w (flags "a" "b"))
               (export "v" (type $v (eq $w)))
               (export "f" (func (param "v" $v)))"#,
        ),
        (
            "own",
            r#"(export "res" (type $r (sub resource)))
               (export "f" (func (param "v" (own $r))))"#,
        ),
        (
            "borrow",
            r#"(export "res" (type $r (sub resource)))
               (export "f" (func (param "v" (borrow $r))))"#,
        ),
        ("future", r#"(export "f" (func (param "v" (future u8))))"#),
        ("stream", r#"(export "f" (func (result (stream u8))))"#),
        (
            "error-context",
            r#"(export "f" (func (param "v" error-context)))"#,
        ),
        (
            "variant",
            // Nested inside a supported type.
            r#"(type $w (variant (case "a")))
               (export "v" (type $v (eq $w)))
               (export "f" (func (result (list (option $v)))))"#,
        ),
    ];
    for (what, funcs) in cases {
        let wasm = component("", "a:b/c", funcs);
        let err = wit_host_trait(&wasm).unwrap_err().to_string();
        assert!(err.contains("unsupported"), "{what}: {err}");
        assert!(err.contains(what), "{what}: {err}");
        assert!(err.contains("a:b/c"), "{what}: {err}");
    }
}

#[test]
fn an_unused_unsupported_type_is_not_an_error() {
    let wasm = component(
        "",
        "a:b/c",
        r#"(type $w (variant (case "a")))
               (export "v" (type $v (eq $w)))
           (export "kind" (type (eq $v)))
           (export "f" (func (result u8)))"#,
    );
    assert!(host(&wasm).contains("fn a_b_c_f(&mut self) -> u8;"));
}

#[test]
fn a_core_module_is_an_error() {
    let wasm = wat::parse_str("(module (func (export \"f\")))").unwrap();
    let err = wit_host_trait(&wasm).unwrap_err().to_string();
    assert!(err.contains("component"), "{err}");
}

#[test]
fn malformed_input_is_an_error() {
    assert!(wit_host_trait(b"not wasm").is_err());
}

#[test]
fn a_component_without_imports_gives_an_empty_trait() {
    let wasm = wat::parse_str("(component (core module))").unwrap();
    let rust = host(&wasm);
    assert!(rust.contains("pub trait Host {"), "{rust}");
    assert!(!rust.contains("fn "), "{rust}");
}

#[test]
fn imports_of_nested_components_are_not_the_hosts() {
    let wasm = wat::parse_str(
        r#"(component
          (component (import "a:b/inner" (instance (export "f" (func)))))
          (import "a:b/outer" (instance (export "g" (func)))))"#,
    )
    .unwrap();
    let rust = host(&wasm);
    assert!(rust.contains("fn a_b_outer_g("), "{rust}");
    assert!(!rust.contains("inner"), "{rust}");
}

#[test]
fn an_import_that_is_not_an_instance_is_an_error() {
    let wasm = wat::parse_str(r#"(component (import "f" (func)))"#).unwrap();
    let err = wit_host_trait(&wasm).unwrap_err().to_string();
    assert!(
        err.contains("unsupported") && err.contains("\"f\""),
        "{err}"
    );
}

#[test]
fn the_generated_trait_compiles_and_can_be_implemented() {
    let wasm = component(
        "",
        "a:b/c",
        r#"(type $r (record (field "x" u32) (field "type" string)))
           (export "point" (type $p (eq $r)))
           (export "origin" (func (result $p)))
           (export "shift" (func (param "p" $p) (param "by" (option s32)) (result (result $p (error string)))))
           (export "names" (func (result (list string))))
           (export "pair" (func (param "t" (tuple u8 char f64))))
           (export "fail" (func (result (result (error string)))))"#,
    );
    let rust = host(&wasm);
    let program = format!(
        r#"{rust}
struct H;
impl Host for H {{
    fn a_b_c_origin(&mut self) -> Point {{ Point {{ x: 1, type_: "o".into() }} }}
    fn a_b_c_shift(&mut self, p: Point, by: Option<i32>) -> Result<Point, String> {{
        match by {{ Some(n) => Ok(Point {{ x: p.x + n as u32, ..p }}), None => Err("none".into()) }}
    }}
    fn a_b_c_names(&mut self) -> Vec<String> {{ vec!["a".into()] }}
    fn a_b_c_pair(&mut self, _t: (u8, char, f64)) {{}}
    fn a_b_c_fail(&mut self) -> Result<(), String> {{ Err("x".into()) }}
}}
fn main() {{
    let mut h = H;
    let p = h.a_b_c_origin();
    assert_eq!(h.a_b_c_shift(p.clone(), Some(2)).unwrap().x, 3);
    assert!(h.a_b_c_shift(p, None).is_err());
    assert_eq!(h.a_b_c_names(), vec!["a".to_string()]);
    h.a_b_c_pair((1, 'c', 2.0));
    assert!(h.a_b_c_fail().is_err());
}}
"#
    );
    let bin = common::build("wit_host_trait", &program);
    assert!(Command::new(bin).status().unwrap().success());
}

#[test]
fn the_cli_prints_the_trait_or_writes_it_to_a_file() {
    let dir = std::env::temp_dir().join(format!("wasm2rs_wit_host_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("c.wasm");
    let wasm = component("", "a:b/c", r#"(export "f" (func (result u8)))"#);
    std::fs::write(&input, &wasm).unwrap();

    let printed = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
        .arg("--wit-host")
        .arg(&input)
        .output()
        .unwrap();
    assert!(printed.status.success());
    assert_eq!(String::from_utf8_lossy(&printed.stdout), host(&wasm));

    let out = dir.join("host.rs");
    let written = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
        .arg("--wit-host")
        .arg(&input)
        .arg(&out)
        .output()
        .unwrap();
    assert!(written.status.success());
    assert_eq!(std::fs::read_to_string(&out).unwrap(), host(&wasm));
}

#[test]
fn the_cli_fails_on_a_core_module_and_unsupported_types() {
    let dir = std::env::temp_dir().join(format!("wasm2rs_wit_host_err_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let run = |wasm: &[u8]| {
        let input = dir.join("in.wasm");
        std::fs::write(&input, wasm).unwrap();
        Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
            .arg("--wit-host")
            .arg(&input)
            .output()
            .unwrap()
    };
    let module = run(&wat::parse_str("(module)").unwrap());
    assert!(!module.status.success());
    let variant = run(&component(
        "",
        "a:b/c",
        r#"(type $w (variant (case "a")))
               (export "v" (type $v (eq $w)))
           (export "f" (func (param "v" $v)))"#,
    ));
    assert!(!variant.status.success());
    assert!(String::from_utf8_lossy(&variant.stderr).contains("variant"));
}

/// `--wit-host` generates no code from the module, so it refuses extra arguments and the
/// flags of the other modes instead of silently ignoring them.
#[test]
fn the_cli_rejects_other_flags_and_extra_arguments() {
    let dir = std::env::temp_dir().join(format!("wasm2rs_wit_host_flags_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("c.wasm");
    std::fs::write(&input, component("", "a:b/c", r#"(export "f" (func))"#)).unwrap();
    let out = dir.join("out.rs");
    for flag in [
        "--unsafe-memory",
        "--named-exports",
        "--named-imports",
        "--import-memory",
        "--reentrant-imports",
        "--split-dispatch=4",
        "--jobs=2",
        "--entry-exports=a",
        "--dump",
        "--extract-core",
    ] {
        std::fs::remove_file(&out).ok();
        let output = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
            .arg("--wit-host")
            .arg(flag)
            .arg(&input)
            .arg(&out)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let name = flag.split('=').next().unwrap();
        assert!(!output.status.success(), "{flag} was accepted");
        assert!(
            stderr.contains("--wit-host") && stderr.contains(name),
            "{flag}: {stderr}"
        );
        assert!(!out.exists(), "{flag} still wrote output");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
        .arg("--wit-host")
        .arg(&input)
        .arg(&out)
        .arg("10")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!out.exists());
}

/// A component of `levels` types that each refer to the previous one twice (a tree of
/// 2^levels leaves when expanded, a handful of bytes shared), then `copies` lists of the
/// last one; `used` also gives the interface a function returning it.
fn doubling_component(levels: usize, copies: usize, used: bool) -> Vec<u8> {
    let mut s = String::from("(component (type (instance\n (type (tuple u8 u8))\n");
    for i in 1..levels {
        s.push_str(&format!(" (type (tuple {} {}))\n", i - 1, i - 1));
    }
    for _ in 0..copies {
        s.push_str(&format!(" (type (list {}))\n", levels - 1));
    }
    if used {
        s.push_str(&format!(
            " (type (func (result {})))\n (export \"f\" (func (type {})))\n",
            levels - 1,
            levels + copies
        ));
    }
    s.push_str(")) (import \"a:b/c\" (instance (type 0))))\n");
    wat::parse_str(s).unwrap()
}

#[test]
fn types_that_share_a_type_twice_do_not_blow_up_when_unused() {
    let wasm = doubling_component(18, 3000, false);
    let start = std::time::Instant::now();
    let rust = host(&wasm);
    assert!(!rust.contains("fn "), "{rust}");
    assert!(start.elapsed().as_secs() < 5, "{:?}", start.elapsed());
}

#[test]
fn a_shared_type_that_renders_exponentially_large_is_a_clean_error() {
    let wasm = doubling_component(18, 3000, true);
    let start = std::time::Instant::now();
    let err = wit_host_trait(&wasm).unwrap_err().to_string();
    assert!(
        err.contains("unsupported") && err.contains("too large"),
        "{err}"
    );
    assert!(start.elapsed().as_secs() < 5, "{:?}", start.elapsed());
}

#[test]
fn a_moderately_shared_type_still_renders() {
    let wasm = doubling_component(3, 1, true);
    let rust = host(&wasm);
    assert!(
        rust.contains("(((u8, u8), (u8, u8)), ((u8, u8), (u8, u8)))"),
        "{rust}"
    );
}

#[test]
fn names_cannot_inject_rust_into_the_generated_text() {
    let wasm = wat::parse_str(
        "(component
          (type (instance (export \"f\" (func))))
          (import \"a:b/c\\n} fn pwned() {}\\n//\" (instance (type 0))))",
    )
    .unwrap();
    match wit_host_trait(&wasm) {
        Err(_) => {}
        Ok(rust) => {
            assert!(
                !rust.lines().any(|l| l.trim_start().starts_with("fn pwned")),
                "{rust}"
            );
            let program = format!("{rust}\nfn main() {{}}\n");
            common::build("wit_host_inject", &program);
        }
    }
}

#[test]
fn an_invalid_component_is_rejected() {
    let wasm = wat::parse_str("(component (type (instance (export \"f\" (func (result 7))))))");
    // The text format may refuse it already; a binary that passes is what matters.
    if let Ok(wasm) = wasm {
        assert!(wit_host_trait(&wasm).is_err());
    }
}
