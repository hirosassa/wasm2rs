//! Interface-qualified type names (`WitOptions::naming = Qualified`) and the external-types
//! glue mode (`WitOptions::external_types`).

use crate::common;
use crate::wasi_generated::wasi_p2_component;
use wasm2rs::{
    WitNaming, WitOptions, WitType, wit_functions, wit_host_glue_with_options, wit_host_trait,
    wit_host_trait_with_options,
};

fn qualified() -> WitOptions {
    WitOptions {
        naming: WitNaming::Qualified,
        ..WitOptions::default()
    }
}

/// The external-types glue mode, naming the library at `path`.
fn external(path: &str) -> WitOptions {
    WitOptions {
        external_types: Some(path.to_string()),
        ..WitOptions::default()
    }
}

/// An interface that defines the enum `error-code` with `cases` and a function returning it.
fn error_code_import(interface: &str, cases: &str) -> String {
    format!(
        r#"(import "{interface}" (instance
            (type $e (enum {cases}))
            (export "error-code" (type $c (eq $e)))
            (export "last-error" (func (result $c)))))"#
    )
}

fn component(imports: &[String]) -> Vec<u8> {
    wat::parse_str(format!("(component {})", imports.join("\n"))).unwrap()
}

/// `a:b/types` defines the record `rec`; `a:b/use` re-exports it (through an outer alias, the
/// way `wit-component` encodes a `use`) and has a function returning it.
fn shared_record_component() -> Vec<u8> {
    wat::parse_str(
        r#"(component
          (import "a:b/types" (instance $t
            (type $r (record (field "x" u32)))
            (export "rec" (type (eq $r)))))
          (alias export $t "rec" (type $rec))
          (import "a:b/use" (instance
            (alias outer 1 $rec (type $r))
            (export "rec" (type $e (eq $r)))
            (export "get" (func (result $e))))))"#,
    )
    .unwrap()
}

#[test]
fn a_type_remembers_the_interface_that_defines_it() {
    let funcs = wit_functions(&shared_record_component()).unwrap();
    let get = funcs.iter().find(|f| f.name == "get").unwrap();
    let Some(WitType::Record(r)) = &get.result else {
        unreachable!("{:?}", get.result)
    };
    // Referenced through `a:b/use`, defined by `a:b/types`.
    assert_eq!(r.interface.as_deref(), Some("a:b/types"));
}

#[test]
fn a_resource_remembers_the_interface_that_defines_it() {
    let wasm = wat::parse_str(
        r#"(component
          (import "a:b/err" (instance $e
            (export "error" (type $x (sub resource)))))
          (alias export $e "error" (type $error))
          (import "a:b/streams" (instance
            (alias outer 1 $error (type $er))
            (export "error" (type $r (eq $er)))
            (type $own (own $r))
            (export "f" (func (result $own))))))"#,
    )
    .unwrap();
    let funcs = wit_functions(&wasm).unwrap();
    let f = funcs.iter().find(|f| f.name == "f").unwrap();
    let Some(WitType::Own(r)) = &f.result else {
        unreachable!("{:?}", f.result)
    };
    assert_eq!(r.interface.as_deref(), Some("a:b/err"));
}

