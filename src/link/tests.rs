#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use super::*;
use crate::{TranspileOptions, generate_from_ir, parse};
use std::process::Command;

fn wasm(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).expect("valid wat")
}

fn func(module: usize, index: u32) -> Source {
    Source::Func { module, index }
}

/// Parse every module, link them and render the Rust source.
fn linked_source(wasms: &[Vec<u8>], sources: Vec<ImportSources>) -> Result<String, TranspileError> {
    linked_source_with(wasms, sources, &TranspileOptions::default())
}

fn linked_source_with(
    wasms: &[Vec<u8>],
    sources: Vec<ImportSources>,
    topts: &TranspileOptions,
) -> Result<String, TranspileError> {
    let modules = wasms
        .iter()
        .zip(sources)
        .map(|(bytes, sources)| {
            Ok(LinkModule {
                ir: parse::parse_module(bytes, topts)?,
                sources,
                carried: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>, TranspileError>>()?;
    let rewritten = rewrite_bodies(&modules)?;
    let ir = link(modules, &rewritten)?;
    let mut code = None;
    generate_from_ir(ir, topts, |parts| {
        codegen::generate_module_split(parts, usize::MAX, usize::MAX, false, &mut |file| {
            if let crate::FileContent::Text(text) = file.content {
                code = Some(text);
            }
            Ok(())
        })
    })?;
    code.ok_or_else(|| TranspileError::Internal("no source emitted".into()))
}

/// Compile the linked program with `main_body` appended and require it to exit cleanly.
fn link_and_run(test: &str, wasms: &[Vec<u8>], sources: Vec<ImportSources>, main_body: &str) {
    let generated = linked_source(wasms, sources).expect("link and transpile");
    run_source(test, &generated, main_body);
}

fn run_source(test: &str, generated: &str, main_body: &str) {
    let dir = std::env::temp_dir().join(format!("wasm2rs_link_{test}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let src = dir.join("gen.rs");
    let bin = dir.join("gen");
    let program = format!("{generated}\nfn main() {{\n{main_body}\n}}\n");
    std::fs::write(&src, &program).expect("write source");
    let compile = Command::new("rustc")
        .current_dir(&dir)
        .args(["--edition", "2024", "-D", "warnings", "-C", "opt-level=0"])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        compile.status.success(),
        "linked code failed to compile:\n{program}\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&bin).status().expect("run binary");
    assert!(
        run.success(),
        "linked program assertions failed:\n{program}"
    );
}

fn link_error(wasms: &[Vec<u8>], sources: Vec<ImportSources>) -> String {
    match linked_source(wasms, sources) {
        Err(TranspileError::Unsupported(what)) => what,
        Err(other) => panic!("expected Unsupported, got {other}"),
        Ok(_) => panic!("expected the link to fail"),
    }
}

const MAIN_CALLS_B: &str = r#"(module
    (import "b" "g" (func $g (result i32)))
    (memory (export "memory") 1)
    (func (export "f") (result i32) i32.const 40)
    (func (export "_start") (result i32) call $g))"#;
const B_USES_A: &str = r#"(module
    (import "a" "f" (func $f (result i32)))
    (import "a" "memory" (memory 1))
    (func (export "g") (result i32)
      i32.const 0 i32.const 2 i32.store
      call $f
      i32.const 0 i32.load
      i32.add))"#;

fn two_module_sources() -> Vec<ImportSources> {
    vec![
        ImportSources {
            funcs: vec![func(1, 1)],
            ..ImportSources::default()
        },
        ImportSources {
            funcs: vec![func(0, 1)],
            memory: Some(Source::Memory {
                module: 0,
                index: 0,
            }),
            ..ImportSources::default()
        },
    ]
}

#[test]
fn a_main_import_answered_by_another_module_is_a_direct_call_sharing_its_memory() {
    // main: imports b.g (func 0), defines f (1) and _start (2); B calls main's f and
    // reads the memory main owns.
    link_and_run(
        "two_modules",
        &[wasm(MAIN_CALLS_B), wasm(B_USES_A)],
        two_module_sources(),
        "let mut inst = Instance::new();\n    assert_eq!(inst.func2(), 42);",
    );
}

#[test]
fn entry_exports_keep_the_function_an_aliased_import_reaches() {
    // `_start` is the only entry, and b's `g` is reachable only through main's alias.
    let topts = TranspileOptions::default().with_entry_exports(Some(vec!["_start".into()]));
    let generated = linked_source_with(
        &[wasm(MAIN_CALLS_B), wasm(B_USES_A)],
        two_module_sources(),
        &topts,
    )
    .expect("link and transpile");
    run_source(
        "entry_exports_alias",
        &generated,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func2(), 42);",
    );
}

#[test]
fn linking_one_module_changes_nothing() {
    let bytes = wasm(B_USES_A);
    let alone =
        linked_source(std::slice::from_ref(&bytes), vec![ImportSources::default()]).expect("link");
    assert_eq!(
        alone,
        crate::transpile_with_options(&bytes, &TranspileOptions::default()).expect("direct")
    );
}

#[test]
fn a_table_filled_by_another_module_is_shared_with_the_shim_that_defines_it() {
    // The adapter shape: main calls a shim function that calls through the shim's table;
    // the fixup imports that table and fills it with a function of a third module.
    let main = r#"(module
        (import "shim" "0" (func $t (result i32)))
        (func (export "_start") (result i32) call $t))"#;
    let shim = r#"(module
        (type $r (func (result i32)))
        (table (export "imports") 1 funcref)
        (func (export "0") (result i32) i32.const 0 call_indirect (type $r)))"#;
    let adapter = r#"(module (func (export "answer") (result i32) i32.const 42))"#;
    let fixup = r#"(module
        (import "shim" "imports" (table 1 funcref))
        (import "adapter" "answer" (func $a (result i32)))
        (elem (i32.const 0) func $a))"#;
    let sources = vec![
        ImportSources {
            funcs: vec![func(1, 1)],
            ..ImportSources::default()
        },
        ImportSources::default(),
        ImportSources::default(),
        ImportSources {
            funcs: vec![func(2, 0)],
            tables: vec![Source::Table {
                module: 1,
                index: 0,
            }],
            ..ImportSources::default()
        },
    ];
    link_and_run(
        "shim_fixup",
        &[wasm(main), wasm(shim), wasm(adapter), wasm(fixup)],
        sources,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func1(), 42);",
    );
}

