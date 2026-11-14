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
    let opts = TranspileOptions::default().with_named_imports(true);
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
            "error-context",
            r#"(export "f" (func (param "v" error-context)))"#,
        ),
        (
            "error-context",
            // Nested inside a supported type.
            r#"(export "f" (func (result (list (option error-context)))))"#,
        ),
        (
            "error-context",
            // An unsupported payload inside a variant.
            r#"(type $st error-context)
               (type $w (variant (case "a" $st)))
               (export "v" (type $v (eq $w)))
               (export "f" (func (param "v" $v)))"#,
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
        r#"(type $st error-context)
           (type $w (variant (case "a" $st)))
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
        r#"(export "f" (func (param "v" error-context)))"#,
    ));
    assert!(!variant.status.success());
    assert!(String::from_utf8_lossy(&variant.stderr).contains("error-context"));
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

const POLL: &str = r#"(export "pollable" (type $p (sub resource)))
    (export "[constructor]pollable" (func (param "n" u32) (result (own $p))))
    (export "[method]pollable.block" (func (param "self" (borrow $p))))
    (export "[method]pollable.ready" (func (param "self" (borrow $p)) (result bool)))
    (export "[static]pollable.never" (func (result (own $p))))"#;

#[test]
fn a_resource_becomes_a_copy_newtype_with_method_static_constructor_and_drop() {
    let wasm = component("", "wasi:io/poll@0.2.0", POLL);
    let rust = host(&wasm);
    for want in [
        "#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub struct Pollable(pub u32);",
        "fn wasi_io_poll_constructor_pollable(&mut self, n: u32) -> Pollable;",
        "fn wasi_io_poll_method_pollable_block(&mut self, self_: Pollable);",
        "fn wasi_io_poll_method_pollable_ready(&mut self, self_: Pollable) -> bool;",
        "fn wasi_io_poll_static_pollable_never(&mut self) -> Pollable;",
        "fn wasi_io_poll_resource_drop_pollable(&mut self, handle: Pollable);",
    ] {
        assert!(rust.contains(want), "missing {want}:\n{rust}");
    }
    assert_eq!(rust.matches("pub struct Pollable").count(), 1, "{rust}");
    assert!(
        rust.find("pub struct Pollable") < rust.find("pub trait Host"),
        "{rust}"
    );
}

#[test]
fn resource_method_names_match_the_named_imports_of_a_core_module() {
    let core = wat::parse_str(
        r#"(module
          (import "wasi:io/poll@0.2.0" "[method]pollable.block" (func (param i32)))
          (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func (param i32)))
          (import "wasi:io/poll@0.2.0" "[constructor]pollable" (func (param i32) (result i32)))
          (import "wasi:io/poll@0.2.0" "[static]pollable.never" (func (result i32))))"#,
    )
    .unwrap();
    let opts = TranspileOptions::default().with_named_imports(true);
    let imports = transpile_with_options(&core, &opts).unwrap();
    let rust = host(&component("", "wasi:io/poll@0.2.0", POLL));
    for name in [
        "wasi_io_poll_method_pollable_block",
        "wasi_io_poll_resource_drop_pollable",
        "wasi_io_poll_constructor_pollable",
        "wasi_io_poll_static_pollable_never",
    ] {
        assert!(
            imports.contains(&format!("fn {name}(")),
            "{name}\n{imports}"
        );
        assert!(rust.contains(&format!("fn {name}(")), "{name}\n{rust}");
    }
}

/// `streams` uses `poll`'s pollable through an outer alias of an instance export.
fn two_interfaces() -> Vec<u8> {
    wat::parse_str(
        r#"(component
          (type $poll (instance
            (export "pollable" (type $p (sub resource)))
            (export "[method]pollable.block" (func (param "self" (borrow $p))))))
          (import "wasi:io/poll@0.2.0" (instance $poll_i (type $poll)))
          (alias export $poll_i "pollable" (type $pollable))
          (type $streams (instance
            (alias outer 1 $pollable (type $p))
            (export "pollable" (type $q (eq $p)))
            (export "stream" (type $s (sub resource)))
            (export "[method]stream.subscribe" (func (param "self" (borrow $s)) (result (own $q))))))
          (import "wasi:io/streams@0.2.0" (instance (type $streams))))"#,
    )
    .unwrap()
}

#[test]
fn a_resource_used_from_two_interfaces_is_one_newtype() {
    let rust = host(&two_interfaces());
    assert_eq!(rust.matches("pub struct Pollable").count(), 1, "{rust}");
    assert!(
        rust.contains(
            "fn wasi_io_streams_method_stream_subscribe(&mut self, self_: Stream) -> Pollable;"
        ),
        "{rust}"
    );
    assert!(
        rust.contains("fn wasi_io_poll_method_pollable_block(&mut self, self_: Pollable);"),
        "{rust}"
    );
    // Only the defining interface drops it.
    assert_eq!(rust.matches("resource_drop_pollable").count(), 1, "{rust}");
    assert!(
        rust.contains("fn wasi_io_poll_resource_drop_pollable("),
        "{rust}"
    );
}

