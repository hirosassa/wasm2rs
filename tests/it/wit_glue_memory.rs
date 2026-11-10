//! The memory-backed half of the glue: strings, lists, values spilled to memory (more than 16
//! flat parameters, a result of more than one flat value). Each test hand-writes a core module
//! with linear memory and an exported bump-allocator `cabi_realloc`, transpiles it with
//! `named_imports` + `reentrant_imports` + `named_exports`, appends the glue and an
//! `impl Host`, and checks what the host saw and what the guest memory holds afterwards.

use crate::wit_glue::{component, run, run_panics};
use wasm2rs::wit_host_glue;

/// Linear memory plus a bump `cabi_realloc` (aligns the pointer, never frees).
const MEM: &str = r#"
    (memory (export "memory") 1)
    (global $bump (mut i32) (i32.const 1024))
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
      (local $p i32)
      (local.set $p (i32.and (i32.add (global.get $bump) (i32.sub (local.get 2) (i32.const 1)))
                             (i32.sub (i32.const 0) (local.get 2))))
      (global.set $bump (i32.add (local.get $p) (local.get 3)))
      (local.get $p))
"#;

/// Rust helpers the `main` of each test uses to read the guest's memory.
const HELP: &str = r#"
#[allow(dead_code)]
fn u32_at(m: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(m[at..at + 4].try_into().unwrap())
}
#[allow(dead_code)]
fn u64_at(m: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(m[at..at + 8].try_into().unwrap())
}
/// The string whose `(ptr, len)` is at `at`.
#[allow(dead_code)]
fn str_at(m: &[u8], at: usize) -> String {
    let (p, n) = (u32_at(m, at) as usize, u32_at(m, at + 4) as usize);
    String::from_utf8(m[p..p + n].to_vec()).unwrap()
}
"#;

fn escaped(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("\\{b:02x}")).collect()
}