#[test]
fn an_appended_module_keeps_its_globals_segments_and_block_types() {
    let main = r#"(module
        (import "b" "h" (func $h (result i32)))
        (memory (export "memory") 1)
        (func (export "_start") (result i32) call $h))"#;
    let b = r#"(module
        (import "a" "memory" (memory 1))
        (global $a i32 (i32.const 5))
        (global $b i32 (global.get $a))
        (data (i32.const 8) "\07")
        (func (export "h") (result i32)
          global.get $b
          i32.const 8 i32.load8_u
          i32.add
          (block (param i32) (result i32) i32.const 4 i32.add)
          i32.const 16 i32.const 3 i32.const 2 memory.fill
          i32.const 17 i32.load8_u
          i32.add))"#;
    let sources = vec![
        ImportSources {
            funcs: vec![func(1, 0)],
            ..ImportSources::default()
        },
        ImportSources {
            memory: Some(Source::Memory {
                module: 0,
                index: 0,
            }),
            ..ImportSources::default()
        },
    ];
    link_and_run(
        "globals_segments",
        &[wasm(main), wasm(b)],
        sources,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func1(), 19);",
    );
}

#[test]
fn an_appended_module_with_its_own_memory_and_tables_is_renumbered_operator_by_operator() {
    let main = r#"(module
        (import "b" "run" (func $r (result i32)))
        (func (export "_start") (result i32) call $r))"#;
    // Main has no memory or table, so b's become the merged module's (and its second
    // table is merged table 1).
    let b = r#"(module
        (type $r (func (result i32)))
        (memory 1)
        (table $t 4 funcref)
        (table $u 2 funcref (ref.func $one))
        (elem $p func $one $two)
        (data $d "\05")
        (func $one (result i32) i32.const 1)
        (func $two (result i32) i32.const 2)
        (func (export "run") (result i32) (local $acc i32)
          (table.init $t $p (i32.const 0) (i32.const 0) (i32.const 2))
          (elem.drop $p)
          (local.set $acc (call_indirect $t (type $r) (i32.const 1)))
          (local.set $acc (i32.add (local.get $acc) (table.size $u)))
          (local.set $acc (i32.add (local.get $acc) (ref.is_null (table.get $t (i32.const 3)))))
          (local.set $acc (i32.add (local.get $acc)
            (select (result i32) (i32.const 10) (i32.const 20) (i32.const 1))))
          (i32.store8 (i32.const 0) (i32.const 7))
          (local.set $acc (i32.add (local.get $acc) (i32.load8_u (i32.const 0))))
          (table.set $t (i32.const 3) (ref.func $one))
          (local.set $acc (i32.add (local.get $acc) (call_indirect $t (type $r) (i32.const 3))))
          (local.set $acc (i32.add (local.get $acc) (table.grow $u (ref.null func) (i32.const 1))))
          (local.set $acc (i32.add (local.get $acc) (memory.size)))
          (local.set $acc (i32.add (local.get $acc) (memory.grow (i32.const 1))))
          (memory.init $d (i32.const 100) (i32.const 0) (i32.const 1))
          (data.drop $d)
          (local.set $acc (i32.add (local.get $acc) (i32.load8_u (i32.const 100))))
          (table.copy $t $t (i32.const 0) (i32.const 1) (i32.const 1))
          (local.set $acc (i32.add (local.get $acc) (call_indirect $t (type $r) (i32.const 0))))
          (table.fill $u (i32.const 0) (ref.null func) (i32.const 2))
          (local.get $acc)))"#;
    let sources = vec![
        ImportSources {
            funcs: vec![func(1, 2)],
            ..ImportSources::default()
        },
        ImportSources::default(),
    ];
    link_and_run(
        "tables_memories",
        &[wasm(main), wasm(b)],
        sources,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func1(), 34);",
    );
}