#[test]
fn two_distinct_resources_with_one_name_get_distinct_newtypes() {
    let wasm = wat::parse_str(
        r#"(component
          (import "a:b/one" (instance (export "r" (type $r (sub resource)))
            (export "f" (func (param "x" (own $r))))))
          (import "a:b/two" (instance (export "r" (type $r (sub resource)))
            (export "g" (func (param "x" (own $r)))))))"#,
    )
    .unwrap();
    let rust = host(&wasm);
    assert!(rust.contains("pub struct R(pub u32);"), "{rust}");
    assert!(rust.contains("pub struct R2(pub u32);"), "{rust}");
    assert!(rust.contains("fn a_b_one_f(&mut self, x: R);"), "{rust}");
    assert!(rust.contains("fn a_b_two_g(&mut self, x: R2);"), "{rust}");
}

#[test]
fn the_decoded_form_keeps_own_and_borrow_apart() {
    use wasm2rs::WitType;
    let funcs = wasm2rs::wit_functions(&component("", "a:b/c", POLL)).unwrap();
    let block = funcs
        .iter()
        .find(|f| f.name == "[method]pollable.block")
        .unwrap();
    assert!(matches!(block.params[0].1, WitType::Borrow(_)));
    let ctor = funcs
        .iter()
        .find(|f| f.name == "[constructor]pollable")
        .unwrap();
    assert!(matches!(ctor.result, Some(WitType::Own(_))));
    let drop = funcs
        .iter()
        .find(|f| f.name == "[resource-drop]pollable")
        .unwrap();
    assert!(matches!(drop.params[0].1, WitType::Own(_)));
}

#[test]
fn an_own_inside_a_list_is_a_list_of_handles() {
    let wasm = component(
        "",
        "a:b/c",
        r#"(export "r" (type $r (sub resource)))
           (export "f" (func (param "x" (list (own $r)))))"#,
    );
    assert!(host(&wasm).contains("Vec<R>"));
}

#[test]
fn the_resource_trait_compiles_and_can_be_implemented() {
    let rust = host(&two_interfaces());
    let program = format!(
        r#"{rust}
struct H(Vec<u32>);
impl Host for H {{
    fn wasi_io_poll_method_pollable_block(&mut self, self_: Pollable) {{ self.0.push(self_.0); }}
    fn wasi_io_poll_resource_drop_pollable(&mut self, handle: Pollable) {{ self.0.push(handle.0 + 100); }}
    fn wasi_io_streams_resource_drop_stream(&mut self, _handle: Stream) {{}}
    fn wasi_io_streams_method_stream_subscribe(&mut self, self_: Stream) -> Pollable {{ Pollable(self_.0 + 1) }}
}}
fn main() {{
    let mut h = H(Vec::new());
    let p = h.wasi_io_streams_method_stream_subscribe(Stream(6));
    h.wasi_io_poll_method_pollable_block(p);
    h.wasi_io_poll_resource_drop_pollable(p);
    assert_eq!(h.0, vec![7, 107]);
    assert_eq!(p, p.clone());
}}
"#
    );
    let bin = common::build("wit_host_resources", &program);
    assert!(Command::new(bin).status().unwrap().success());
}

#[test]
fn every_import_of_one_instance_type_gets_fresh_resources() {
    let wasm = wat::parse_str(
        r#"(component
          (type $i (instance
            (export "r" (type (sub resource)))
            (type (own 0))
            (export "make" (func (result 1)))))
          (import "a:b/x" (instance (type $i)))
          (import "a:b/y" (instance (type $i))))"#,
    )
    .unwrap();
    let rust = host(&wasm);
    for want in [
        "pub struct R(pub u32);",
        "pub struct R2(pub u32);",
        "fn a_b_x_make(&mut self) -> R;",
        "fn a_b_y_make(&mut self) -> R2;",
        "fn a_b_x_resource_drop_r(&mut self, handle: R);",
        "fn a_b_y_resource_drop_r(&mut self, handle: R2);",
    ] {
        assert!(rust.contains(want), "missing {want}:\n{rust}");
    }
}

#[test]
fn an_alias_of_the_first_imports_resource_stays_that_resource() {
    let wasm = wat::parse_str(
        r#"(component
          (type $t (instance (export "p" (type (sub resource)))))
          (import "a:b/x" (instance $x (type $t)))
          (import "a:b/y" (instance $y (type $t)))
          (alias export $x "p" (type $xp))
          (type $u (instance
            (alias outer 1 $xp (type $q))
            (export "q" (type $q2 (eq $q)))
            (export "f" (func (param "a" (own $q2))))))
          (import "a:b/z" (instance (type $u))))"#,
    )
    .unwrap();
    let rust = host(&wasm);
    assert!(rust.contains("fn a_b_z_f(&mut self, a: P);"), "{rust}");
    assert!(
        rust.contains("fn a_b_x_resource_drop_p(&mut self, handle: P);"),
        "{rust}"
    );
    assert!(
        rust.contains("fn a_b_y_resource_drop_p(&mut self, handle: P2);"),
        "{rust}"
    );
}

// ---- variant, enum and flags ----

fn variant_host(types_and_funcs: &str) -> String {
    host(&component("", "a:b/c", types_and_funcs))
}