#[test]
fn default_naming_numbers_a_repeated_type_name() {
    let wasm = component(&[
        error_code_import("wasi:filesystem/types@0.2.0", r#""access" "busy""#),
        error_code_import("wasi:sockets/network@0.2.0", r#""timeout" "refused""#),
    ]);
    let rust = wit_host_trait(&wasm).unwrap();
    assert!(rust.contains("pub enum ErrorCode2 {"), "{rust}");
}

#[test]
fn qualified_naming_puts_each_type_in_its_interfaces_module() {
    let wasm = component(&[
        error_code_import("wasi:filesystem/types@0.2.0", r#""access" "busy""#),
        error_code_import("wasi:sockets/network@0.2.0", r#""timeout" "refused""#),
    ]);
    let rust = wit_host_trait_with_options(&wasm, &qualified()).unwrap();
    assert!(!rust.contains("ErrorCode2"), "{rust}");
    assert!(rust.contains("pub mod wasi_filesystem_types {"), "{rust}");
    assert!(rust.contains("pub mod wasi_sockets_network {"), "{rust}");
    assert!(
        rust.contains(
            "fn wasi_filesystem_types_last_error(&mut self) -> wasi_filesystem_types::ErrorCode;"
        ),
        "{rust}"
    );
    assert!(
        rust.contains(
            "fn wasi_sockets_network_last_error(&mut self) -> wasi_sockets_network::ErrorCode;"
        ),
        "{rust}"
    );
}

#[test]
fn a_qualified_type_name_does_not_depend_on_the_other_imports() {
    let fs = error_code_import("wasi:filesystem/types@0.2.0", r#""access" "busy""#);
    let net = error_code_import("wasi:sockets/network@0.2.0", r#""timeout" "refused""#);
    let alone =
        wit_host_trait_with_options(&component(std::slice::from_ref(&fs)), &qualified()).unwrap();
    let both = wit_host_trait_with_options(&component(&[net, fs]), &qualified()).unwrap();
    let ty = "fn wasi_filesystem_types_last_error(&mut self) -> wasi_filesystem_types::ErrorCode;";
    assert!(alone.contains(ty), "{alone}");
    assert!(both.contains(ty), "{both}");
    // The module is the same text too, whatever the import order.
    let module = |s: &str| {
        let start = s.find("pub mod wasi_filesystem_types {").unwrap();
        s[start..start + s[start..].find("\n}\n").unwrap()].to_string()
    };
    assert_eq!(module(&alone), module(&both));
}

#[test]
fn a_shared_type_is_declared_once_in_its_definers_module() {
    let rust = wit_host_trait_with_options(&shared_record_component(), &qualified()).unwrap();
    assert_eq!(rust.matches("pub struct Rec {").count(), 1, "{rust}");
    let types = rust.find("pub mod a_b_types {").unwrap();
    assert!(rust.find("pub struct Rec {").unwrap() > types, "{rust}");
    assert!(!rust.contains("pub mod a_b_use"), "{rust}");
    assert!(
        rust.contains("fn a_b_use_get(&mut self) -> a_b_types::Rec;"),
        "{rust}"
    );
}

#[test]
fn a_type_refers_to_another_module_through_super() {
    let wasm = wat::parse_str(
        r#"(component
          (import "a:b/types" (instance $t
            (type $r (record (field "x" u32)))
            (export "rec" (type (eq $r)))))
          (alias export $t "rec" (type $rec))
          (import "a:b/use" (instance
            (alias outer 1 $rec (type $r))
            (export "rec" (type $e (eq $r)))
            (type $w (record (field "inner" $e) (field "n" u8)))
            (export "wrapper" (type $x (eq $w)))
            (export "get" (func (result $x))))))"#,
    )
    .unwrap();
    let rust = wit_host_trait_with_options(&wasm, &qualified()).unwrap();
    assert!(rust.contains("pub inner: super::a_b_types::Rec,"), "{rust}");
}

#[test]
fn two_interfaces_may_define_the_same_name_and_shape() {
    // Each declares its own `error-code { a }`: two types, so two declarations.
    let wasm = component(&[
        error_code_import("x:y/one", r#""a""#),
        error_code_import("x:y/two", r#""a""#),
    ]);
    let rust = wit_host_trait_with_options(&wasm, &qualified()).unwrap();
    assert!(rust.contains("pub mod x_y_one {"), "{rust}");
    assert!(rust.contains("pub mod x_y_two {"), "{rust}");
    assert_eq!(rust.matches("pub enum ErrorCode {").count(), 2, "{rust}");
}

#[test]
fn a_module_name_never_collides_with_a_reserved_name() {
    // `glue_abi` is the glue's helper module; `type` is a keyword; `std` is a crate.
    let wasm = component(&[
        error_code_import("glue-abi", r#""a""#),
        error_code_import("type", r#""a""#),
        error_code_import("std", r#""a""#),
    ]);
    let rust = wit_host_trait_with_options(&wasm, &qualified()).unwrap();
    for module in ["glue_abi_", "type_", "std_"] {
        assert!(rust.contains(&format!("pub mod {module} {{")), "{rust}");
    }
}

#[test]
fn two_interface_names_with_the_same_module_name_get_distinct_modules() {
    let wasm = component(&[
        error_code_import("a:b/c@0.2.0", r#""a""#),
        error_code_import("a:b/c@0.3.0", r#""b""#),
    ]);
    let rust = wit_host_trait_with_options(&wasm, &qualified()).unwrap();
    assert!(rust.contains("pub mod a_b_c {"), "{rust}");
    assert!(rust.contains("pub mod a_b_c_2 {"), "{rust}");
}

#[test]
fn the_glue_of_a_qualified_component_names_the_qualified_types() {
    let wasm = component(&[r#"(import "a:b/c" (instance
        (type $e (enum "a" "b"))
        (export "error-code" (type $c (eq $e)))
        (export "take" (func (param "e" $c)))))"#
        .to_string()]);
    let glue = wit_host_glue_with_options(&wasm, &qualified()).unwrap();
    assert!(glue.contains("a_b_c::ErrorCode::"), "{glue}");
}

/// `a:b/types` defines `color` and `pt`; `a:b/ops` uses both (through outer aliases, as
/// `wit-component` encodes a `use`).
fn ops_component() -> Vec<u8> {
    wat::parse_str(
        r#"(component
          (import "a:b/types" (instance $t
            (type $c (enum "red" "green"))
            (export "color" (type $ce (eq $c)))
            (type $p (record (field "x" u32) (field "y" u32)))
            (export "pt" (type (eq $p)))
            (export "default-color" (func (result $ce)))))
          (alias export $t "color" (type $color))
          (alias export $t "pt" (type $pt))
          (import "a:b/ops" (instance
            (alias outer 1 $color (type $c))
            (export "color" (type $ce (eq $c)))
            (alias outer 1 $pt (type $p))
            (export "pt" (type $pe (eq $p)))
            (export "mix" (func (param "c" $ce) (param "p" $pe) (result u32))))))"#,
    )
    .unwrap()
}

const OPS_CORE: &str = r#"(module
  (import "a:b/ops" "mix" (func $mix (param i32 i32 i32) (result i32)))
  (import "a:b/types" "default-color" (func $dc (result i32)))
  (func (export "run") (result i32) (call $mix (call $dc) (i32.const 3) (i32.const 4))))"#;

const OPS_HOST: &str = r#"
struct H;
impl host_lib::Host for H {
    fn a_b_types_default_color(&mut self) -> host_lib::a_b_types::Color {
        host_lib::a_b_types::Color::Green
    }
    fn a_b_ops_mix(&mut self, c: host_lib::a_b_types::Color, p: host_lib::a_b_types::Pt) -> u32 {
        assert_eq!(c, host_lib::a_b_types::Color::Green);
        p.x * 10 + p.y
    }
}
fn main() {
    let mut inst = Instance::new(Glue(H));
    assert_eq!(inst.run(), 34);
}
"#;

#[test]
fn the_external_glue_runs_against_a_library_that_declares_the_types() {
    let wasm = ops_component();
    // The library: the whole qualified surface in a module the glue refers to by path.
    let surface = wit_host_trait_with_options(&wasm, &qualified()).unwrap();
    let glue = wit_host_glue_with_options(&wasm, &external("host_lib")).unwrap();
    assert!(!glue.contains("pub trait Host"), "{glue}");
    assert!(!glue.contains("pub enum Color"), "{glue}");
    assert!(glue.contains("use host_lib::Host;"), "{glue}");

    let opts = wasm2rs::TranspileOptions::default()
        .with_named_imports(true)
        .with_reentrant_imports(true)
        .with_named_exports(true);
    let generated =
        wasm2rs::transpile_with_options(&wat::parse_str(OPS_CORE).unwrap(), &opts).unwrap();
    let program = format!("{generated}\nmod host_lib {{\n{surface}\n}}\n{glue}\n{OPS_HOST}\n");
    let bin = common::build("wit_qualified_external", &program);
    let out = std::process::Command::new(bin).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_qualified_surface_of_all_of_wasi_p2_compiles() {
    let rust = wit_host_trait_with_options(&wasi_p2_component(), &qualified()).unwrap();
    // The two error enums that default naming had to number are plain names in their modules.
    assert!(!rust.contains("ErrorCode2"), "{rust}");
    for module in [
        "wasi_filesystem_types",
        "wasi_sockets_network",
        "wasi_io_error",
    ] {
        assert!(rust.contains(&format!("pub mod {module} {{")), "{module}");
    }
    let program = format!("#![allow(dead_code)]\nmod host_lib {{\n{rust}\n}}\nfn main() {{}}\n");
    common::build("wit_qualified_wasi_p2", &program);
}

#[test]
fn the_cli_writes_the_external_glue_and_rejects_misuse() {
    let dir = std::env::temp_dir().join(format!("wasm2rs_wit_external_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("c.wasm");
    let wasm = ops_component();
    std::fs::write(&input, &wasm).unwrap();
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
            .arg("--wit-host")
            .args(args)
            .arg(&input)
            .output()
            .unwrap()
    };

    let printed = run(&["--glue", "--external-types=my_host::wasi"]);
    assert!(
        printed.status.success(),
        "{}",
        String::from_utf8_lossy(&printed.stderr)
    );
    let expected = wit_host_glue_with_options(&wasm, &external("my_host::wasi")).unwrap();
    assert_eq!(String::from_utf8_lossy(&printed.stdout), expected);

    // The glue is what refers to a library's types; the trait alone has nothing to refer to.
    let no_glue = run(&["--external-types=my_host::wasi"]);
    assert!(!no_glue.status.success());
    assert!(String::from_utf8_lossy(&no_glue.stderr).contains("--external-types"));
    let empty = run(&["--glue", "--external-types="]);
    assert!(!empty.status.success());
    // Other modes take no such flag.
    let dump = std::process::Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
        .args(["--dump", "--external-types=x"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(!dump.status.success());
}

fn default_trap() -> WitOptions {
    WitOptions {
        default_trap: true,
        ..WitOptions::default()
    }
}

#[test]
fn default_trap_gives_every_method_a_body() {
    let wasm = component(&[error_code_import(
        "wasi:filesystem/types@0.2.0",
        r#""access" "busy""#,
    )]);
    let plain = wit_host_trait(&wasm).unwrap();
    assert!(plain.contains("-> ErrorCode;"), "{plain}");
    let rust = wit_host_trait_with_options(&wasm, &default_trap()).unwrap();
    assert!(!rust.contains("-> ErrorCode;"), "{rust}");
    assert!(
        rust.contains("not implemented: wasi_filesystem_types_last_error"),
        "{rust}"
    );
}

#[test]
fn a_host_may_override_some_default_trap_methods_and_run() {
    let wasm = wat::parse_str(
        r#"(component
          (import "a:b/c" (instance
            (export "one" (func (result u32)))
            (export "two" (func (param "n" u32) (result u32))))))"#,
    )
    .unwrap();
    let trait_src = wit_host_trait_with_options(&wasm, &default_trap()).unwrap();
    let program = format!(
        r#"{trait_src}
struct H;
impl Host for H {{
    fn a_b_c_one(&mut self) -> u32 {{ 7 }}
}}
fn main() {{
    let mut h = H;
    assert_eq!(h.a_b_c_one(), 7);
    let r = std::panic::catch_unwind(move || {{ let mut h = H; h.a_b_c_two(1) }});
    let payload = r.unwrap_err();
    let msg = payload.downcast_ref::<&str>().copied().unwrap_or("");
    assert!(msg.contains("a_b_c_two"), "{{msg}}");
}}
"#
    );
    let bin = common::build("wit_default_trap", &program);
    assert!(std::process::Command::new(bin).status().unwrap().success());
}
