//! `wit_host_glue` lowers the typed `Host` trait to the flat reentrant `Imports` trait of
//! the component's core module. Each test builds a component carrying only types and an
//! import, a hand-written core module importing the same interface and function names,
//! transpiles the module with `named_imports` + `reentrant_imports`, appends the glue and an
//! `impl Host`, and runs the result: the values must round-trip through the host.

use std::process::Command;

use crate::common;
use wasm2rs::{TranspileOptions, transpile_with_options, wit_host_glue, wit_host_trait};

/// A component importing `a:b/c`, whose instance type declares `body` (types and exports).
pub(crate) fn component(body: &str) -> Vec<u8> {
    wat::parse_str(format!(r#"(component (import "a:b/c" (instance {body})))"#)).unwrap()
}

/// Builds the program from the transpiled `core_wat` (its imports must be `a:b/c`
/// functions), the glue for `body`, then `rest` (the `impl Host` and `fn main`), and runs it.
pub(crate) fn build_and_run(
    name: &str,
    body: &str,
    core_wat: &str,
    rest: &str,
) -> std::process::Output {
    let glue = wit_host_glue(&component(body)).unwrap();
    let core = wat::parse_str(core_wat).unwrap();
    let opts = TranspileOptions {
        named_imports: true,
        reentrant_imports: true,
        named_exports: true,
        ..TranspileOptions::default()
    };
    let generated = transpile_with_options(&core, &opts).unwrap();
    let program = format!("{generated}\n{glue}\n{rest}\n");
    let bin = common::build(&format!("wit_glue_{name}"), &program);
    Command::new(bin).output().unwrap()
}

/// Asserts the program exits 0.
pub(crate) fn run(name: &str, body: &str, core_wat: &str, rest: &str) {
    let out = build_and_run(name, body, core_wat, rest);
    assert!(
        out.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Like [`run`], but the program must panic with `needle` on stderr.
pub(crate) fn run_panics(name: &str, body: &str, core_wat: &str, rest: &str, needle: &str) {
    let out = build_and_run(name, body, core_wat, rest);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{name} did not panic");
    assert!(stderr.contains(needle), "{name}: {stderr}");
}

#[test]
fn glue_methods_are_named_like_the_host_methods() {
    let body = r#"(export "get-it" (func (param "n" u32) (result u32)))
        (export "other-one" (func))"#;
    let wasm = component(body);
    let glue = wit_host_glue(&wasm).unwrap();
    let host = wit_host_trait(&wasm).unwrap();
    assert!(
        glue.starts_with(&host),
        "the glue extends the Host trait text"
    );
    for name in ["a_b_c_get_it", "a_b_c_other_one"] {
        assert!(host.contains(&format!("fn {name}(&mut self")), "{host}");
        assert!(
            glue.contains(&format!("    fn {name}(&mut self, g: &mut Instance<Self>")),
            "{glue}"
        );
    }
    assert!(glue.contains("pub struct Glue<H: Host>(pub H);"), "{glue}");
    assert!(
        glue.contains("impl<H: Host> Imports for Glue<H> {"),
        "{glue}"
    );
}

#[test]
fn every_scalar_kind_is_lifted_from_the_flat_params() {
    run(
        "scalars_in",
        r#"(export "f" (func
            (param "a" u8) (param "b" u16) (param "c" u32) (param "d" u64)
            (param "e" s8) (param "f" s16) (param "g" s32) (param "h" s64)
            (param "i" f32) (param "j" f64) (param "k" bool) (param "l" char)
            (result u64)))"#,
        r#"(module
          (import "a:b/c" "f" (func $f (param i32 i32 i32 i64 i32 i32 i32 i64 f32 f64 i32 i32) (result i64)))
          (func (export "run") (result i64)
            ;; the narrow signed values are sign-extended, as the canonical ABI lowers them
            (call $f (i32.const 255) (i32.const 65535) (i32.const -1) (i64.const -2)
                     (i32.const -3) (i32.const -300) (i32.const -70000) (i64.const -5000000000)
                     (f32.const 1.5) (f64.const -2.25) (i32.const 1) (i32.const 0x1F600))))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_f(&mut self, a: u8, b: u16, c: u32, d: u64, e: i8, f: i16, g: i32,
                       h: i64, i: f32, j: f64, k: bool, l: char) -> u64 {
                assert_eq!((a, b, c, d), (255, 65535, u32::MAX, u64::MAX - 1));
                assert_eq!((e, f, g, h), (-3, -300, -70000, -5_000_000_000));
                assert_eq!((i, j, k, l), (1.5, -2.25, true, '\u{1F600}'));
                0xDEAD_BEEF_0000_0001
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            assert_eq!(inst.run() as u64, 0xDEAD_BEEF_0000_0001);
        }
        "#,
    );
}

#[test]
fn every_scalar_kind_is_lowered_to_the_flat_result() {
    let results = [
        ("u8", "i32", "200"),
        ("u16", "i32", "65000"),
        ("u32", "i32", "-1"),
        ("u64", "i64", "-2"),
        ("s8", "i32", "-5"),
        ("s16", "i32", "-300"),
        ("s32", "i32", "-70000"),
        ("s64", "i64", "-5000000000"),
        ("f32", "f32", "1.5"),
        ("f64", "f64", "-2.25"),
        ("bool", "i32", "1"),
        ("char", "i32", "0x1F600"),
    ];
    let mut body = String::new();
    let mut imports = String::new();
    let mut exports = String::new();
    let mut host = String::new();
    let mut asserts = String::new();
    let values = [
        "200u8",
        "65000u16",
        "u32::MAX",
        "u64::MAX - 1",
        "-5i8",
        "-300i16",
        "-70000i32",
        "-5_000_000_000i64",
        "1.5f32",
        "-2.25f64",
        "true",
        "'\\u{1F600}'",
    ];
    let rust_ty = [
        "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f32", "f64", "bool", "char",
    ];
    for (i, (wit, core, lit)) in results.iter().enumerate() {
        body.push_str(&format!(r#"(export "r-{wit}" (func (result {wit})))"#));
        imports.push_str(&format!(
            r#"(import "a:b/c" "r-{wit}" (func $r{i} (result {core})))"#
        ));
        let konst = match *core {
            "i32" => format!("(i32.const {lit})"),
            "i64" => format!("(i64.const {lit})"),
            "f32" => format!("(f32.const {lit})"),
            _ => format!("(f64.const {lit})"),
        };
        exports.push_str(&format!(
            r#"(func (export "run{i}") (result {core}) (call $r{i}))
               (func (export "expect{i}") (result {core}) {konst})"#
        ));
        host.push_str(&format!(
            "fn a_b_c_r_{wit}(&mut self) -> {} {{ {} }}\n",
            rust_ty[i], values[i]
        ));
        asserts.push_str(&format!("assert_eq!(inst.run{i}(), inst.expect{i}());\n"));
    }
    run(
        "scalars_out",
        &body,
        &format!("(module {imports} {exports})"),
        &format!(
            "struct H;\nimpl Host for H {{ {host} }}\n\
             fn main() {{ let mut inst = Instance::new(Glue(H)); {asserts} }}"
        ),
    );
}

#[test]
fn narrow_ints_and_bool_are_lifted_leniently() {
    // The canonical ABI keeps the low bits of a narrow int and takes any non-zero bool as true.
    run(
        "lenient",
        r#"(export "f" (func (param "x" u8) (param "b" bool) (param "y" s8)
            (param "z" u16) (param "w" s16)))"#,
        r#"(module
          (import "a:b/c" "f" (func $f (param i32 i32 i32 i32 i32)))
          (func (export "run")
            (call $f (i32.const 0x1ff) (i32.const 2) (i32.const 0x1ff)
                     (i32.const 0x1ffff) (i32.const 0x18000))))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_f(&mut self, x: u8, b: bool, y: i8, z: u16, w: i16) {
                assert_eq!((x, b, y, z, w), (0xff, true, -1, 0xffff, i16::MIN));
            }
        }
        fn main() { Instance::new(Glue(H)).run(); }
        "#,
    );
}