#[test]
fn a_variant_becomes_a_rust_enum_with_payload_cases() {
    let rust = variant_host(
        r#"(type $w (variant (case "none-yet") (case "bytes" (list u8)) (case "code" u32)))
           (export "outcome" (type $v (eq $w)))
           (export "f" (func (param "v" $v) (result $v)))"#,
    );
    assert!(
        rust.contains(
            "#[derive(Debug, Clone, PartialEq)]\npub enum Outcome {\n    NoneYet,\n    Bytes(Vec<u8>),\n    Code(u32),\n}"
        ),
        "{rust}"
    );
    assert!(
        rust.contains("fn a_b_c_f(&mut self, v: Outcome) -> Outcome;"),
        "{rust}"
    );
}

#[test]
fn a_variant_with_compound_payloads_nests_records_options_and_handles() {
    let rust = host(
        &wat::parse_str(
            r#"(component
          (type (instance
            (export "res" (type $res (sub resource)))
            (type $rec (record (field "n" u8)))
            (export "info" (type $info (eq $rec)))
            (type $w (variant
              (case "r" $info)
              (case "o" (option (list string)))
              (case "h" (own $res))
              (case "b" (borrow $res))
              (case "t" (tuple u8 bool))
              (case "unit")))
            (export "sum" (type $sum (eq $w)))
            (type $f (func (param "s" $sum)))
            (export "f" (func (type $f)))))
          (import "a:b/c" (instance (type 0))))"#,
        )
        .unwrap(),
    );
    for want in [
        "pub struct Info {",
        "    R(Info),",
        "    O(Option<Vec<String>>),",
        "    H(Res),",
        "    B(Res),",
        "    T((u8, bool)),",
        "    Unit,",
        "fn a_b_c_f(&mut self, s: Sum);",
    ] {
        assert!(rust.contains(want), "missing {want}:\n{rust}");
    }
    // The record the variant contains is declared before it (deterministic order).
    assert!(rust.find("pub struct Info").unwrap() < rust.find("pub enum Sum").unwrap());
}

#[test]
fn an_enum_is_a_fieldless_copy_enum() {
    let rust = variant_host(
        r#"(type $w (enum "not-found" "denied" "io-error"))
           (export "error-code" (type $e (eq $w)))
           (export "f" (func (result (option $e))))"#,
    );
    assert!(
        rust.contains(
            "#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub enum ErrorCode {\n    NotFound,\n    Denied,\n    IoError,\n}"
        ),
        "{rust}"
    );
    assert!(
        rust.contains("fn a_b_c_f(&mut self) -> Option<ErrorCode>;"),
        "{rust}"
    );
}

fn flag_names(n: usize) -> String {
    (0..n).map(|i| format!("\"f{i}\" ")).collect()
}

#[test]
fn flags_use_the_canonical_abi_integer_width() {
    for (count, int) in [
        (3, "u8"),
        (8, "u8"),
        (9, "u16"),
        (16, "u16"),
        (17, "u32"),
        (32, "u32"),
    ] {
        let rust = variant_host(&format!(
            r#"(type $w (flags {}))
               (export "perms" (type $p (eq $w)))
               (export "f" (func (param "p" $p)))"#,
            flag_names(count)
        ));
        assert!(
            rust.contains(&format!(
                "#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]\npub struct Perms(pub {int});"
            )),
            "{count}: {rust}"
        );
        assert!(
            rust.contains(&format!(
                "    pub const F{}: Self = Self(1 << {});",
                count - 1,
                count - 1
            )),
            "{count}: {rust}"
        );
    }
}

#[test]
fn more_than_32_flags_are_unsupported() {
    let wasm = wat::parse_str(format!(
        r#"(component (import "a:b/c" (instance
            (type $w (flags {}))
            (export "perms" (type $p (eq $w)))
            (export "f" (func (param "p" $p))))))"#,
        flag_names(33)
    ));
    // Either the validator or this reader refuses; never a silently wrong trait.
    if let Ok(wasm) = wasm {
        let err = wit_host_trait(&wasm).unwrap_err().to_string();
        assert!(err.contains("flags") || err.contains("32"), "{err}");
    }
}

#[test]
fn a_variant_used_by_two_functions_is_emitted_once() {
    let rust = variant_host(
        r#"(type $w (variant (case "a") (case "b" u8)))
           (export "v" (type $v (eq $w)))
           (export "f" (func (param "x" $v)))
           (export "g" (func (result (list $v))))"#,
    );
    assert_eq!(rust.matches("pub enum V {").count(), 1, "{rust}");
}

#[test]
fn different_types_with_one_name_get_distinct_rust_names() {
    let wasm = wat::parse_str(
        r#"(component
          (import "a:b/one" (instance
            (type $w (variant (case "a")))
            (export "kind" (type $k (eq $w)))
            (export "f" (func (param "k" $k)))))
          (import "a:b/two" (instance
            (type $w (variant (case "b" u8)))
            (export "kind" (type $k (eq $w)))
            (export "f" (func (param "k" $k)))))
          (import "a:b/three" (instance
            (type $w (enum "c"))
            (export "kind" (type $k (eq $w)))
            (export "f" (func (param "k" $k))))))"#,
    )
    .unwrap();
    let rust = host(&wasm);
    for want in [
        "pub enum Kind {\n    A,\n}",
        "pub enum Kind2 {\n    B(u8),\n}",
        "pub enum Kind3 {\n    C,\n}",
        "fn a_b_one_f(&mut self, k: Kind);",
        "fn a_b_two_f(&mut self, k: Kind2);",
        "fn a_b_three_f(&mut self, k: Kind3);",
    ] {
        assert!(rust.contains(want), "missing {want}:\n{rust}");
    }
}