#[test]
fn a_second_memory_loops_and_bulk_copies_are_renumbered() {
    let main = r#"(module
        (import "b" "run" (func $r (result i32)))
        (memory (export "memory") 1)
        (func (export "_start") (result i32) call $r))"#;
    let b = r#"(module
        (import "a" "memory" (memory $a 1))
        (memory $m2 1)
        (func (export "run") (result i32) (local $i i32)
          (i64.store $a (i32.const 0) (i64.const 258))
          (memory.copy $m2 $a (i32.const 0) (i32.const 0) (i32.const 8))
          (loop $l
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br_if $l (i32.lt_u (local.get $i) (i32.const 3))))
          (i32.add (i32.wrap_i64 (i64.load $m2 (i32.const 0))) (local.get $i))))"#;
    let sources = vec![
        ImportSources {
            funcs: vec![func(1, 0)],
            ..ImportSources::default()
        },
        ImportSources {
            memory: Some(Source::Memory {
                module: 0,
                index: 0,
            }),
            ..ImportSources::default()
        },
    ];
    link_and_run(
        "second_memory",
        &[wasm(main), wasm(b)],
        sources,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func1(), 261);",
    );
}

#[test]
fn each_kind_of_import_reports_a_cycle_a_wrong_provider_or_a_missing_host() {
    let main = r#"(module (global (export "g") i32 (i32.const 1)) (func (export "f")))"#;
    let cases = [
        (r#"(module (import "a" "g" (global i32)))"#, "global"),
        (r#"(module (import "a" "t" (table 1 funcref)))"#, "table"),
        (r#"(module (import "a" "m" (memory 1)))"#, "memory"),
    ];
    let own = |i: usize| match i {
        0 => Source::Global {
            module: 1,
            index: 0,
        },
        1 => Source::Table {
            module: 1,
            index: 0,
        },
        _ => Source::Memory {
            module: 1,
            index: 0,
        },
    };
    let with = |i: usize, source: Source| match i {
        0 => ImportSources {
            globals: vec![source],
            ..ImportSources::default()
        },
        1 => ImportSources {
            tables: vec![source],
            ..ImportSources::default()
        },
        _ => ImportSources {
            memory: Some(source),
            ..ImportSources::default()
        },
    };
    for (i, (module, kind)) in cases.iter().enumerate() {
        let wasms = [wasm(main), wasm(module)];
        let cycle = link_error(&wasms, vec![ImportSources::default(), with(i, own(i))]);
        assert!(cycle.contains("cycle"), "{kind}: {cycle}");
        let wrong = link_error(&wasms, vec![ImportSources::default(), with(i, func(0, 0))]);
        assert!(wrong.contains("provided by"), "{kind}: {wrong}");
        let host = link_error(
            &wasms,
            vec![ImportSources::default(), ImportSources::default()],
        );
        assert!(
            host.contains("host import") && host.contains(kind),
            "{kind}: {host}"
        );
    }
}

#[test]
fn typed_references_and_unfoldable_offsets_in_an_appended_module_are_refused() {
    let main = r#"(module (global (export "g") i32 (i32.const 1)) (func (export "f")))"#;
    let typed = r#"(module (type $t (func)) (func (local (ref null $t))))"#;
    // The provider's global is immutable but reads a host global: not a constant.
    let host_based = r#"(module
        (import "env" "h" (global i32))
        (global (export "g") i32 (global.get 0)))"#;
    let runtime = r#"(module
        (import "a" "g" (global i32))
        (memory 1)
        (data (global.get 0) "x"))"#;
    let none = || vec![ImportSources::default(), ImportSources::default()];
    let what = link_error(&[wasm(main), wasm(typed)], none());
    assert!(what.contains("typed reference"), "{what}");
    let what = link_error(
        &[wasm(host_based), wasm(runtime)],
        vec![
            ImportSources::default(),
            ImportSources {
                globals: vec![Source::Global {
                    module: 0,
                    index: 1,
                }],
                ..ImportSources::default()
            },
        ],
    );
    assert!(what.contains("not an immutable integer constant"), "{what}");
}

#[test]
fn a_data_offset_reading_a_constant_global_of_another_module_is_folded() {
    let main = r#"(module (global (export "base") i32 (i32.const 5)) (memory (export "m") 1)
        (func (export "peek") (param i32) (result i32) local.get 0 i32.load8_u))"#;
    let lib = r#"(module
        (import "a" "base" (global i32))
        (import "a" "m" (memory 1))
        (data (global.get 0) "\2a"))"#;
    link_and_run(
        "folded_data_offset",
        &[wasm(main), wasm(lib)],
        vec![
            ImportSources::default(),
            ImportSources {
                globals: vec![Source::Global {
                    module: 0,
                    index: 0,
                }],
                memory: Some(Source::Memory {
                    module: 0,
                    index: 0,
                }),
                ..ImportSources::default()
            },
        ],
        "let mut inst = Instance::new();\n    assert_eq!(inst.func0(5), 42);\n    assert_eq!(inst.func0(4), 0);",
    );
}