/// A `(data ...)` segment of little-endian `u32`s at `at`.
fn words(at: u32, values: &[u32]) -> String {
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    format!(r#"(data (i32.const {at}) "{}")"#, escaped(&bytes))
}

fn text(at: u32, s: &str) -> String {
    format!(r#"(data (i32.const {at}) "{}")"#, escaped(s.as_bytes()))
}

fn go(name: &str, body: &str, imports: &str, guest: &str, host: &str) {
    run(
        name,
        body,
        &format!("(module {imports} {MEM} {guest})"),
        &format!("{HELP}\n{host}"),
    );
}

fn go_panics(name: &str, body: &str, imports: &str, guest: &str, host: &str, needle: &str) {
    run_panics(
        name,
        body,
        &format!("(module {imports} {MEM} {guest})"),
        &format!("{HELP}\n{host}"),
        needle,
    );
}

/// A `main` that only runs the guest's `run` export (for tests expecting a panic).
const RUN_MAIN: &str = "fn main() { Instance::new(Glue(H)).run(); }";

const SHOUT: &str = r#"(export "shout" (func (param "s" string) (result string)))"#;
const SHOUT_IMPORT: &str = r#"(import "a:b/c" "shout" (func $shout (param i32 i32 i32)))"#;
const SHOUT_HOST: &str = r#"
struct H;
impl Host for H {
    fn a_b_c_shout(&mut self, s: String) -> String { s.to_uppercase() }
}
"#;

#[test]
fn a_string_param_is_lifted_and_a_string_result_is_allocated_in_the_guest() {
    go(
        "string",
        SHOUT,
        SHOUT_IMPORT,
        &format!(
            r#"{} (func (export "run") (call $shout (i32.const 16) (i32.const 5) (i32.const 64)))"#,
            text(16, "hello")
        ),
        &format!(
            "{SHOUT_HOST}
            fn main() {{
                let mut inst = Instance::new(Glue(H));
                inst.run();
                assert_eq!(str_at(inst.memory(), 64), \"HELLO\");
                // The result string lives where the guest's allocator put it, not at 16.
                assert!(u32_at(inst.memory(), 64) >= 1024);
            }}"
        ),
    );
}

#[test]
fn the_glue_signature_has_a_trailing_return_pointer_and_no_result() {
    let glue = wit_host_glue(&component(SHOUT)).unwrap();
    assert!(
        glue.contains(
            "fn a_b_c_shout(&mut self, g: &mut Instance<Self>, p0: i32, p1: i32, p2: i32) {"
        ),
        "{glue}"
    );
}

#[test]
fn an_empty_string_and_an_empty_list_cross() {
    go(
        "empty",
        &format!(r#"{SHOUT} (export "nums" (func (param "l" (list u32)) (result (list u32))))"#),
        &format!(
            r#"{SHOUT_IMPORT}
            (import "a:b/c" "nums" (func $nums (param i32 i32 i32)))"#
        ),
        r#"(func (export "run")
             (call $shout (i32.const 0) (i32.const 0) (i32.const 64))
             (call $nums (i32.const 0) (i32.const 0) (i32.const 80)))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_shout(&mut self, s: String) -> String { assert!(s.is_empty()); s }
            fn a_b_c_nums(&mut self, l: Vec<u32>) -> Vec<u32> { assert!(l.is_empty()); l }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.run();
            let m = inst.memory();
            assert_eq!((u32_at(m, 68), u32_at(m, 84)), (0, 0));
        }
        "#,
    );
}

#[test]
fn list_u8_and_list_of_scalars_round_trip() {
    go(
        "list_u8",
        r#"(export "shift" (func (param "b" (list u8)) (result (list u8))))
        (export "wide" (func (param "w" (list u64)) (result (list s16))))"#,
        r#"(import "a:b/c" "shift" (func $shift (param i32 i32 i32)))
        (import "a:b/c" "wide" (func $wide (param i32 i32 i32)))"#,
        &format!(
            r#"(data (i32.const 16) "\01\02\ff")
            {}
            (func (export "run")
              (call $shift (i32.const 16) (i32.const 3) (i32.const 64))
              (call $wide (i32.const 32) (i32.const 2) (i32.const 80)))"#,
            // two u64 at an 8-aligned address
            r#"(data (i32.const 32) "\01\00\00\00\00\00\00\00\02\00\00\00\00\00\00\00")"#
        ),
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_shift(&mut self, b: Vec<u8>) -> Vec<u8> {
                assert_eq!(b, vec![1, 2, 255]);
                b.into_iter().map(|x| x.wrapping_add(1)).collect()
            }
            fn a_b_c_wide(&mut self, w: Vec<u64>) -> Vec<i16> {
                assert_eq!(w, vec![1, 2]);
                vec![-1, 300, i16::MIN]
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.run();
            let m = inst.memory();
            let (p, n) = (u32_at(m, 64) as usize, u32_at(m, 68));
            assert_eq!((&m[p..p + 3], n), (&[2u8, 3, 0][..], 3));
            let (p, n) = (u32_at(m, 80) as usize, u32_at(m, 84));
            assert_eq!(n, 3);
            assert_eq!(p % 2, 0, "list<s16> is 2-aligned");
            let got: Vec<i16> = (0..3)
                .map(|i| i16::from_le_bytes([m[p + 2 * i], m[p + 2 * i + 1]]))
                .collect();
            assert_eq!(got, vec![-1, 300, i16::MIN]);
        }
        "#,
    );
}

#[test]
fn list_of_string_is_read_and_written_as_eight_byte_elements() {
    go(
        "list_string",
        r#"(export "rev" (func (param "parts" (list string)) (result (list string))))"#,
        r#"(import "a:b/c" "rev" (func $rev (param i32 i32 i32)))"#,
        &format!(
            r#"{} {} {}
            (func (export "run") (call $rev (i32.const 32) (i32.const 2) (i32.const 200)))"#,
            words(32, &[100, 2, 110, 3]),
            text(100, "ab"),
            text(110, "cde"),
        ),
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_rev(&mut self, parts: Vec<String>) -> Vec<String> {
                assert_eq!(parts, vec!["ab".to_string(), "cde".to_string()]);
                parts.iter().rev().map(|s| format!("{s}!")).collect()
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.run();
            let m = inst.memory();
            let (p, n) = (u32_at(m, 200) as usize, u32_at(m, 204));
            assert_eq!(n, 2);
            assert_eq!(str_at(m, p), "cde!");
            assert_eq!(str_at(m, p + 8), "ab!");
        }
        "#,
    );
}

#[test]
fn list_of_record_uses_the_padded_element_size() {
    go(
        "list_record",
        r#"(type $p (record (field "x" u8) (field "y" u32)))
        (export "point" (type $pt (eq $p)))
        (export "bump" (func (param "ps" (list $pt)) (result (list $pt))))"#,
        r#"(import "a:b/c" "bump" (func $bump (param i32 i32 i32)))"#,
        &format!(
            r#"{} (func (export "run") (call $bump (i32.const 32) (i32.const 2) (i32.const 64)))"#,
            words(32, &[1, 10, 2, 20])
        ),
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_bump(&mut self, ps: Vec<Point>) -> Vec<Point> {
                assert_eq!((ps[0].x, ps[0].y, ps[1].x, ps[1].y), (1, 10, 2, 20));
                ps.into_iter().map(|p| Point { x: p.x + 1, y: p.y * 2 }).collect()
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.run();
            let m = inst.memory();
            let p = u32_at(m, 64) as usize;
            assert_eq!(u32_at(m, 68), 2);
            assert_eq!((m[p], u32_at(m, p + 4)), (2, 20));
            assert_eq!((m[p + 8], u32_at(m, p + 12)), (3, 40));
        }
        "#,
    );
}

#[test]
fn list_of_list_of_u32_nests() {
    go(
        "list_list",
        r#"(export "scale" (func (param "l" (list (list u32))) (result (list (list u32)))))"#,
        r#"(import "a:b/c" "scale" (func $scale (param i32 i32 i32)))"#,
        &format!(
            r#"{} {} {}
            (func (export "run") (call $scale (i32.const 32) (i32.const 2) (i32.const 200)))"#,
            words(32, &[100, 2, 120, 1]),
            words(100, &[1, 2]),
            words(120, &[3]),
        ),
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_scale(&mut self, l: Vec<Vec<u32>>) -> Vec<Vec<u32>> {
                assert_eq!(l, vec![vec![1, 2], vec![3]]);
                l.into_iter().map(|v| v.into_iter().map(|x| x * 10).collect()).collect()
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.run();
            let m = inst.memory();
            let (p, n) = (u32_at(m, 200) as usize, u32_at(m, 204));
            assert_eq!(n, 2);
            let inner = |at: usize| -> Vec<u32> {
                let (q, k) = (u32_at(m, at) as usize, u32_at(m, at + 4) as usize);
                (0..k).map(|i| u32_at(m, q + 4 * i)).collect()
            };
            assert_eq!(inner(p), vec![10, 20]);
            assert_eq!(inner(p + 8), vec![30]);
        }
        "#,
    );
}

#[test]
fn option_string_is_lifted_from_flat_and_lowered_to_memory() {
    go(
        "option_string",
        r#"(export "maybe" (func (param "s" (option string)) (result (option string))))"#,
        r#"(import "a:b/c" "maybe" (func $maybe (param i32 i32 i32 i32)))"#,
        &format!(
            r#"{}
            (func (export "some") (call $maybe (i32.const 1) (i32.const 16) (i32.const 2) (i32.const 64)))
            (func (export "none") (call $maybe (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 96)))"#,
            text(16, "hi")
        ),
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_maybe(&mut self, s: Option<String>) -> Option<String> {
                s.map(|s| format!("{s}!"))
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.some();
            inst.none();
            let m = inst.memory();
            assert_eq!(m[64], 1);
            assert_eq!(str_at(m, 68), "hi!", "the payload sits at offset 4");
            assert_eq!(m[96], 0);
        }
        "#,
    );
}

#[test]
fn result_with_a_string_and_an_enum_error_is_written_at_the_payload_offset() {
    go(
        "result_string_enum",
        r#"(type $f (enum "bad" "worse"))
        (export "fault" (type $fault (eq $f)))
        (export "parse" (func (param "n" u32) (result (result string (error $fault)))))
        (type $r2 (result (error string)))
        (export "check" (func (param "n" u32) (result $r2)))"#,
        r#"(import "a:b/c" "parse" (func $parse (param i32 i32)))
        (import "a:b/c" "check" (func $check (param i32 i32)))"#,
        r#"(func (export "parse") (param i32 i32) (call $parse (local.get 0) (local.get 1)))
        (func (export "check") (param i32 i32) (call $check (local.get 0) (local.get 1)))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_parse(&mut self, n: u32) -> Result<String, Fault> {
                match n { 0 => Ok("zero".into()), 1 => Err(Fault::Bad), _ => Err(Fault::Worse) }
            }
            fn a_b_c_check(&mut self, n: u32) -> Result<(), String> {
                if n == 0 { Ok(()) } else { Err("nope".into()) }
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.parse(0, 64);
            inst.parse(1, 96);
            inst.parse(2, 128);
            inst.check(0, 160);
            inst.check(5, 192);
            let m = inst.memory();
            assert_eq!(m[64], 0);
            assert_eq!(str_at(m, 68), "zero");
            assert_eq!((m[96], m[100]), (1, 0));
            assert_eq!((m[128], m[132]), (1, 1));
            assert_eq!(m[160], 0);
            assert_eq!(m[192], 1);
            assert_eq!(str_at(m, 196), "nope");
        }
        "#,
    );
}

#[test]
fn records_with_strings_cross_flat_and_through_a_return_pointer() {
    go(
        "record_strings",
        r#"(type $p (record (field "name" string) (field "age" u8) (field "nick" string)))
        (export "person" (type $pe (eq $p)))
        (export "swap" (func (param "p" $pe) (result $pe)))"#,
        r#"(import "a:b/c" "swap" (func $swap (param i32 i32 i32 i32 i32 i32)))"#,
        &format!(
            r#"{} {}
            (func (export "run")
              (call $swap (i32.const 16) (i32.const 3) (i32.const 41) (i32.const 24) (i32.const 2)
                          (i32.const 64)))"#,
            text(16, "Bob"),
            text(24, "bo"),
        ),
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_swap(&mut self, p: Person) -> Person {
                assert_eq!((p.name.as_str(), p.age, p.nick.as_str()), ("Bob", 41, "bo"));
                Person { name: p.nick, age: p.age + 1, nick: p.name }
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.run();
            let m = inst.memory();
            assert_eq!(str_at(m, 64), "bo");
            assert_eq!(m[72], 42);
            assert_eq!(str_at(m, 76), "Bob");
        }
        "#,
    );
}

#[test]
fn a_variant_payload_sits_at_the_alignment_of_its_widest_case() {
    // disc u8, payload at 8 (the u64 case), 16 bytes.
    let body = r#"(type $s (variant (case "small" u8) (case "big" u64) (case "named" string) (case "nothing")))
        (export "shape" (type $sh (eq $s)))
        (export "make" (func (param "n" u32) (result $sh)))
        (export "total" (func (param "l" (list $sh)) (result u64)))"#;
    go(
        "variant_offsets",
        body,
        r#"(import "a:b/c" "make" (func $make (param i32 i32)))
        (import "a:b/c" "total" (func $total (param i32 i32) (result i64)))"#,
        &format!(
            r#"(func (export "make") (param i32 i32) (call $make (local.get 0) (local.get 1)))
            (func (export "total") (result i64) (call $total (i32.const 32) (i32.const 3)))
            {} {} {} {} {} {} {}"#,
            // elem 0 at 32: small(7); elem 1 at 48: big(1 << 40); elem 2 at 64: named("abc")
            r#"(data (i32.const 32) "\00")"#,
            r#"(data (i32.const 40) "\07")"#,
            r#"(data (i32.const 48) "\01")"#,
            r#"(data (i32.const 56) "\00\00\00\00\00\01\00\00")"#,
            r#"(data (i32.const 64) "\02")"#,
            words(72, &[100, 3]),
            text(100, "abc"),
        ),
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_make(&mut self, n: u32) -> Shape {
                match n {
                    0 => Shape::Small(200),
                    1 => Shape::Big(0x0102_0304_0506_0708),
                    2 => Shape::Named("xyz".into()),
                    _ => Shape::Nothing,
                }
            }
            fn a_b_c_total(&mut self, l: Vec<Shape>) -> u64 {
                l.iter().map(|s| match s {
                    Shape::Small(n) => u64::from(*n),
                    Shape::Big(n) => *n,
                    Shape::Named(s) => s.len() as u64,
                    Shape::Nothing => 0,
                }).sum()
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            assert_eq!(inst.total() as u64, 7 + (1u64 << 40) + 3);
            for (n, at) in [(0u32, 256), (1, 288), (2, 320), (3, 352)] {
                inst.make(n as i32, at);
            }
            let m = inst.memory();
            assert_eq!((m[256], m[264]), (0, 200));
            assert_eq!((m[288], u64_at(m, 296)), (1, 0x0102_0304_0506_0708));
            assert_eq!((m[320], str_at(m, 328)), (2, "xyz".to_string()));
            assert_eq!(m[352], 3);
        }
        "#,
    );
}

#[test]
fn flags_are_stored_and_loaded_with_their_memory_width() {
    let flags = |n: usize| {
        (0..n)
            .map(|i| format!(r#""f{i}""#))
            .collect::<Vec<_>>()
            .join(" ")
    };
    go(
        "flags_mem",
        &format!(
            r#"(type $s (flags {a}))
            (export "small" (type $small (eq $s)))
            (type $m (flags {b}))
            (export "medium" (type $medium (eq $m)))
            (type $p (record (field "f" $small) (field "m" $medium) (field "x" u32)))
            (export "mixed" (type $mx (eq $p)))
            (export "tally" (func (param "a" (list $small)) (param "b" (list $medium)) (result u32)))
            (export "mk" (func (param "x" u32) (result $mx)))"#,
            a = flags(3),
            b = flags(12),
        ),
        r#"(import "a:b/c" "tally" (func $tally (param i32 i32 i32 i32) (result i32)))
        (import "a:b/c" "mk" (func $mk (param i32 i32)))"#,
        r#"(data (i32.const 16) "\01\06")
        (data (i32.const 32) "\01\08\02\00")
        (func (export "tally") (result i32)
          (call $tally (i32.const 16) (i32.const 2) (i32.const 32) (i32.const 2)))
        (func (export "mk") (param i32 i32) (call $mk (local.get 0) (local.get 1)))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_tally(&mut self, a: Vec<Small>, b: Vec<Medium>) -> u32 {
                a.iter().map(|f| u32::from(f.0)).sum::<u32>()
                    + b.iter().map(|f| u32::from(f.0)).sum::<u32>() * 100
            }
            fn a_b_c_mk(&mut self, x: u32) -> Mixed {
                Mixed { f: Small(0b101), m: Medium(0x801), x }
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            assert_eq!(inst.tally(), 7 + (0x801 + 2) * 100);
            inst.mk(9, 64);
            let m = inst.memory();
            // f at 0 (1 byte), m at 2 (2 bytes), x at 4.
            assert_eq!(m[64], 0b101);
            assert_eq!(u16::from_le_bytes([m[66], m[67]]), 0x801);
            assert_eq!(u32_at(m, 68), 9);
        }
        "#,
    );
}

#[test]
fn seventeen_flat_params_are_passed_as_one_pointer_to_a_record() {
    let params: String = (0..17).map(|i| format!(r#"(param "p{i}" u32)"#)).collect();
    let host_params = (0..17)
        .map(|i| format!("p{i}: u32"))
        .collect::<Vec<_>>()
        .join(", ");
    let sum = (0..17)
        .map(|i| format!("p{i}"))
        .collect::<Vec<_>>()
        .join(" + ");
    let values: Vec<u32> = (1..=17).collect();
    let glue = wit_host_glue(&component(&format!(
        r#"(export "wider" (func {params} (result u32)))"#
    )))
    .unwrap();
    assert!(
        glue.contains("fn a_b_c_wider(&mut self, g: &mut Instance<Self>, p0: i32) -> i32 {"),
        "{glue}"
    );
    go(
        "spill_params",
        &format!(r#"(export "wider" (func {params} (result u32)))"#),
        r#"(import "a:b/c" "wider" (func $w (param i32) (result i32)))"#,
        &format!(
            r#"{} (func (export "run") (result i32) (call $w (i32.const 32)))"#,
            words(32, &values)
        ),
        &format!(
            "struct H;
            impl Host for H {{
                fn a_b_c_wider(&mut self, {host_params}) -> u32 {{ {sum} }}
            }}
            fn main() {{
                let mut inst = Instance::new(Glue(H));
                assert_eq!(inst.run(), 153);
            }}"
        ),
    );
    go_panics(
        "spill_params_unaligned",
        &format!(r#"(export "wider" (func {params} (result u32)))"#),
        r#"(import "a:b/c" "wider" (func $w (param i32) (result i32)))"#,
        r#"(func (export "run") (result i32) (call $w (i32.const 34)))"#,
        &format!(
            "struct H;
            impl Host for H {{
                fn a_b_c_wider(&mut self, {host_params}) -> u32 {{ {sum} }}
            }}
            fn main() {{ Instance::new(Glue(H)).run(); }}"
        ),
        "unaligned",
    );
}

#[test]
fn nine_strings_spill_their_eighteen_flat_values_to_memory() {
    let params: String = (0..9)
        .map(|i| format!(r#"(param "s{i}" string)"#))
        .collect();
    let host_params = (0..9)
        .map(|i| format!("s{i}: String"))
        .collect::<Vec<_>>()
        .join(", ");
    let joined = (0..9)
        .map(|i| format!("s{i}.as_str()"))
        .collect::<Vec<_>>()
        .join(", ");
    let pairs: Vec<u32> = (0..9).flat_map(|_| [16, 2]).collect();
    go(
        "spill_strings",
        &format!(r#"(export "many" (func {params} (result u32)))"#),
        r#"(import "a:b/c" "many" (func $many (param i32) (result i32)))"#,
        &format!(
            r#"{} {} (func (export "run") (result i32) (call $many (i32.const 64)))"#,
            text(16, "ab"),
            words(64, &pairs)
        ),
        &format!(
            "struct H;
            impl Host for H {{
                fn a_b_c_many(&mut self, {host_params}) -> u32 {{
                    [{joined}].concat().len() as u32
                }}
            }}
            fn main() {{ assert_eq!(Instance::new(Glue(H)).run(), 18); }}"
        ),
    );
}

#[test]
fn a_result_of_several_flat_values_is_returned_through_the_return_pointer() {
    go(
        "retptr_record",
        r#"(type $t (record (field "a" u32) (field "b" u8) (field "c" u64)))
        (export "triple" (type $tr (eq $t)))
        (export "mk" (func (param "n" u32) (result $tr)))"#,
        r#"(import "a:b/c" "mk" (func $mk (param i32 i32)))"#,
        r#"(func (export "run") (param i32) (call $mk (local.get 0) (i32.const 64)))
        (func (export "run_unaligned") (call $mk (i32.const 0) (i32.const 68)))
        (func (export "run_oob") (call $mk (i32.const 0) (i32.const 65528)))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_mk(&mut self, n: u32) -> Triple { Triple { a: n, b: 7, c: u64::MAX } }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.run(5);
            let m = inst.memory();
            // a at 0, b at 4, c at 8; 16 bytes, align 8.
            assert_eq!((u32_at(m, 64), m[68], u64_at(m, 72)), (5, 7, u64::MAX));
        }
        "#,
    );
    let host = r#"
        struct H;
        impl Host for H {
            fn a_b_c_mk(&mut self, n: u32) -> Triple { Triple { a: n, b: 7, c: 1 } }
        }
    "#;
    let body = r#"(type $t (record (field "a" u32) (field "b" u8) (field "c" u64)))
        (export "triple" (type $tr (eq $t)))
        (export "mk" (func (param "n" u32) (result $tr)))"#;
    let imports = r#"(import "a:b/c" "mk" (func $mk (param i32 i32)))"#;
    let guest = r#"(func (export "run_unaligned") (call $mk (i32.const 0) (i32.const 68)))
        (func (export "run_oob") (call $mk (i32.const 0) (i32.const 65528)))"#;
    go_panics(
        "retptr_unaligned",
        body,
        imports,
        guest,
        &format!("{host} fn main() {{ Instance::new(Glue(H)).run_unaligned(); }}"),
        "unaligned",
    );
    go_panics(
        "retptr_oob",
        body,
        imports,
        guest,
        &format!("{host} fn main() {{ Instance::new(Glue(H)).run_oob(); }}"),
        "out of bounds",
    );
}

#[test]
fn a_list_of_string_pairs_mirrors_get_environment() {
    go(
        "environment",
        r#"(export "get-environment" (func (result (list (tuple string string)))))"#,
        r#"(import "a:b/c" "get-environment" (func $env (param i32)))"#,
        r#"(func (export "run") (call $env (i32.const 64)))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_get_environment(&mut self) -> Vec<(String, String)> {
                vec![("HOME".into(), "/root".into()), ("K".into(), String::new())]
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.run();
            let m = inst.memory();
            let (p, n) = (u32_at(m, 64) as usize, u32_at(m, 68));
            assert_eq!(n, 2);
            assert_eq!((str_at(m, p), str_at(m, p + 8)), ("HOME".into(), "/root".to_string()));
            assert_eq!((str_at(m, p + 16), str_at(m, p + 24)), ("K".into(), String::new()));
        }
        "#,
    );
}

#[test]
fn three_levels_of_nesting_round_trip() {
    // list<tuple<string, list<option<string>>>>
    go(
        "deep",
        r#"(export "deep" (func (param "x" (list (tuple string (list (option string)))))
                                (result (list (tuple string (list (option string)))))))"#,
        r#"(import "a:b/c" "deep" (func $deep (param i32 i32 i32)))"#,
        &format!(
            r#"{} {} {} {} {}
            (func (export "run") (call $deep (i32.const 32) (i32.const 1) (i32.const 300)))"#,
            // the one tuple at 32: ("k" at 200, list of 2 at 48)
            words(32, &[200, 1, 48, 2]),
            // two option<string> at 48 (12 bytes each): Some("v" at 210), None
            words(48, &[1, 210, 1, 0, 0, 0]),
            text(200, "k"),
            text(210, "v"),
            "",
        ),
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_deep(
                &mut self,
                x: Vec<(String, Vec<Option<String>>)>,
            ) -> Vec<(String, Vec<Option<String>>)> {
                assert_eq!(x, vec![("k".to_string(), vec![Some("v".to_string()), None])]);
                x.into_iter()
                    .map(|(k, l)| {
                        let l = l.into_iter().map(|o| Some(o.map_or("none".into(), |s| s.to_uppercase()))).collect();
                        (k.to_uppercase(), l)
                    })
                    .collect()
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.run();
            let m = inst.memory();
            let (p, n) = (u32_at(m, 300) as usize, u32_at(m, 304));
            assert_eq!(n, 1);
            assert_eq!(str_at(m, p), "K");
            let (q, k) = (u32_at(m, p + 8) as usize, u32_at(m, p + 12));
            assert_eq!(k, 2);
            assert_eq!((m[q], str_at(m, q + 4)), (1, "V".to_string()));
            assert_eq!((m[q + 12], str_at(m, q + 16)), (1, "none".to_string()));
        }
        "#,
    );
}

#[test]
fn resource_handles_in_a_list_are_four_byte_elements() {
    go(
        "handles",
        r#"(export "r" (type $r (sub resource)))
        (export "take" (func (param "hs" (list (own $r))) (result u32)))
        (export "give" (func (result (list (own $r)))))"#,
        r#"(import "a:b/c" "take" (func $take (param i32 i32) (result i32)))
        (import "a:b/c" "give" (func $give (param i32)))
        (import "a:b/c" "[resource-drop]r" (func (param i32)))"#,
        &format!(
            r#"{} (func (export "take") (result i32) (call $take (i32.const 32) (i32.const 2)))
            (func (export "give") (call $give (i32.const 64)))"#,
            words(32, &[5, 6])
        ),
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_take(&mut self, hs: Vec<R>) -> u32 { hs.iter().map(|h| h.0).sum() }
            fn a_b_c_give(&mut self) -> Vec<R> { vec![R(40), R(41), R(42)] }
            fn a_b_c_resource_drop_r(&mut self, _h: R) {}
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            assert_eq!(inst.take(), 11);
            inst.give();
            let m = inst.memory();
            let p = u32_at(m, 64) as usize;
            assert_eq!(u32_at(m, 68), 3);
            assert_eq!((u32_at(m, p), u32_at(m, p + 4), u32_at(m, p + 8)), (40, 41, 42));
        }
        "#,
    );
}

#[test]
fn an_invalid_utf8_string_traps() {
    go_panics(
        "bad_utf8",
        SHOUT,
        SHOUT_IMPORT,
        r#"(data (i32.const 16) "\ff\fe")
        (func (export "run") (call $shout (i32.const 16) (i32.const 2) (i32.const 64)))"#,
        &format!("{SHOUT_HOST} {RUN_MAIN}"),
        "invalid UTF-8",
    );
}

#[test]
fn a_string_outside_guest_memory_traps() {
    go_panics(
        "string_oob",
        SHOUT,
        SHOUT_IMPORT,
        r#"(func (export "run") (call $shout (i32.const 65530) (i32.const 100) (i32.const 64)))"#,
        &format!("{SHOUT_HOST} {RUN_MAIN}"),
        "out of bounds",
    );
    go_panics(
        "string_wrap",
        SHOUT,
        SHOUT_IMPORT,
        // ptr + len wraps past 2^32 as 32-bit arithmetic but not as the spec's integers.
        r#"(func (export "run") (call $shout (i32.const -4) (i32.const 8) (i32.const 64)))"#,
        &format!("{SHOUT_HOST} {RUN_MAIN}"),
        "out of bounds",
    );
}

#[test]
fn a_list_with_an_unaligned_pointer_or_too_many_elements_traps() {
    let body = r#"(export "nums" (func (param "l" (list u32)) (result u32)))"#;
    let imports = r#"(import "a:b/c" "nums" (func $nums (param i32 i32) (result i32)))"#;
    let host = r#"
        struct H;
        impl Host for H { fn a_b_c_nums(&mut self, l: Vec<u32>) -> u32 { l.len() as u32 } }
        fn main() { Instance::new(Glue(H)).run(); }
    "#;
    go_panics(
        "list_unaligned",
        body,
        imports,
        r#"(func (export "run") (result i32) (call $nums (i32.const 34) (i32.const 2)))"#,
        host,
        "unaligned",
    );
    go_panics(
        "list_oob",
        body,
        imports,
        // 0x4000_0000 elements of 4 bytes is 2^32 bytes: far more than memory holds.
        r#"(func (export "run") (result i32) (call $nums (i32.const 32) (i32.const 0x40000000)))"#,
        host,
        "lifted data too large",
    );
    go_panics(
        "list_end_oob",
        body,
        imports,
        // Two elements at 65532: the second one is past the end of memory.
        r#"(func (export "run") (result i32) (call $nums (i32.const 65532) (i32.const 2)))"#,
        host,
        "out of bounds",
    );
}

#[test]
fn an_invalid_discriminant_in_memory_traps() {
    go_panics(
        "disc_mem",
        r#"(export "opts" (func (param "l" (list (option u8))) (result u32)))"#,
        r#"(import "a:b/c" "opts" (func $opts (param i32 i32) (result i32)))"#,
        r#"(data (i32.const 32) "\02\00")
        (func (export "run") (result i32) (call $opts (i32.const 32) (i32.const 1)))"#,
        r#"
        struct H;
        impl Host for H { fn a_b_c_opts(&mut self, l: Vec<Option<u8>>) -> u32 { l.len() as u32 } }
        fn main() { Instance::new(Glue(H)).run(); }
        "#,
        "invalid discriminant",
    );
}

const TAKE_STRINGS: &str = r#"(export "take" (func (param "l" (list string)) (result u32)))"#;
const TAKE_IMPORT: &str = r#"(import "a:b/c" "take" (func $take (param i32 i32) (result i32)))"#;
const TAKE_STRINGS_HOST: &str = r#"
struct H;
impl Host for H {
    fn a_b_c_take(&mut self, l: Vec<String>) -> u32 { l.iter().map(|s| s.len() as u32).sum() }
}
fn main() { println!("{}", Instance::new(Glue(H)).run()); }
"#;

#[test]
fn many_descriptors_aliasing_all_of_memory_exceed_the_lift_budget_fast() {
    // 8192 strings that each cover the whole 64 KiB memory: 512 MiB if copied.
    let start = std::time::Instant::now();
    go_panics(
        "alias_strings",
        TAKE_STRINGS,
        TAKE_IMPORT,
        r#"(func (export "run") (result i32) (local $i i32)
            (loop $l
              (i32.store offset=4 (i32.mul (local.get $i) (i32.const 8)) (i32.const 65536))
              (local.set $i (i32.add (local.get $i) (i32.const 1)))
              (br_if $l (i32.lt_u (local.get $i) (i32.const 8192))))
            (call $take (i32.const 0) (i32.const 8192)))"#,
        TAKE_STRINGS_HOST,
        "lifted data too large",
    );
    // Build and run included: the budget refuses before anything is copied.
    assert!(start.elapsed() < std::time::Duration::from_secs(60));
}

#[test]
fn nested_lists_cannot_amplify_either() {
    // list<list<string>>: outer entries alias one inner array whose strings alias memory.
    go_panics(
        "alias_nested",
        r#"(export "take" (func (param "l" (list (list string))) (result u32)))"#,
        TAKE_IMPORT,
        // 64 inner descriptors (ptr 4096, len 64 at 0..) and 64 outer ones at 1024 pointing to it.
        r#"(func (export "run") (result i32) (local $i i32)
            (loop $l
              (i32.store offset=4 (i32.add (i32.const 4096) (i32.mul (local.get $i) (i32.const 8))) (i32.const 1000))
              (i32.store (i32.add (i32.const 1024) (i32.mul (local.get $i) (i32.const 8))) (i32.const 4096))
              (i32.store offset=4 (i32.add (i32.const 1024) (i32.mul (local.get $i) (i32.const 8))) (i32.const 64))
              (local.set $i (i32.add (local.get $i) (i32.const 1)))
              (br_if $l (i32.lt_u (local.get $i) (i32.const 64))))
            (call $take (i32.const 1024) (i32.const 64)))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_take(&mut self, l: Vec<Vec<String>>) -> u32 { l.len() as u32 }
        }
        fn main() { Instance::new(Glue(H)).run(); }
        "#,
        "lifted data too large",
    );
}

#[test]
fn honest_large_lists_of_strings_still_round_trip() {
    // 4 strings of 8000 bytes each, all different places: 32000 bytes < 64 KiB.
    let descriptors = words(0, &[1024, 8000, 9100, 8000, 18200, 8000, 27300, 8000]);
    go(
        "honest_big",
        TAKE_STRINGS,
        TAKE_IMPORT,
        &format!(
            r#"{descriptors} (func (export "run") (result i32) (call $take (i32.const 0) (i32.const 4)))"#
        ),
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_take(&mut self, l: Vec<String>) -> u32 { l.iter().map(|s| s.len() as u32).sum() }
        }
        fn main() { assert_eq!(Instance::new(Glue(H)).run(), 32000); }
        "#,
    );
}

#[test]
fn a_call_exactly_at_the_cap_passes_and_one_byte_over_fails() {
    // The cap is the memory length, 65536 = 9 (the list: 1 element + 8 descriptor bytes) +
    // 65527 (the string, at address 0).
    let exact = |len: u32, name: &str, ok: bool| {
        let guest = format!(
            r#"{} (func (export "run") (result i32) (call $take (i32.const 65528) (i32.const 1)))"#,
            words(65_528, &[0, len])
        );
        if ok {
            go(
                name,
                TAKE_STRINGS,
                TAKE_IMPORT,
                &guest,
                TAKE_STRINGS_HOST_OK,
            );
        } else {
            go_panics(
                name,
                TAKE_STRINGS,
                TAKE_IMPORT,
                &guest,
                TAKE_STRINGS_HOST_OK,
                "lifted data too large",
            );
        }
    };
    exact(65_527, "cap_exact", true);
    exact(65_528, "cap_over", false);
}

const TAKE_STRINGS_HOST_OK: &str = r#"
struct H;
impl Host for H {
    fn a_b_c_take(&mut self, l: Vec<String>) -> u32 { l[0].len() as u32 }
}
fn main() { assert_eq!(Instance::new(Glue(H)).run(), 65527); }
"#;

#[test]
fn a_list_with_a_four_gigabyte_length_traps_fast() {
    go_panics(
        "zero_sized",
        // (A WIT type cannot have size 0, so the element count and the byte count only differ
        // by the element size; the count is spent as well.)
        r#"(export "take" (func (param "l" (list bool)) (result u32)))"#,
        TAKE_IMPORT,
        r#"(func (export "run") (result i32) (call $take (i32.const 0) (i32.const -1)))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_take(&mut self, l: Vec<bool>) -> u32 { l.len() as u32 }
        }
        fn main() { Instance::new(Glue(H)).run(); }
        "#,
        "lifted data too large",
    );
}

#[test]
fn the_discriminant_is_one_byte_and_the_payload_sits_at_its_own_alignment() {
    // option<u8>: disc 1 byte, payload at 1, size 2. variant{a(u8), b(u16)}: disc 1 byte,
    // payload at 2 (the u16's alignment), size 4. The list strides expose the sizes.
    go(
        "disc_small",
        r#"(type $v (variant (case "a" u8) (case "b" u16)))
        (export "ab" (type $ab (eq $v)))
        (export "opts" (func (param "l" (list (option u8))) (result (list (option u8)))))
        (export "abs" (func (param "l" (list $ab)) (result (list $ab))))"#,
        r#"(import "a:b/c" "opts" (func $opts (param i32 i32 i32)))
        (import "a:b/c" "abs" (func $abs (param i32 i32 i32)))"#,
        // opts at 16: Some(5), None (garbage payload), Some(9). abs at 32: a(7), b(0x0102),
        // with garbage in the padding bytes.
        r#"(data (i32.const 16) "\01\05\00\77\01\09")
        (data (i32.const 32) "\00\ee\07\ee\01\ee\02\01")
        (func (export "run")
          (call $opts (i32.const 16) (i32.const 3) (i32.const 64))
          (call $abs (i32.const 32) (i32.const 2) (i32.const 80)))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_opts(&mut self, l: Vec<Option<u8>>) -> Vec<Option<u8>> {
                assert_eq!(l, vec![Some(5), None, Some(9)]);
                vec![Some(0xAB), None, Some(0xCD)]
            }
            fn a_b_c_abs(&mut self, l: Vec<Ab>) -> Vec<Ab> {
                assert_eq!(l, vec![Ab::A(7), Ab::B(0x0102)]);
                vec![Ab::B(0x0304), Ab::A(0x55)]
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.run();
            let m = inst.memory();
            let (p, n) = (u32_at(m, 64) as usize, u32_at(m, 68));
            assert_eq!(n, 3);
            // [disc, payload] per element; the None payload is not written.
            assert_eq!((m[p], m[p + 1]), (1, 0xAB));
            assert_eq!(m[p + 2], 0);
            assert_eq!((m[p + 4], m[p + 5]), (1, 0xCD));
            let (p, n) = (u32_at(m, 80) as usize, u32_at(m, 84));
            assert_eq!(n, 2);
            assert_eq!((m[p], m[p + 1]), (1, 0)); // b: disc byte, payload from p + 2
            assert_eq!((m[p + 2], m[p + 3]), (0x04, 0x03));
            assert_eq!((m[p + 4], m[p + 6]), (0, 0x55)); // a, element stride 4
        }
        "#,
    );
}

#[test]
fn a_257_case_enum_and_variant_use_a_two_byte_discriminant() {
    let cases: String = (0..257).map(|i| format!(r#" "c{i}""#)).collect();
    let vcases: String = (0..256).map(|i| format!(r#" (case "c{i}")"#)).collect();
    go(
        "disc_257",
        &format!(
            r#"(type $e (enum{cases}))
            (export "big" (type $big (eq $e)))
            (type $v (variant{vcases} (case "last" u8)))
            (export "wide" (type $wide (eq $v)))
            (export "enums" (func (param "l" (list $big)) (result (list $big))))
            (export "wides" (func (param "l" (list $wide)) (result (list $wide))))"#
        ),
        r#"(import "a:b/c" "enums" (func $enums (param i32 i32 i32)))
        (import "a:b/c" "wides" (func $wides (param i32 i32 i32)))"#,
        // enums at 16: case 256 is bytes 00 01, then case 1 is 01 00 (2-byte elements).
        // wides at 32: last(0x42) is disc 00 01 then payload at 2; then c1 with garbage.
        r#"(data (i32.const 16) "\00\01\01\00")
        (data (i32.const 32) "\00\01\42\ee\01\00\ee\ee")
        (func (export "run")
          (call $enums (i32.const 16) (i32.const 2) (i32.const 64))
          (call $wides (i32.const 32) (i32.const 2) (i32.const 80)))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_enums(&mut self, l: Vec<Big>) -> Vec<Big> {
                assert_eq!(l, vec![Big::C256, Big::C1]);
                vec![Big::C256, Big::C0, Big::C255]
            }
            fn a_b_c_wides(&mut self, l: Vec<Wide>) -> Vec<Wide> {
                assert_eq!(l, vec![Wide::Last(0x42), Wide::C1]);
                vec![Wide::C255, Wide::Last(0x99)]
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.run();
            let m = inst.memory();
            let (p, n) = (u32_at(m, 64) as usize, u32_at(m, 68));
            assert_eq!(n, 3);
            assert_eq!(&m[p..p + 6], &[0x00, 0x01, 0x00, 0x00, 0xff, 0x00]);
            let (p, n) = (u32_at(m, 80) as usize, u32_at(m, 84));
            assert_eq!(n, 2);
            // disc (2 bytes), payload at 2, element stride 4.
            assert_eq!(&m[p..p + 2], &[0xff, 0x00]);
            assert_eq!(&m[p + 4..p + 7], &[0x00, 0x01, 0x99]);
        }
        "#,
    );
}

#[test]
fn signed_bytes_and_chars_are_stored_and_loaded_at_their_memory_widths() {
    go(
        "s8_char_mem",
        r#"(type $r (record (field "c" char) (field "b" s8)))
        (export "cb" (type $cb (eq $r)))
        (export "sb" (func (param "l" (list s8)) (result (list s8))))
        (export "cs" (func (param "l" (list char)) (result (list char))))
        (export "rec" (func (result $cb)))
        (export "tup" (func (result (tuple u32 u64))))"#,
        r#"(import "a:b/c" "sb" (func $sb (param i32 i32 i32)))
        (import "a:b/c" "cs" (func $cs (param i32 i32 i32)))
        (import "a:b/c" "rec" (func $rec (param i32)))
        (import "a:b/c" "tup" (func $tup (param i32)))"#,
        r#"(data (i32.const 16) "\ff\7f\80")
        (data (i32.const 32) "\e9\00\00\00\00\f6\01\00")
        (func (export "run")
          (call $sb (i32.const 16) (i32.const 3) (i32.const 64))
          (call $cs (i32.const 32) (i32.const 2) (i32.const 80))
          (call $rec (i32.const 96))
          (call $tup (i32.const 112)))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_sb(&mut self, l: Vec<i8>) -> Vec<i8> {
                assert_eq!(l, vec![-1, 127, -128]);
                vec![-1, 1, i8::MIN]
            }
            fn a_b_c_cs(&mut self, l: Vec<char>) -> Vec<char> {
                assert_eq!(l, vec!['\u{e9}', '\u{1F600}']);
                vec!['\u{e9}', 'a']
            }
            fn a_b_c_rec(&mut self) -> Cb { Cb { c: '\u{1F600}', b: -1 } }
            fn a_b_c_tup(&mut self) -> (u32, u64) { (0x0102_0304, 0x1112_1314_1516_1718) }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.run();
            let m = inst.memory();
            let (p, n) = (u32_at(m, 64) as usize, u32_at(m, 68));
            // The byte after the list is still zero: an s8 is stored as one byte, not as the flat i32.
            assert_eq!((&m[p..p + 4], n), (&[0xff, 0x01, 0x80, 0x00][..], 3));
            let (p, n) = (u32_at(m, 80) as usize, u32_at(m, 84));
            assert_eq!(n, 2);
            assert_eq!(&m[p..p + 8], &[0xe9, 0, 0, 0, 0x61, 0, 0, 0]);
            // record{c: char, b: s8}: c at 0, b at 4.
            assert_eq!(&m[96..104], &[0x00, 0xf6, 0x01, 0, 0xff, 0, 0, 0]);
            // tuple<u32, u64>: u32 at 0, u64 at 8.
            assert_eq!(u32_at(m, 112), 0x0102_0304);
            assert_eq!(u64_at(m, 120), 0x1112_1314_1516_1718);
        }
        "#,
    );
}

#[test]
fn a_scalar_is_loaded_with_exactly_its_memory_width() {
    // Each list sits at the very end of the one-page memory (65536 bytes), so a load wider
    // than the element would run past it and trap. The last two bytes are `05 01`.
    go(
        "mem_width_at_the_end",
        r#"(type $f3 (flags "a" "b" "c"))
        (export "f3" (type $f3t (eq $f3)))
        (type $f12 (flags "a" "b" "c" "d" "e" "f" "g" "h" "i" "j" "k" "l"))
        (export "f12" (type $f12t (eq $f12)))
        (export "s8s" (func (param "l" (list s8))))
        (export "bools" (func (param "l" (list bool))))
        (export "u16s" (func (param "l" (list u16))))
        (export "s16s" (func (param "l" (list s16))))
        (export "f3s" (func (param "l" (list $f3t))))
        (export "f12s" (func (param "l" (list $f12t))))"#,
        r#"(import "a:b/c" "s8s" (func $s8s (param i32 i32)))
        (import "a:b/c" "bools" (func $bools (param i32 i32)))
        (import "a:b/c" "u16s" (func $u16s (param i32 i32)))
        (import "a:b/c" "s16s" (func $s16s (param i32 i32)))
        (import "a:b/c" "f3s" (func $f3s (param i32 i32)))
        (import "a:b/c" "f12s" (func $f12s (param i32 i32)))"#,
        r#"(data (i32.const 65534) "\05\01")
        (func (export "run")
          (call $s8s (i32.const 65535) (i32.const 1))
          (call $bools (i32.const 65535) (i32.const 1))
          (call $u16s (i32.const 65534) (i32.const 1))
          (call $s16s (i32.const 65534) (i32.const 1))
          (call $f3s (i32.const 65535) (i32.const 1))
          (call $f12s (i32.const 65534) (i32.const 1)))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_s8s(&mut self, l: Vec<i8>) { assert_eq!(l, vec![1]); }
            fn a_b_c_bools(&mut self, l: Vec<bool>) { assert_eq!(l, vec![true]); }
            fn a_b_c_u16s(&mut self, l: Vec<u16>) { assert_eq!(l, vec![0x0105]); }
            fn a_b_c_s16s(&mut self, l: Vec<i16>) { assert_eq!(l, vec![0x0105]); }
            fn a_b_c_f3s(&mut self, l: Vec<F3>) { assert_eq!(l, vec![F3(1)]); }
            fn a_b_c_f12s(&mut self, l: Vec<F12>) { assert_eq!(l, vec![F12(0x105)]); }
        }
        fn main() { Instance::new(Glue(H)).run(); }
        "#,
    );
}

#[test]
fn a_bool_is_loaded_from_its_own_byte_and_stored_as_a_zero_or_one_byte() {
    // The byte after the `bool` at 16 is nonzero: a wider load would read `true`.
    go(
        "bool_mem",
        r#"(export "flags" (func (param "l" (list bool)) (result (list bool))))"#,
        r#"(import "a:b/c" "flags" (func $flags (param i32 i32 i32)))"#,
        r#"(data (i32.const 16) "\00\01\00\00")
        (func (export "run") (call $flags (i32.const 16) (i32.const 2) (i32.const 64)))"#,
        r#"
        struct H;
        impl Host for H {
            fn a_b_c_flags(&mut self, l: Vec<bool>) -> Vec<bool> {
                assert_eq!(l, vec![false, true]);
                vec![true, false, true]
            }
        }
        fn main() {
            let mut inst = Instance::new(Glue(H));
            inst.run();
            let m = inst.memory();
            let (p, n) = (u32_at(m, 64) as usize, u32_at(m, 68));
            assert_eq!((&m[p..p + 4], n), (&[1u8, 0, 1, 0][..], 3));
        }
        "#,
    );
}