#[test]
fn case_and_flag_names_are_escaped_and_deduplicated() {
    let rust = variant_host(
        r#"(type $w (variant (case "self") (case "a-b") (case "a1" u8) (case "type")))
           (export "option" (type $v (eq $w)))
           (type $x (flags "type" "a-b" "a-b2" "x1"))
           (export "host" (type $h (eq $x)))
           (export "f" (func (param "v" $v) (param "h" $h)))"#,
    );
    for want in [
        "pub enum Option_ {",
        "    Self_,",
        "    AB,",
        "    A1(u8),",
        "    Type,",
        "pub struct Host_(pub u8);",
        "pub const TYPE: Self",
        "pub const A_B: Self",
        "pub const A_B2: Self",
        "pub const X1: Self",
    ] {
        assert!(rust.contains(want), "missing {want}:\n{rust}");
    }
}

#[test]
fn the_decoded_form_has_variant_enum_and_flags() {
    use wasm2rs::{WitType, wit_functions};
    let wasm = component(
        "",
        "a:b/c",
        r#"(type $w (variant (case "a") (case "b" u8)))
           (export "v" (type $v (eq $w)))
           (type $e (enum "x" "y"))
           (export "e" (type $e2 (eq $e)))
           (type $fl (flags "r" "w"))
           (export "fl" (type $fl2 (eq $fl)))
           (export "f" (func (param "v" $v) (param "e" $e2) (param "fl" $fl2)))"#,
    );
    let funcs = wit_functions(&wasm).unwrap();
    let params = &funcs[0].params;
    assert!(
        matches!(&params[0].1, WitType::Variant(v) if v.name == "v" && v.cases.len() == 2 && v.cases[0].1.is_none())
    );
    assert!(matches!(&params[1].1, WitType::Enum(e) if e.name == "e" && e.cases == ["x", "y"]));
    assert!(matches!(&params[2].1, WitType::Flags(f) if f.name == "fl" && f.flags == ["r", "w"]));
}

#[test]
fn the_variant_enum_and_flags_types_compile_and_can_be_used() {
    let rust = variant_host(
        r#"(type $s (variant (case "closed") (case "last-operation-failed" u32) (case "text" string)))
           (export "stream-error" (type $se (eq $s)))
           (type $e (enum "not-found" "denied"))
           (export "error-code" (type $ec (eq $e)))
           (type $fl (flags "read" "write" "mutate-directory"))
           (export "descriptor-flags" (type $df (eq $fl)))
           (export "step" (func (param "s" $se) (param "f" $df) (result (result $ec (error $se)))))"#,
    );
    let program = format!(
        r#"{rust}
struct H;
impl Host for H {{
    fn a_b_c_step(&mut self, s: StreamError, f: DescriptorFlags) -> Result<ErrorCode, StreamError> {{
        if f.contains(DescriptorFlags::WRITE) && !f.contains(DescriptorFlags::MUTATE_DIRECTORY) {{
            return Ok(ErrorCode::Denied);
        }}
        match s {{
            StreamError::Closed => Ok(ErrorCode::NotFound),
            StreamError::LastOperationFailed(n) => Err(StreamError::LastOperationFailed(n + 1)),
            StreamError::Text(t) => Err(StreamError::Text(t + "!")),
        }}
    }}
}}
fn main() {{
    let mut h = H;
    let rw = DescriptorFlags::READ.union(DescriptorFlags::WRITE);
    assert_eq!(rw, DescriptorFlags(0b011));
    assert_eq!(DescriptorFlags::default(), DescriptorFlags(0));
    assert_eq!(h.a_b_c_step(StreamError::Closed, rw), Ok(ErrorCode::Denied));
    assert_eq!(h.a_b_c_step(StreamError::Closed, DescriptorFlags::READ), Ok(ErrorCode::NotFound));
    assert_eq!(
        h.a_b_c_step(StreamError::LastOperationFailed(4), DescriptorFlags::READ),
        Err(StreamError::LastOperationFailed(5))
    );
    assert_eq!(
        h.a_b_c_step(StreamError::Text("x".into()), DescriptorFlags(0)),
        Err(StreamError::Text("x!".into()))
    );
    let e = ErrorCode::Denied;
    let copy = e;
    assert_eq!(e, copy);
}}
"#
    );
    let bin = common::build("wit_host_variants", &program);
    assert!(Command::new(bin).status().unwrap().success());
}