#[test]
fn narrow_ints_read_from_a_joined_slot_wrap_to_32_bits_first() {
    // variant slot is i64 (u64 case); the u8 case reads its low 8 bits.
    run(
        "lenient_joined",
        r#"(type $w (variant (case "small" u8) (case "big" u64)))
        (export "w" (type $v (eq $w)))
        (export "f" (func (param "v" $v) (result u32)))"#,
        r#"(module
          (import "a:b/c" "f" (func $f (param i32 i64) (result i32)))
          (func (export "run") (result i32) (call $f (i32.const 0) (i64.const 0x1_0000_01ff))))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_f(&mut self, v: W) -> u32 {
                match v { W::Small(n) => u32::from(n), W::Big(_) => 0 }
            }
        }
        fn main() { assert_eq!(Instance::new(Glue(H)).run(), 0xff); }
        "#,
    );
}

#[test]
fn an_invalid_char_still_traps() {
    run_panics(
        "bad_char",
        r#"(export "f" (func (param "v" char)))"#,
        r#"(module
          (import "a:b/c" "f" (func $f (param i32)))
          (func (export "run") (call $f (i32.const 0xD800))))"#,
        r#"
        struct H;
        impl Host for H { fn a_b_c_f(&mut self, _v: char) {} }
        fn main() { Instance::new(Glue(H)).run(); }
        "#,
        "invalid char",
    );
}