#[test]
fn start_functions_run_in_module_order() {
    // main sets g = 10, b adds 1, c doubles: 22 only in that order.
    let main = r#"(module
        (global $g (export "g") (mut i32) (i32.const 0))
        (func $s i32.const 10 global.set $g)
        (func (export "_start") (result i32) global.get $g)
        (start $s))"#;
    let b = r#"(module
        (import "a" "g" (global $g (mut i32)))
        (func $s global.get $g i32.const 1 i32.add global.set $g)
        (start $s))"#;
    let c = r#"(module
        (import "a" "g" (global $g (mut i32)))
        (func $s global.get $g i32.const 2 i32.mul global.set $g)
        (start $s))"#;
    let global = |_: ()| ImportSources {
        globals: vec![Source::Global {
            module: 0,
            index: 0,
        }],
        ..ImportSources::default()
    };
    link_and_run(
        "start_order",
        &[wasm(main), wasm(b), wasm(c)],
        vec![ImportSources::default(), global(()), global(())],
        "let mut inst = Instance::new();\n    assert_eq!(inst.func1(), 22);",
    );
}

/// Links `wasms` (main first) with `carried[k]` kept from module `k`.
fn link_carrying(
    wasms: &[Vec<u8>],
    carried: Vec<Vec<(String, String)>>,
) -> Result<String, TranspileError> {
    let topts = TranspileOptions::default();
    let modules = wasms
        .iter()
        .zip(carried)
        .map(|(bytes, carried)| {
            Ok(LinkModule {
                ir: parse::parse_module(bytes, &topts)?,
                sources: ImportSources::default(),
                carried,
            })
        })
        .collect::<Result<Vec<_>, TranspileError>>()?;
    let rewritten = rewrite_bodies(&modules)?;
    let ir = link(modules, &rewritten)?;
    Ok(ir
        .exports
        .iter()
        .map(|e| e.name.clone())
        .collect::<Vec<_>>()
        .join(","))
}