/// Like [`doubling_component`] with variants: each level has two cases of the previous one.
fn doubling_variants(levels: usize, copies: usize) -> Vec<u8> {
    // Variants must be named: level i is type 2i (the definition) and 2i+1 (its export).
    let mut s = String::from(
        "(component (type (instance\n (type (variant (case \"a\" u8) (case \"b\")))\n (export \"v0\" (type (eq 0)))\n",
    );
    for i in 1..levels {
        let prev = 2 * (i - 1) + 1;
        s.push_str(&format!(
            " (type (variant (case \"a\" {prev}) (case \"b\" {prev})))\n (export \"v{i}\" (type (eq {})))\n",
            2 * i
        ));
    }
    let last = 2 * (levels - 1) + 1;
    for _ in 0..copies {
        s.push_str(&format!(" (type (list {last}))\n"));
    }
    s.push_str(&format!(
        " (type (func (result {last})))\n (export \"f\" (func (type {})))\n",
        2 * levels + copies
    ));
    s.push_str(")) (import \"a:b/c\" (instance (type 0))))\n");
    wat::parse_str(s).unwrap()
}

#[test]
fn a_shared_chain_of_variants_is_bounded_by_the_node_budget() {
    let wasm = doubling_variants(17, 0);
    let start = std::time::Instant::now();
    let err = wit_host_trait(&wasm).unwrap_err().to_string();
    assert!(
        err.contains("unsupported") && err.contains("too large"),
        "{err}"
    );
    assert!(start.elapsed().as_secs() < 5, "{:?}", start.elapsed());
}

#[test]
fn a_moderately_shared_chain_of_variants_still_renders() {
    let rust = host(&doubling_variants(3, 0));
    assert!(rust.contains("pub enum V2 {"), "{rust}");
}

#[test]
fn type_names_do_not_shadow_the_prelude() {
    let rust = variant_host(
        r#"(type $r (record (field "n" u8)))
           (export "some" (type $some (eq $r)))
           (type $e (enum "x"))
           (export "none" (type $none (eq $e)))
           (type $fl (flags "x"))
           (export "ok" (type $ok (eq $fl)))
           (type $v (variant (case "x")))
           (export "err" (type $err (eq $v)))
           (export "default" (type $default (sub resource)))
           (export "f" (func (param "a" $some) (param "b" $none) (param "c" $ok) (param "d" $err)
               (param "e" (own $default)) (result (option u8))))
           (export "g" (func (result (result u8 (error u8)))))"#,
    );
    for want in [
        "pub struct Some_ {",
        "pub enum None_ {",
        "pub struct Ok_(pub u8);",
        "pub enum Err_ {",
        "pub struct Default_(pub u32);",
    ] {
        assert!(rust.contains(want), "missing {want}:\n{rust}");
    }
    let program = format!(
        r#"{rust}
struct H;
impl Host for H {{
    fn a_b_c_f(&mut self, _a: Some_, _b: None_, _c: Ok_, _d: Err_, _e: Default_) -> Option<u8> {{
        Some(Default::default())
    }}
    fn a_b_c_resource_drop_default(&mut self, _handle: Default_) {{}}
    fn a_b_c_g(&mut self) -> Result<u8, u8> {{ if None::<u8>.is_none() {{ Ok(1) }} else {{ Err(2) }} }}
}}
fn main() {{
    let mut h = H;
    assert_eq!(h.a_b_c_g(), Ok(1));
    let a = Some_ {{ n: 1 }};
    assert_eq!(h.a_b_c_f(a, None_::X, Ok_::default(), Err_::X, Default_(0)), Some(0));
}}
"#
    );
    let bin = common::build("wit_host_prelude", &program);
    assert!(Command::new(bin).status().unwrap().success());
}

/// A component around the core module `core` (wrapped by `wit-component`, which lowers
/// exactly the functions the core imports) importing `a:b/res`, which defines the resource `r`.
fn component_around(core: &str) -> Vec<u8> {
    use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
    let mut resolve = wit_parser::Resolve::default();
    let package = resolve
        .push_str(
            "test.wit",
            "package a:b;
             interface res { resource r { poke: func(); } }
             world w { import res; }",
        )
        .unwrap();
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

const POKE: &str = r#"(import "a:b/res" "[method]r.poke" (func $poke (param i32)))"#;
const DROP: &str = r#"(import "a:b/res" "[resource-drop]r" (func $drop (param i32)))"#;

#[test]
fn a_resource_drop_the_component_does_not_lower_is_not_declared() {
    let component = component_around(&format!("(module {POKE})"));
    let names: Vec<String> = wasm2rs::wit_functions(&component)
        .unwrap()
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert_eq!(names, ["[method]r.poke"]);
    assert!(!host(&component).contains("resource_drop"));
}

#[test]
fn a_resource_drop_the_component_lowers_is_declared() {
    let component = component_around(&format!("(module {POKE} {DROP})"));
    let names: Vec<String> = wasm2rs::wit_functions(&component)
        .unwrap()
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert_eq!(names, ["[method]r.poke", "[resource-drop]r"]);
}

/// The glue's `impl Imports for Glue` and the trait derived from the core's imports agree
/// (whether or not the core imports the drop), and the glue builds with `host_impl`.
fn glue_builds_with(core: &str, host_impl: &str) {
    glue_of_builds_with(&component_around(core), core, host_impl);
}

fn glue_of_builds_with(component: &[u8], core: &str, host_impl: &str) {
    let glue = wasm2rs::wit_host_glue(component).unwrap();
    let opts = TranspileOptions::default()
        .with_named_imports(true)
        .with_reentrant_imports(true);
    let generated = transpile_with_options(&wat::parse_str(core).unwrap(), &opts).unwrap();
    let program = format!(
        "{generated}\n{glue}\n{host_impl}\nfn main() {{ let _inst = Instance::new(Glue(H)); }}\n"
    );
    common::build("wit_host_drop", &program);
}

#[test]
fn the_glue_builds_when_the_core_does_not_import_the_drop() {
    glue_builds_with(
        &format!("(module {POKE})"),
        "struct H;
         impl Host for H { fn a_b_res_method_r_poke(&mut self, _self: R) {} }",
    );
}

#[test]
fn the_glue_builds_when_the_core_imports_the_drop() {
    glue_builds_with(
        &format!("(module {POKE} {DROP})"),
        "struct H;
         impl Host for H {
             fn a_b_res_method_r_poke(&mut self, _self: R) {}
             fn a_b_res_resource_drop_r(&mut self, _handle: R) {}
         }",
    );
}

/// An outer component importing `a:b/res` (with the resource `r`) and holding a nested
/// component that lowers `resource.drop` of it (as a composed component's inner component
/// does) when `inner_drops`. The outer body has no `canon resource.drop`; the nested one is not
/// mapped, so the drop must stay declared.
fn composed_component(inner_drops: bool) -> Vec<u8> {
    let drop = if inner_drops {
        "(core func (canon resource.drop $ir))"
    } else {
        ""
    };
    wat::parse_str(format!(
        r#"(component
          (import "a:b/res" (instance $i
            (export "r" (type $r (sub resource)))
            (export "poke" (func (param "self" (borrow $r))))))
          (alias export $i "r" (type $or))
          (core module $m (func))
          (component $inner
            (import "r" (type $ir (sub resource)))
            {drop}
            (core module (func)))
          (instance (instantiate $inner (with "r" (type $or)))))"#
    ))
    .unwrap()
}