#[test]
fn an_enum_round_trips_by_discriminant() {
    let body = r#"(type $e (enum "red" "green" "blue"))
        (export "color" (type $c (eq $e)))
        (export "next" (func (param "c" $c) (result $c)))"#;
    let core = r#"(module
        (import "a:b/c" "next" (func $next (param i32) (result i32)))
        (func (export "run") (param i32) (result i32) (call $next (local.get 0))))"#;
    run(
        "enum",
        body,
        core,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_next(&mut self, c: Color) -> Color {
                match c {
                    Color::Red => Color::Green,
                    Color::Green => Color::Blue,
                    Color::Blue => Color::Red,
                }
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            assert_eq!(inst.run(0), 1);
            assert_eq!(inst.run(1), 2);
            assert_eq!(inst.run(2), 0);
        }
        "#,
    );
    run_panics(
        "enum_bad",
        body,
        core,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_next(&mut self, c: Color) -> Color { c }
        }
        fn main() { Instance::new(Glue(H)).run(3); }
        "#,
        "invalid discriminant",
    );
}

#[test]
fn flags_of_each_width_round_trip_as_their_bits() {
    let many = |n: usize| {
        (0..n)
            .map(|i| format!(r#""f{i}""#))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let body = format!(
        r#"(type $s (flags {a}))
        (export "small" (type $small (eq $s)))
        (type $m (flags {b}))
        (export "medium" (type $medium (eq $m)))
        (type $l (flags {c}))
        (export "large" (type $large (eq $l)))
        (export "s" (func (param "v" $small) (result $small)))
        (export "m" (func (param "v" $medium) (result $medium)))
        (export "l" (func (param "v" $large) (result $large)))"#,
        a = many(3),
        b = many(12),
        c = many(32),
    );
    let core = r#"(module
        (import "a:b/c" "s" (func $s (param i32) (result i32)))
        (import "a:b/c" "m" (func $m (param i32) (result i32)))
        (import "a:b/c" "l" (func $l (param i32) (result i32)))
        (func (export "rs") (param i32) (result i32) (call $s (local.get 0)))
        (func (export "rm") (param i32) (result i32) (call $m (local.get 0)))
        (func (export "rl") (param i32) (result i32) (call $l (local.get 0))))"#;
    let host = r#"
        struct H;
        impl Host for H {
            fn a_b_c_s(&mut self, v: Small) -> Small { Small(v.0 ^ 0b101) }
            fn a_b_c_m(&mut self, v: Medium) -> Medium { Medium(v.0 ^ 0x800) }
            fn a_b_c_l(&mut self, v: Large) -> Large { Large(v.0 ^ 0x8000_0000) }
        }
    "#;
    run(
        "flags",
        &body,
        core,
        &format!(
            "{host}
            fn main() {{
                let mut inst = Instance::new(Glue(H));
                assert_eq!(inst.rs(0b010), 0b111);
                assert_eq!(inst.rm(0x001), 0x801);
                assert_eq!(inst.rl(0x8000_0001u32 as i32), 1);
                let _ = Small::default();
            }}"
        ),
    );
    // A bit beyond the declared flags is ignored.
    run(
        "flags_extra_bits",
        r#"(type $s (flags "a"))
        (export "one" (type $one (eq $s)))
        (export "f" (func (param "v" $one) (result u32)))"#,
        r#"(module
          (import "a:b/c" "f" (func $f (param i32) (result i32)))
          (func (export "run") (result i32) (call $f (i32.const 3))))"#,
        r#"
        struct H;
        impl Host for H { fn a_b_c_f(&mut self, v: One) -> u32 { u32::from(v.0) } }
        fn main() { assert_eq!(Instance::new(Glue(H)).run(), 1); }
        "#,
    );
}