fn carry(own: &str, name: &str) -> Vec<(String, String)> {
    vec![(own.to_string(), name.to_string())]
}

#[test]
fn a_carried_export_of_an_appended_module_is_exported_under_the_given_name() {
    let wasms = [
        wasm(r#"(module (func (export "f")))"#),
        wasm(r#"(module (func (export "g")))"#),
    ];
    let exports = link_carrying(&wasms, vec![Vec::new(), carry("g", "renamed")]).unwrap();
    assert_eq!(exports, "f,renamed");
}

#[test]
fn carrying_an_export_the_module_lacks_is_refused() {
    let wasms = [
        wasm(r#"(module (func (export "f")))"#),
        wasm(r#"(module (func (export "g")))"#),
    ];
    let err = link_carrying(&wasms, vec![Vec::new(), carry("missing", "x")]).unwrap_err();
    assert!(err.to_string().contains("`missing`"), "{err}");
}

#[test]
fn carrying_an_export_over_an_existing_name_is_refused() {
    let wasms = [
        wasm(r#"(module (func (export "f")))"#),
        wasm(r#"(module (func (export "g")))"#),
    ];
    let err = link_carrying(&wasms, vec![Vec::new(), carry("g", "f")]).unwrap_err();
    assert!(err.to_string().contains("more than once"), "{err}");
}

/// A signature that only appended modules declare gets one canonical id across them, so
/// a `call_indirect` of one reaches a function of the other.
#[test]
fn appended_modules_share_the_canonical_id_of_a_signature_the_main_module_lacks() {
    let main = r#"(module (func (export "f")))"#;
    let b = r#"(module
        (type $t (func (param i64) (result i64)))
        (table (export "t") 1 funcref)
        (elem (i32.const 0) $inc)
        (func $inc (type $t) local.get 0 i64.const 1 i64.add))"#;
    let c = r#"(module
        (import "b" "t" (table 1 funcref))
        (type $t (func (param i64) (result i64)))
        (func (export "g") (result i64) i64.const 41 i32.const 0 call_indirect (type $t)))"#;
    let wasms = [wasm(main), wasm(b), wasm(c)];
    let sources = vec![
        ImportSources::default(),
        ImportSources::default(),
        ImportSources {
            tables: vec![Source::Table {
                module: 1,
                index: 0,
            }],
            ..ImportSources::default()
        },
    ];
    // `g` is function 2 of the merged module (main's `f`, then `inc`, then `g`).
    link_and_run(
        "canon_shared",
        &wasms,
        sources,
        "let mut inst = Instance::new();\n    assert_eq!(inst.func2(), 42);",
    );
}

#[test]
fn a_single_appended_start_is_the_merged_start() {
    let main = r#"(module (func (export "f")))"#;
    let b = r#"(module (func $s) (start $s))"#;
    let topts = TranspileOptions::default();
    let (main, b) = (wasm(main), wasm(b));
    let modules = vec![
        LinkModule {
            ir: parse::parse_module(&main, &topts).unwrap(),
            sources: ImportSources::default(),
            carried: Vec::new(),
        },
        LinkModule {
            ir: parse::parse_module(&b, &topts).unwrap(),
            sources: ImportSources::default(),
            carried: Vec::new(),
        },
    ];
    let rewritten = rewrite_bodies(&modules).unwrap();
    let ir = link(modules, &rewritten).unwrap();
    // main defines function 0; b's function 0 is merged function 1.
    assert_eq!(ir.start, Some(1));
    assert_eq!(ir.bodies.len(), 2);
}

#[test]
fn an_operator_outside_the_supported_subset_is_named() {
    let main = r#"(module (func (export "f")))"#;
    let simd = r#"(module (func (result v128) i32.const 1 i32x4.splat))"#;
    let tail = r#"(module (func $f return_call $f))"#;
    for (extra, name) in [(simd, "i32x4_splat"), (tail, "return_call")] {
        let what = link_error(
            &[wasm(main), wasm(extra)],
            vec![ImportSources::default(), ImportSources::default()],
        );
        assert!(what.contains(name), "{what}");
    }
}

/// main calls b.g (answered by b); b calls the host import `env.scale` and adds one.
const MAIN_CALLS_B_ARG: &str = r#"(module
    (import "b" "g" (func $g (param i32) (result i32)))
    (func (export "run") (param i32) (result i32) local.get 0 call $g))"#;
const B_CALLS_HOST: &str = r#"(module
    (import "env" "scale" (func $scale (param i32) (result i32)))
    (func (export "g") (param i32) (result i32)
      local.get 0 call $scale i32.const 1 i32.add))"#;

fn host_pair_sources() -> Vec<ImportSources> {
    vec![
        ImportSources {
            funcs: vec![func(1, 1)],
            ..ImportSources::default()
        },
        ImportSources::default(),
    ]
}

#[test]
fn an_appended_host_import_is_called_through_the_host() {
    // main: import 0, run = 1, b.scale (import) = module 1 index 0, b.g = module 1 index 1
    // -> merged 2; b's host import takes the tail slot 3.
    link_and_run(
        "tail_import_run",
        &[wasm(MAIN_CALLS_B_ARG), wasm(B_CALLS_HOST)],
        host_pair_sources(),
        "struct H;\n    impl Imports for H {\n        fn import3(&mut self, a0: i32) -> i32 { a0 * 10 }\n    }\n    let mut inst = Instance::new(H);\n    assert_eq!(inst.func1(5), 51);",
    );
}

#[test]
fn an_appended_host_import_is_a_method_of_the_imports_trait() {
    let generated = linked_source(
        &[wasm(MAIN_CALLS_B_ARG), wasm(B_CALLS_HOST)],
        host_pair_sources(),
    )
    .expect("link and transpile");
    assert!(
        generated.contains("fn import3(&mut self, a0: i32) -> i32;"),
        "{generated}"
    );
}

#[test]
fn named_imports_name_an_appended_host_import_after_the_wasm_import() {
    let topts = TranspileOptions {
        named_imports: true,
        ..TranspileOptions::default()
    };
    let generated = linked_source_with(
        &[wasm(MAIN_CALLS_B_ARG), wasm(B_CALLS_HOST)],
        host_pair_sources(),
        &topts,
    )
    .expect("link and transpile");
    assert!(
        generated.contains("fn env_scale(&mut self, a0: i32) -> i32;"),
        "{generated}"
    );
}

#[test]
fn appended_host_imports_of_several_modules_get_consecutive_slots() {
    // main: imports 0,1, run = 2; b.g = 3; c.h = 4; tail: b.scale = 5, c.bump = 6.
    let main = r#"(module
        (import "b" "g" (func $g (param i32) (result i32)))
        (import "c" "h" (func $h (param i32) (result i32)))
        (func (export "run") (param i32) (result i32)
          local.get 0 call $g call $h))"#;
    let c = r#"(module
        (import "env" "bump" (func $bump (param i32) (result i32)))
        (func (export "h") (param i32) (result i32) local.get 0 call $bump))"#;
    link_and_run(
        "tail_import_two",
        &[wasm(main), wasm(B_CALLS_HOST), wasm(c)],
        vec![
            ImportSources {
                funcs: vec![func(1, 1), func(2, 1)],
                ..ImportSources::default()
            },
            ImportSources::default(),
            ImportSources::default(),
        ],
        "struct H;\n    impl Imports for H {\n        fn import5(&mut self, a0: i32) -> i32 { a0 * 10 }\n        fn import6(&mut self, a0: i32) -> i32 { a0 + 100 }\n    }\n    let mut inst = Instance::new(H);\n    assert_eq!(inst.func2(5), 151);",
    );
}