#[test]
fn a_nested_component_keeps_every_resource_drop() {
    let names: Vec<String> = wasm2rs::wit_functions(&composed_component(true))
        .unwrap()
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert_eq!(names, ["poke", "[resource-drop]r"]);
}

#[test]
fn a_nested_component_without_a_drop_does_not_declare_one() {
    let names: Vec<String> = wasm2rs::wit_functions(&composed_component(false))
        .unwrap()
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert_eq!(names, ["poke"]);
}

#[test]
fn a_component_without_a_core_module_keeps_every_resource_drop() {
    let names: Vec<String> = wasm2rs::wit_functions(&component("", "a:b/c", POLL))
        .unwrap()
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert!(
        names.iter().any(|n| n == "[resource-drop]pollable"),
        "{names:?}"
    );
}

#[test]
fn a_top_level_import_that_is_not_an_instance_names_the_import_and_its_kind() {
    let cases = [
        ("fun", "a function", r#"(import "fun" (func))"#),
        (
            "val",
            "a value",
            r#"(import "val" (value $v u32)) (export "o" (value $v))"#,
        ),
        (
            "mod",
            "a module or component",
            r#"(import "mod" (core module))"#,
        ),
        (
            "comp",
            "a module or component",
            r#"(import "comp" (component))"#,
        ),
    ];
    for (name, kind, import) in cases {
        let wasm = wat::parse_str(format!("(component {import})")).unwrap();
        let err = wit_host_trait(&wasm).unwrap_err().to_string();
        assert!(err.contains("unsupported"), "{name}: {err}");
        assert!(err.contains(&format!("import \"{name}\"")), "{name}: {err}");
        assert!(err.contains(kind), "{name}: {err}");
    }
}

/// A component whose `defines_t` declares a type `$t`, then imports `pt` as
/// `(eq $t)` and an interface `a:b/d` with `f(p: pt)`.
fn eq_import_of(defines_t: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(component {defines_t}
          (import "pt" (type $e (eq $t)))
          (import "a:b/d" (instance
            (alias outer 1 $e (type $q))
            (export "f" (func (param "p" $q))))))"#
    ))
    .unwrap()
}

#[test]
fn an_eq_import_of_an_aliased_record_is_the_same_named_record() {
    let wasm = eq_import_of(
        r#"(type $r (record (field "x" u32)))
          (import "a:b/c" (instance $i
            (alias outer 1 $r (type $r0))
            (export "point" (type $p (eq $r0)))))
          (alias export $i "point" (type $t))"#,
    );
    let rust = host(&wasm);
    assert!(rust.contains("pub struct Point {"), "{rust}");
    assert!(rust.contains("fn a_b_d_f(&mut self, p: Point);"), "{rust}");
}

#[test]
fn an_instance_exported_by_the_component_keeps_its_types() {
    let wasm = eq_import_of(
        r#"(type $r (record (field "x" u32)))
          (import "a:b/c" (instance $i
            (alias outer 1 $r (type $r0))
            (export "point" (type $p (eq $r0)))))
          (export $ie "ie" (instance $i))
          (alias export $ie "point" (type $t))"#,
    );
    let rust = host(&wasm);
    // The type went through an instance export and is still `point`.
    assert!(rust.contains("fn a_b_d_f(&mut self, p: Point);"), "{rust}");
}