#[test]
fn resource_handles_cross_in_and_out_and_drop_is_covered() {
    let body = r#"(export "r" (type $r (sub resource)))
        (export "[constructor]r" (func (param "n" u32) (result (own $r))))
        (export "[method]r.get" (func (param "self" (borrow $r)) (result u32)))
        (export "[static]r.pick" (func (param "a" (own $r)) (param "b" (own $r)) (result (own $r))))"#;
    // The drop import is named by the defining interface, `[resource-drop]r`.
    let core = r#"(module
        (import "a:b/c" "[constructor]r" (func $new (param i32) (result i32)))
        (import "a:b/c" "[method]r.get" (func $get (param i32) (result i32)))
        (import "a:b/c" "[static]r.pick" (func $pick (param i32 i32) (result i32)))
        (import "a:b/c" "[resource-drop]r" (func $drop (param i32)))
        (func (export "run") (result i32)
          (local $a i32) (local $b i32) (local $c i32) (local $v i32)
          (local.set $a (call $new (i32.const 7)))
          (local.set $b (call $new (i32.const 9)))
          (local.set $c (call $pick (local.get $a) (local.get $b)))
          (local.set $v (call $get (local.get $c)))
          (call $drop (local.get $c))
          (local.get $v)))"#;
    run(
        "resource",
        body,
        core,
        r#"
        struct H { live: Vec<u32>, dropped: std::rc::Rc<std::cell::RefCell<Vec<u32>>> }
        impl Host for H {
            fn a_b_c_constructor_r(&mut self, n: u32) -> R {
                self.live.push(n);
                R(100 + n)
            }
            fn a_b_c_method_r_get(&mut self, self_: R) -> u32 { self_.0 - 100 }
            fn a_b_c_static_r_pick(&mut self, a: R, b: R) -> R {
                assert_eq!((a, b), (R(107), R(109)));
                b
            }
            fn a_b_c_resource_drop_r(&mut self, handle: R) { self.dropped.borrow_mut().push(handle.0); }
        }
        fn main() {
            let dropped = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
            let mut inst = Instance::new(Glue(H { live: vec![], dropped: dropped.clone() }));
            assert_eq!(inst.run(), 9);
            assert_eq!(*dropped.borrow(), vec![109]);
        }
        "#,
    );
}

#[test]
fn a_variant_with_differently_typed_payloads_is_lifted_through_join() {
    // flat: disc, then i64 (join of s32 -> i32, f32 -> i32, u64 -> i64).
    let body = r#"(type $w (variant (case "nothing") (case "num" s32) (case "real" f32) (case "big" u64)))
        (export "word" (type $v (eq $w)))
        (export "f" (func (param "v" $v) (result s64)))"#;
    let core = r#"(module
        (import "a:b/c" "f" (func $f (param i32 i64) (result i64)))
        (func (export "run") (param i32 i64) (result i64) (call $f (local.get 0) (local.get 1))))"#;
    run(
        "variant_join",
        body,
        core,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_f(&mut self, v: Word) -> i64 {
                match v {
                    Word::Nothing => -1,
                    Word::Num(n) => i64::from(n),
                    Word::Real(r) => (r * 2.0) as i64,
                    Word::Big(b) => (b >> 1) as i64,
                }
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            assert_eq!(inst.run(0, 12345), -1); // unused payload slot is ignored
            assert_eq!(inst.run(1, 0xFFFF_FFFE), -2); // wraps to s32 -2
            assert_eq!(inst.run(2, i64::from(1.5f32.to_bits())), 3);
            assert_eq!(inst.run(3, 1 << 40), 1 << 39);
        }
        "#,
    );
}

#[test]
fn an_option_and_a_result_are_lifted_with_their_payload_slots() {
    let body = r#"(export "f" (func (param "o" (option u8)) (param "r" (result s16 (error f32))) (result u32)))"#;
    // option<u8>: disc, u8. result<s16, f32>: disc, joined(s16 -> i32, f32 -> i32).
    let core = r#"(module
        (import "a:b/c" "f" (func $f (param i32 i32 i32 i32) (result i32)))
        (func (export "run") (param i32 i32 i32 i32) (result i32)
          (call $f (local.get 0) (local.get 1) (local.get 2) (local.get 3))))"#;
    run(
        "option_result",
        body,
        core,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_f(&mut self, o: Option<u8>, r: Result<i16, f32>) -> u32 {
                let a = match o { None => 1000, Some(b) => u32::from(b) };
                let b = match r {
                    Ok(v) => v as i32 as u32,
                    Err(e) => (e * 10.0) as u32,
                };
                a + b
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            assert_eq!(inst.run(0, 77, 0, 5), 1005);
            assert_eq!(inst.run(1, 7, 0, 5), 12);
            assert_eq!(inst.run(1, 7, 1, 2.5f32.to_bits() as i32), 32);
        }
        "#,
    );
}