#[test]
fn a_tail_import_can_be_a_call_indirect_target_and_follows_a_synthesized_start() {
    // Both modules start, so a start function is synthesized after the defined
    // functions; the tail slot comes after it.
    let main = r#"(module
        (import "b" "g" (func $g (param i32) (result i32)))
        (global $s (export "s") (mut i32) (i32.const 0))
        (func $init i32.const 7 global.set $s)
        (func (export "run") (param i32) (result i32) local.get 0 call $g)
        (start $init))"#;
    let b = r#"(module
        (import "env" "scale" (func $scale (param i32) (result i32)))
        (table 1 funcref)
        (elem (i32.const 0) $scale)
        (func $nop)
        (func (export "g") (param i32) (result i32)
          local.get 0 i32.const 0 call_indirect (param i32) (result i32))
        (start $nop))"#;
    // main: import 0, init 1, run 2; b: nop 3, g 4; synthesized start 5; tail scale 6.
    link_and_run(
        "tail_import_indirect",
        &[wasm(main), wasm(b)],
        vec![
            ImportSources {
                funcs: vec![func(1, 2)],
                ..ImportSources::default()
            },
            ImportSources::default(),
        ],
        "struct H;\n    impl Imports for H {\n        fn import6(&mut self, a0: i32) -> i32 { a0 * 10 }\n    }\n    let mut inst = Instance::new(H);\n    assert_eq!(inst.func2(5), 50);",
    );
}