#[test]
fn a_type_of_an_instantiated_instance_is_an_error_naming_the_function() {
    // The type of an instance made by `instantiate` is not known to the host trait.
    let wasm = eq_import_of(
        r#"(component $c
            (type $r (record (field "x" u32)))
            (export "point" (type $r)))
          (instance $s (instantiate $c))
          (alias export $s "point" (type $t))"#,
    );
    let err = wit_host_trait(&wasm).unwrap_err().to_string();
    assert!(err.contains("unsupported"), "{err}");
    assert!(err.contains("unknown instance"), "{err}");
    assert!(err.contains("`f`") && err.contains("a:b/d"), "{err}");
}

#[test]
fn a_sub_resource_import_is_a_resource_named_after_the_import() {
    let wasm = wat::parse_str(
        r#"(component
          (import "my-res" (type $r (sub resource)))
          (import "a:b/c" (instance
            (alias outer 1 $r (type $q))
            (export "f" (func (param "x" (borrow $q)) (result (own $q)))))))"#,
    )
    .unwrap();
    let rust = host(&wasm);
    assert!(
        rust.contains("fn a_b_c_f(&mut self, x: MyRes) -> MyRes;"),
        "{rust}"
    );
}

#[test]
fn a_drop_that_is_not_tied_to_an_imported_resource_keeps_every_resource_drop() {
    // Two imported resources `r` and `s`, a core module, and one `canon resource.drop`.
    let names = |dropped: &str| -> Vec<String> {
        let wasm = wat::parse_str(format!(
            r#"(component
              (import "a:b/res" (instance $i
                (export "r" (type $r (sub resource)))
                (export "s" (type $s (sub resource)))))
              (alias export $i "r" (type $ir))
              (component $c
                (type $res (resource (rep i32)))
                (export "res" (type $res)))
              (instance $made (instantiate $c))
              (alias export $made "res" (type $local))
              (core func (canon resource.drop {dropped}))
              (core module))"#
        ))
        .unwrap();
        wasm2rs::wit_functions(&wasm)
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect()
    };
    // The drop of the imported `r` is tied to it: only that drop is declared.
    assert_eq!(names("$ir"), ["[resource-drop]r"]);
    // The drop of a resource of an instantiated instance is tied to no import: keep both.
    assert_eq!(names("$local"), ["[resource-drop]r", "[resource-drop]s"]);
}

#[test]
fn an_async_function_without_result_has_no_return_type() {
    let wasm = component("", "a:b/c", r#"(export "f" (func async (param "a" u8)))"#);
    assert!(host(&wasm).contains("fn a_b_c_f(&mut self, a: u8);"));
}

#[test]
fn an_async_function_among_synchronous_ones_marks_only_itself() {
    let wasm = component(
        "",
        "a:b/c",
        r#"(export "ok" (func (result u8)))
           (export "slow" (func async))"#,
    );
    let rust = host(&wasm);
    assert_eq!(rust.matches("An `async func`").count(), 1, "{rust}");
    assert!(rust.contains("fn a_b_c_ok(&mut self) -> u8;"), "{rust}");
}

#[test]
fn an_alias_of_a_type_exported_by_an_instantiated_component_is_not_an_error_when_unused() {
    // `$i` is the result of an `instantiate`, not an imported instance, so its
    // exports are unknown to the trait generator; aliasing one must not break
    // an unrelated import.
    let wasm = component(
        r#"(component $inner
             (type $r (record (field "n" u8)))
             (export "r" (type $r)))
           (instance $i (instantiate $inner))
           (alias export $i "r" (type $t))"#,
        "a:b/c",
        r#"(export "f" (func (result u8)))"#,
    );
    assert!(host(&wasm).contains("fn a_b_c_f(&mut self) -> u8;"));
}

#[test]
fn a_record_inside_a_tuple_is_declared() {
    let rust = host(&component(
        "",
        "a:b/c",
        r#"(type $p (record (field "x" u32)))
        (export "point" (type $pt (eq $p)))
        (export "f" (func (result (tuple $pt u8))))"#,
    ));
    assert!(rust.contains("pub struct Point {"), "{rust}");
    assert!(rust.contains("-> (Point, u8);"), "{rust}");
}

#[test]
fn a_variant_case_named_record_keeps_its_name() {
    // A case whose name has no letters or digits falls back to `Record` and is renamed
    // `Case`; a real `record` case is not.
    let rust = host(&component(
        "",
        "a:b/c",
        r#"(type $v (variant (case "record") (case "x")))
        (export "v" (type $vt (eq $v)))
        (export "f" (func (result $vt)))"#,
    ));
    assert!(rust.contains("Record,") && rust.contains("X,"), "{rust}");
    assert!(!rust.contains("Case"), "{rust}");
}

#[test]
fn an_exported_type_takes_a_slot_of_the_component_type_space() {
    // Types: `$ta` (0), the export of it (1), `$tb` (2). The interface uses `$tb`, which
    // resolves to the second record only if the export took its slot.
    let wasm = wat::parse_str(
        r#"(component
  (import "i" (instance $i
    (type $a (record (field "x" u32)))
    (export "a" (type (eq $a)))
    (type $b (record (field "y" u8)))
    (export "b" (type (eq $b)))))
  (alias export $i "a" (type $ta))
  (export $e "exported" (type $ta))
  (alias export $i "b" (type $tb))
  (import "a:b/c" (instance
    (alias outer 1 $tb (type $t))
    (export "t" (type $tt (eq $t)))
    (export "f" (func (result $tt))))))"#,
    )
    .unwrap();
    let rust = host(&wasm);
    assert!(rust.contains("pub y: u8,"), "{rust}");
}