#[test]
fn a_variant_with_a_multi_slot_case_joins_each_slot() {
    // case a: [f32]; case b: [i32, f64]. Flat: disc, join(f32, i32) = i32, f64.
    let body = r#"(type $w (variant (case "a" f32) (case "b" (tuple u8 f64))))
        (export "w" (type $v (eq $w)))
        (export "f" (func (param "v" $v) (result f64)))"#;
    let core = r#"(module
        (import "a:b/c" "f" (func $f (param i32 i32 f64) (result f64)))
        (func (export "run") (param i32 i32 f64) (result f64)
          (call $f (local.get 0) (local.get 1) (local.get 2))))"#;
    run(
        "variant_slots",
        body,
        core,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_f(&mut self, v: W) -> f64 {
                match v {
                    W::A(x) => f64::from(x),
                    W::B((n, d)) => f64::from(n) + d,
                }
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            assert_eq!(inst.run(0, 2.5f32.to_bits() as i32, 0.0), 2.5);
            assert_eq!(inst.run(1, 4, 0.25), 4.25);
        }
        "#,
    );
}

#[test]
fn a_record_of_scalars_and_a_flat_result_record_cross() {
    let body = r#"(type $p (record (field "x" s32) (field "y" s32) (field "scale" f64)))
        (export "point" (type $point (eq $p)))
        (type $o (record (field "n" u32)))
        (export "one" (type $one (eq $o)))
        (type $m (record (field "p" $point) (field "pair" (tuple u8 bool))))
        (export "mix" (type $mix (eq $m)))
        (export "scale" (func (param "p" $point) (result f64)))
        (export "mk" (func (param "n" u32) (result $one)))
        (export "unmix" (func (param "m" $mix) (result u32)))"#;
    let core = r#"(module
        (import "a:b/c" "scale" (func $scale (param i32 i32 f64) (result f64)))
        (import "a:b/c" "mk" (func $mk (param i32) (result i32)))
        (import "a:b/c" "unmix" (func $unmix (param i32 i32 f64 i32 i32) (result i32)))
        (func (export "rs") (param i32 i32 f64) (result f64)
          (call $scale (local.get 0) (local.get 1) (local.get 2)))
        (func (export "rm") (param i32) (result i32) (call $mk (local.get 0)))
        (func (export "ru") (result i32)
          (call $unmix (i32.const 3) (i32.const -4) (f64.const 0.5) (i32.const 9) (i32.const 1))))"#;
    run(
        "records",
        body,
        core,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_scale(&mut self, p: Point) -> f64 { f64::from(p.x + p.y) * p.scale }
            fn a_b_c_mk(&mut self, n: u32) -> One { One { n: n * 2 } }
            fn a_b_c_unmix(&mut self, m: Mix) -> u32 {
                assert_eq!((m.p.x, m.p.y, m.p.scale), (3, -4, 0.5));
                assert_eq!(m.pair, (9, true));
                u32::from(m.pair.0)
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            assert_eq!(inst.rs(3, 4, 0.5), 3.5);
            assert_eq!(inst.rm(21), 42);
            assert_eq!(inst.ru(), 9);
        }
        "#,
    );
}

#[test]
fn a_payload_free_variant_and_a_unit_result_are_lowered() {
    let body = r#"(type $w (variant (case "yes") (case "no") (case "maybe")))
        (export "answer" (type $a (eq $w)))
        (export "ask" (func (param "n" u8) (result $a)))
        (export "check" (func (param "n" u8) (result (result))))"#;
    // `check` is result<_, _>: its flat form is just the discriminant.
    let core = r#"(module
        (import "a:b/c" "ask" (func $ask (param i32) (result i32)))
        (import "a:b/c" "check" (func $check (param i32) (result i32)))
        (func (export "ra") (param i32) (result i32) (call $ask (local.get 0)))
        (func (export "rc") (param i32) (result i32) (call $check (local.get 0))))"#;
    run(
        "payload_free",
        body,
        core,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_ask(&mut self, n: u8) -> Answer {
                match n { 0 => Answer::Yes, 1 => Answer::No, _ => Answer::Maybe }
            }
            fn a_b_c_check(&mut self, n: u8) -> Result<(), ()> {
                if n == 0 { Ok(()) } else { Err(()) }
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            assert_eq!((inst.ra(0), inst.ra(1), inst.ra(9)), (0, 1, 2));
            assert_eq!((inst.rc(0), inst.rc(1)), (0, 1));
        }
        "#,
    );
}