#[test]
fn a_host_import_chain_through_an_appended_module_reaches_one_tail_slot() {
    // c's import is answered by b's host import, so the call reaches that one slot.
    let main = r#"(module
        (import "c" "h" (func $h (param i32) (result i32)))
        (func (export "run") (param i32) (result i32) local.get 0 call $h))"#;
    let c = r#"(module
        (import "b" "scale" (func $s (param i32) (result i32)))
        (func (export "h") (param i32) (result i32) local.get 0 call $s))"#;
    let b = r#"(module
        (import "env" "scale" (func (param i32) (result i32))))"#;
    // main: import 0, run 1; c.h = 2; b defines nothing; the tail slot is 3.
    link_and_run(
        "tail_import_chain",
        &[wasm(main), wasm(c), wasm(b)],
        vec![
            ImportSources {
                funcs: vec![func(1, 1)],
                ..ImportSources::default()
            },
            ImportSources {
                funcs: vec![func(2, 0)],
                ..ImportSources::default()
            },
            ImportSources::default(),
        ],
        "struct H;\n    impl Imports for H {\n        fn import3(&mut self, a0: i32) -> i32 { a0 * 10 }\n    }\n    let mut inst = Instance::new(H);\n    assert_eq!(inst.func1(5), 50);",
    );
}