#[test]
fn a_nested_component_with_nested_items_keeps_all_its_resource_drops() {
    // The nested component holds an inner component and then drops a resource itself. That
    // drop cannot be tied to a top-level resource, so both resources keep their
    // `[resource-drop]`. If the inner component ended the nested one early, the drop would be
    // read at the top level, where type 2 is the first resource.
    let wasm = wat::parse_str(
        r#"(component
  (import "a:b/one" (instance $one (export "r1" (type (sub resource)))))
  (import "a:b/two" (instance $two (export "r2" (type (sub resource)))))
  (alias export $one "r1" (type $r1))
  (alias export $two "r2" (type $r2))
  (core module $m)
  (component $n
    (import "pad0" (type (sub resource)))
    (import "pad1" (type (sub resource)))
    (import "r" (type $r (sub resource)))
    (component $inner)
    (core func (canon resource.drop $r))))"#,
    )
    .unwrap();
    let names: Vec<String> = wasm2rs::wit_functions(&wasm)
        .unwrap()
        .into_iter()
        .map(|f| format!("{}#{}", f.interface, f.name))
        .collect();
    assert_eq!(
        names,
        ["a:b/one#[resource-drop]r1", "a:b/two#[resource-drop]r2"],
        "{names:?}"
    );
}

/// A trait whose functions each return a tuple of `u8`s, one function per size.
fn tuples_of(sizes: &[usize]) -> Result<String, wasm2rs::TranspileError> {
    let funcs: String = sizes
        .iter()
        .enumerate()
        .map(|(i, k)| {
            format!(
                r#"(export "f{i}" (func (result (tuple {}))))"#,
                "u8 ".repeat(*k)
            )
        })
        .collect();
    wit_host_trait(&component("", "a:b/c", &funcs))
}

#[test]
fn the_type_node_budget_is_exactly_two_hundred_thousand() {
    // Each tuple node and each element is visited twice (declared, then rendered), so ten
    // tuples of 99 990 elements in all take 2 * (99 990 + 10) = 200 000 visits: the last
    // one that fits. One element more is refused.
    let mut sizes = vec![10_000; 9];
    sizes.push(9_990);
    assert!(tuples_of(&sizes).is_ok());
    sizes[9] = 9_991;
    let err = tuples_of(&sizes).unwrap_err().to_string();
    assert!(err.contains("200000 type nodes"), "{err}");
}

#[test]
fn an_async_function_becomes_a_typed_method_marked_async() {
    let wasm = component(
        "",
        "a:b/c",
        r#"(export "slow" (func async (param "n" u32) (result u32)))"#,
    );
    let rust = host(&wasm);
    assert!(
        rust.contains("fn a_b_c_slow(&mut self, n: u32) -> u32;"),
        "{rust}"
    );
    assert!(rust.contains("An `async func` (WASI 0.3)"), "{rust}");
}

#[test]
fn future_and_stream_are_typed_handles() {
    let wasm = component(
        "",
        "a:b/c",
        r#"(export "numbers" (func (result (stream u32))))
           (export "one" (func (param "f" (future string)) (result (option (future)))))"#,
    );
    let rust = host(&wasm);
    assert!(rust.contains("pub struct WitStream<T>"), "{rust}");
    assert!(rust.contains("pub struct WitFuture<T>"), "{rust}");
    assert!(
        rust.contains("fn a_b_c_numbers(&mut self) -> WitStream<u32>;"),
        "{rust}"
    );
    assert!(
        rust.contains("fn a_b_c_one(&mut self, f: WitFuture<String>) -> Option<WitFuture<()>>;"),
        "{rust}"
    );
}

#[test]
fn handle_types_are_declared_only_when_used() {
    let wasm = component("", "a:b/c", r#"(export "f" (func (result u8)))"#);
    assert!(!host(&wasm).contains("WitFuture"));
}

/// A component around `core` (wrapped by `wit-component`) importing `a:b/i`, whose WIT is
/// `body`.
fn p3_component_around(body: &str, core: &str) -> Vec<u8> {
    use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
    let mut resolve = wit_parser::Resolve::default();
    let package = resolve
        .push_str(
            "test.wit",
            &format!("package a:b; interface i {{ {body} }} world w {{ import i; }}"),
        )
        .unwrap();
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

#[test]
fn the_glue_passes_future_and_stream_handles_as_i32() {
    let core = r#"(module (import "a:b/i" "pump" (func (param i32) (result i32))))"#;
    let component = p3_component_around("pump: func(s: stream<u8>) -> future<u32>;", core);
    let glue = wasm2rs::wit_host_glue(&component).unwrap();
    assert!(glue.contains("<WitStream<u8>>::new("), "{glue}");
    assert!(glue.contains("self.0.a_b_i_pump(a0)"), "{glue}");
    glue_of_builds_with(
        &component,
        core,
        "struct H;\nimpl Host for H {\n    fn a_b_i_pump(&mut self, s: WitStream<u8>) -> WitFuture<u32> {\n        WitFuture::new(s.0)\n    }\n}",
    );
}