#[test]
fn sixteen_flat_params_are_passed_flat() {
    let params = |n: usize| {
        (0..n)
            .map(|i| format!(r#"(param "p{i}" u32)"#))
            .collect::<String>()
    };
    let ok = format!(r#"(export "wide" (func {} (result u32)))"#, params(16));
    let glue = wit_host_glue(&component(&ok)).unwrap();
    assert!(glue.contains("p15: i32"), "{glue}");
    assert!(!glue.contains("p16"), "{glue}");
    let sum_args = (0..16)
        .map(|i| format!("{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let core_params = "i32 ".repeat(16);
    let gets = (0..16)
        .map(|i| format!("(local.get {i})"))
        .collect::<String>();
    let host_params = (0..16)
        .map(|i| format!("p{i}: u32"))
        .collect::<Vec<_>>()
        .join(", ");
    let sum = (0..16)
        .map(|i| format!("p{i}"))
        .collect::<Vec<_>>()
        .join(" + ");
    run(
        "sixteen",
        &ok,
        &format!(
            r#"(module
              (import "a:b/c" "wide" (func $w (param {core_params}) (result i32)))
              (func (export "run") (param {core_params}) (result i32) {gets} call $w))"#
        ),
        &format!(
            "struct H;
            impl Host for H {{
                fn a_b_c_wide(&mut self, {host_params}) -> u32 {{ {sum} }}
            }}
            fn main() {{
                let mut inst = Instance::new(Glue(H));
                assert_eq!(inst.run({sum_args}), 120);
            }}"
        ),
    );
}

#[test]
fn the_cli_glue_flag_prints_the_glue_and_needs_wit_host() {
    let dir = std::env::temp_dir().join(format!("wasm2rs_wit_glue_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("c.wasm");
    let wasm = component(r#"(export "f" (func (param "n" u8) (result u8)))"#);
    std::fs::write(&input, &wasm).unwrap();

    let printed = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
        .args(["--wit-host", "--glue"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(printed.status.success());
    assert_eq!(
        String::from_utf8_lossy(&printed.stdout),
        wit_host_glue(&wasm).unwrap()
    );

    let alone = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
        .arg("--glue")
        .arg(&input)
        .output()
        .unwrap();
    assert!(!alone.status.success());
    assert!(String::from_utf8_lossy(&alone.stderr).contains("--glue"));

    let string_param = dir.join("s.wasm");
    std::fs::write(
        &string_param,
        component(r#"(export "say" (func (param "s" string)))"#),
    )
    .unwrap();
    // A string parameter is covered too (the memory-backed glue).
    let with_string = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
        .args(["--wit-host", "--glue"])
        .arg(&string_param)
        .output()
        .unwrap();
    assert!(with_string.status.success());
    assert!(String::from_utf8_lossy(&with_string.stdout).contains("glue_abi::string("));
}

#[test]
fn type_names_that_clash_with_the_generated_code_get_a_trailing_underscore() {
    let body = r#"(export "glue" (type $g (sub resource)))
        (type $r (record (field "n" u8)))
        (export "instance" (type $i (eq $r)))
        (type $e (enum "x" "y"))
        (export "imports" (type $m (eq $e)))
        (export "f" (func (param "g" (own $g)) (param "i" $i) (param "m" $m) (result u8)))"#;
    let glue = wit_host_glue(&component(body)).unwrap();
    for want in [
        "pub struct Glue_(",
        "pub struct Instance_ {",
        "pub enum Imports_ {",
    ] {
        assert!(glue.contains(want), "missing {want}:\n{glue}");
    }
    run(
        "reserved_names",
        body,
        r#"(module
          (import "a:b/c" "f" (func $f (param i32 i32 i32) (result i32)))
          (import "a:b/c" "[resource-drop]glue" (func $drop (param i32)))
          (func (export "run") (result i32) (call $f (i32.const 5) (i32.const 6) (i32.const 1))))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_resource_drop_glue(&mut self, _handle: Glue_) {}
            fn a_b_c_f(&mut self, g: Glue_, i: Instance_, m: Imports_) -> u8 {
                assert_eq!((g.0, i.n, m), (5, 6, Imports_::Y));
                7
            }
        }
        fn main() { assert_eq!(Instance::new(Glue(H)).run(), 7); }
        "#,
    );
}