#[test]
fn entry_exports_and_reentrant_hosts_see_an_appended_host_import() {
    let topts = TranspileOptions {
        entry_exports: Some(vec!["run".to_string()]),
        reentrant_imports: true,
        ..TranspileOptions::default()
    };
    let generated = linked_source_with(
        &[wasm(MAIN_CALLS_B_ARG), wasm(B_CALLS_HOST)],
        host_pair_sources(),
        &topts,
    )
    .expect("link and transpile");
    assert!(generated.contains("fn import3(&mut self, g: &mut Instance<Self>"));
    run_source(
        "tail_import_reentrant",
        &generated,
        "struct H;\n    impl Imports for H {\n        fn import3(&mut self, _g: &mut Instance<Self>, a0: i32) -> i32 { a0 * 10 }\n    }\n    let mut inst = Instance::new(H);\n    assert_eq!(inst.func1(5), 51);",
    );
}

#[test]
fn linked_modules_without_an_appended_host_import_have_no_tail_slot() {
    let topts = TranspileOptions::default();
    let wasms = [wasm(MAIN_CALLS_B), wasm(B_USES_A)];
    let modules = wasms
        .iter()
        .zip(two_module_sources())
        .map(|(bytes, sources)| LinkModule {
            ir: parse::parse_module(bytes, &topts).expect("parse"),
            sources,
            carried: Vec::new(),
        })
        .collect::<Vec<_>>();
    let rewritten = rewrite_bodies(&modules).expect("rewrite");
    let ir = link(modules, &rewritten).expect("link");
    assert!(ir.tail_imports.is_empty());
}

#[test]
fn imports_that_cannot_be_bound_are_refused() {
    let main = r#"(module (import "b" "f" (func)) (import "b" "g" (global i32)))"#;
    let b = r#"(module (global (export "g") i32 (i32.const 1)) (func (export "f")))"#;
    let moved_global = ImportSources {
        funcs: vec![Source::Host],
        globals: vec![Source::Global {
            module: 1,
            index: 0,
        }],
        ..ImportSources::default()
    };
    let what = link_error(
        &[wasm(main), wasm(b)],
        vec![moved_global, ImportSources::default()],
    );
    assert!(what.contains("main module imports"), "{what}");

    // b's import is answered by itself.
    let main = r#"(module (func (export "f")))"#;
    let looping = r#"(module (import "b" "f" (func)))"#;
    let what = link_error(
        &[wasm(main), wasm(looping)],
        vec![
            ImportSources::default(),
            ImportSources {
                funcs: vec![func(1, 0)],
                ..ImportSources::default()
            },
        ],
    );
    assert!(what.contains("cycle"), "{what}");

    let what = link_error(
        &[wasm(main), wasm(looping)],
        vec![
            ImportSources::default(),
            ImportSources {
                funcs: vec![Source::Global {
                    module: 0,
                    index: 0,
                }],
                ..ImportSources::default()
            },
        ],
    );
    assert!(what.contains("provided by"), "{what}");
}

#[test]
fn an_appended_module_with_gc_types_tags_or_runtime_offsets_is_refused() {
    let main = r#"(module (func (export "f")))"#;
    let gc = r#"(module (type (struct)))"#;
    let tag = r#"(module (tag))"#;
    for (extra, what) in [(gc, "GC"), (tag, "tag")] {
        let got = link_error(
            &[wasm(main), wasm(extra)],
            vec![ImportSources::default(), ImportSources::default()],
        );
        assert!(got.contains(what), "{got}");
    }
}
