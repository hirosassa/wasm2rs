//! Output-shape tests for codegen decisions that no run-time result depends on: which
//! operands a branch must still evaluate, which helpers `unsafe_memory`/`typed_traps` rewrite,
//! where the expression-length cap bites, and how dead code is skipped.

use crate::common::{compile_run, expect_trap};
use wasm2rs::TranspileOptions;

fn gen_with(wat: &str, opts: &TranspileOptions) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::transpile_with_options(&wasm, opts).expect("transpile ok")
}

fn gen_src(wat: &str) -> String {
    gen_with(wat, &TranspileOptions::default())
}

/// The source of the generated `fn {name}` helper, up to its closing `    }`.
fn helper_fn<'a>(src: &'a str, name: &str) -> Vec<&'a str> {
    let lines: Vec<&str> = src.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.contains(&format!("fn {name}(")))
        .unwrap_or_else(|| panic!("no helper {name} in:\n{src}"));
    let end = lines[start..]
        .iter()
        .position(|l| *l == "    }")
        .map_or(lines.len(), |e| start + e);
    lines[start..=end.min(lines.len() - 1)].to_vec()
}

// ----- operands left below a branch --------------------------------------------------------

/// A load below the branch operands may trap, so it still runs (as `let _ =`) when the
/// branch leaves it behind; a global read cannot trap and is simply dropped.
const BELOW_BR: &str = r#"(module (memory 1) (global $g (mut i32) (i32.const 0))
  (func (export "br") (param i32) (result i32)
    (block (result i32)
      (global.get $g)
      (i32.load (local.get 0))
      (i32.const 5)
      (br 0)))
  (func (export "ret") (param i32) (result i32)
    (global.get $g)
    (i32.load (local.get 0))
    (i32.const 5)
    (return)))"#;

#[test]
fn a_branch_runs_a_trapping_operand_it_leaves_behind_and_drops_the_rest() {
    let src = gen_src(BELOW_BR);
    let discards: Vec<&str> = src.lines().filter(|l| l.contains("let _ = ")).collect();
    // One per function (`br` and `return`), and only the load, not the global read.
    assert_eq!(discards.len(), 2, "{src}");
    assert!(discards.iter().all(|l| l.contains("self.r32(")), "{src}");
}

#[test]
fn the_dropped_load_still_traps_when_the_branch_is_taken() {
    expect_trap(
        "dropped_load_br",
        BELOW_BR,
        "let mut i = Instance::new(); i.func0(1 << 20);",
    );
    expect_trap(
        "dropped_load_ret",
        BELOW_BR,
        "let mut i = Instance::new(); i.func1(1 << 20);",
    );
}

#[test]
fn a_conditional_branch_freezes_only_the_operands_that_may_trap() {
    let src = gen_src(
        r#"(module (memory 1) (global $g (mut i32) (i32.const 0))
          (func (export "f") (param i32) (result i32)
            (block
              (global.get $g)
              (i32.load (local.get 0))
              (br_if 0 (local.get 0))
              drop drop)
            (i32.const 1)))"#,
    );
    let frozen: Vec<&str> = src
        .lines()
        .filter(|l| l.trim_start().starts_with("let v"))
        .collect();
    // The load is pinned before the branch can skip it; the global read stays inline.
    assert_eq!(frozen.len(), 1, "{src}");
    assert!(frozen[0].contains("self.r32("), "{src}");
}

// ----- the expression-length cap -----------------------------------------------------------

/// `local.get 0` followed by `terms` additions of `1`, then three of constants with `d1`,
/// `d2` and `d3` digits: each digit lengthens the generated expression by exactly one character.
fn chain(terms: usize, d1: usize, d2: usize, d3: usize) -> String {
    let big = |d: usize| "1".repeat(d.min(9));
    format!(
        "(module (func (export \"f\") (param i32) (result i32) local.get 0 {} i32.const {} i32.add i32.const {} i32.add i32.const {} i32.add))",
        "i32.const 1 i32.add ".repeat(terms),
        big(d1),
        big(d2),
        big(d3)
    )
}

#[test]
fn an_expression_is_spilled_exactly_when_it_passes_4096_characters() {
    let (mut longest_kept, mut shortest_spilled) = (0, usize::MAX);
    for terms in 185..197 {
        for (d1, d2, d3) in
            (1..=9).flat_map(|a| (1..=9).flat_map(move |b| [1, 5].map(move |c| (a, b, c))))
        {
            {
                let src = gen_src(&chain(terms, d1, d2, d3));
                let line = src.lines().max_by_key(|l| l.len()).expect("a line").trim();
                let (len, spilled) = match line.strip_prefix("let v") {
                    // `let vN: i32 = <expr>;`
                    Some(rest) => {
                        let expr = rest.split_once(" = ").expect("a binding").1;
                        (expr.len() - 1, true)
                    }
                    None => (line.len(), false),
                };
                assert_eq!(spilled, len > 4096, "an expression of {len} characters");
                if spilled {
                    shortest_spilled = shortest_spilled.min(len);
                } else {
                    longest_kept = longest_kept.max(len);
                }
            }
        }
    }
    // The sweep reaches both sides of the boundary without a gap.
    assert_eq!((longest_kept, shortest_spilled), (4096, 4097));
}

// ----- unsafe_memory / typed_traps helper rewrites -----------------------------------------

const BULK: &str = r#"(module (memory 1) (table 4 funcref)
  (func (export "mc") (param i32 i32 i32) (memory.copy (local.get 0) (local.get 1) (local.get 2)))
  (func (export "mg") (param i32) (result i32) (memory.grow (local.get 0)))
  (func (export "tc") (param i32 i32 i32) (table.copy (local.get 0) (local.get 1) (local.get 2)))
  (func (export "tf") (param i32 i32) (table.fill (local.get 0) (ref.null func) (local.get 1))))"#;

fn bulk_with(unsafe_memory: bool, typed_traps: bool) -> String {
    gen_with(
        BULK,
        &TranspileOptions::default()
            .with_unsafe_memory(unsafe_memory)
            .with_typed_traps(typed_traps),
    )
}

#[test]
fn checked_helpers_contain_no_unsafe_code() {
    let src = bulk_with(false, false);
    assert!(!src.contains("unsafe"), "{src}");
}

#[test]
fn unsafe_memory_rewrites_memory_helpers_but_not_table_or_grow() {
    let src = bulk_with(true, false);
    let copy = helper_fn(&src, "memory_copy").join("\n");
    assert!(copy.contains("core::ptr::copy"), "{copy}");
    // Memory helpers are inlined; `memory.grow` and the table helpers are emitted verbatim.
    let lines: Vec<&str> = src.lines().collect();
    let attr_before = |name: &str| {
        let at = lines
            .iter()
            .position(|l| l.contains(&format!("fn {name}(")))
            .unwrap_or_else(|| panic!("no {name}:\n{src}"));
        lines[at - 1].trim().to_string()
    };
    assert_eq!(attr_before("memory_copy"), "#[inline(always)]");
    assert_ne!(attr_before("memory_grow"), "#[inline(always)]");
    for table_helper in ["table_copy", "table_fill"] {
        let body = helper_fn(&src, table_helper).join("\n");
        assert!(!body.contains("unsafe"), "{table_helper}:\n{body}");
    }
}

#[test]
fn typed_traps_range_check_the_copy_helpers() {
    let src = bulk_with(false, true);
    let memory = helper_fn(&src, "memory_copy").join("\n");
    assert!(memory.contains("trap_memory_oob"), "{memory}");
    let table = helper_fn(&src, "table_copy").join("\n");
    assert!(table.contains("trap_table_oob"), "{table}");
    // `memory.grow` touches no slice, so it is left alone.
    assert!(!helper_fn(&src, "memory_grow").join("\n").contains("trap_"));
    let fill = helper_fn(&src, "table_fill").join("\n");
    assert!(fill.contains("trap_table_oob"), "{fill}");
}

// ----- typed memory traps ------------------------------------------------------------------

#[test]
fn a_cross_memory_copy_traps_typed_only_when_checked() {
    let copy = "(module (memory 1) (memory 1) (func (export \"f\") (param i32) \
                (memory.copy 0 1 (local.get 0) (i32.const 0) (i32.const 4))))";
    let in_func = |src: String| {
        helper_fn(&src, "func0")
            .join("\n")
            .contains("trap_memory_oob")
    };
    let typed = TranspileOptions::default().with_typed_traps(true);
    assert!(in_func(gen_with(copy, &typed)));
    // Unchecked accesses have no per-access trap to raise, and untyped ones keep Rust's.
    assert!(!in_func(gen_with(
        copy,
        &typed.clone().with_unsafe_memory(true)
    )));
    assert!(!in_func(gen_with(
        copy,
        &typed.clone().with_typed_traps(false)
    )));
}

#[test]
fn a_memory_init_checks_its_destination_only_for_a_typed_plain_memory() {
    let init = |memory: &str| {
        format!(
            "(module {memory} (data $d \"abcd\") (func (export \"f\") (param i32) \
             (memory.init $d (local.get 0) (i32.const 0) (i32.const 4))))"
        )
    };
    let dest_checked = |memory: &str, opts: &TranspileOptions| {
        helper_fn(&gen_with(&init(memory), opts), "func0")
            .join("\n")
            .contains("get_mut(")
    };
    let typed = TranspileOptions::default().with_typed_traps(true);
    assert!(dest_checked("(memory 1)", &typed));
    // A shared memory takes its own typed form that leaves the destination to the lock.
    assert!(!dest_checked("(memory 1 1 shared)", &typed));
    assert!(!dest_checked(
        "(memory 1)",
        &typed.clone().with_unsafe_memory(true)
    ));
}

// ----- shared-memory read-modify-write op codes --------------------------------------------

#[test]
fn each_shared_memory_rmw_op_passes_its_own_code_to_the_backend() {
    for (op, code) in [
        ("add", 0),
        ("sub", 1),
        ("and", 2),
        ("or", 3),
        ("xor", 4),
        ("xchg", 5),
    ] {
        let src = gen_src(&format!(
            "(module (memory 1 1 shared) (func (export \"f\") (param i32 i32) (result i32) \
             (i32.atomic.rmw.{op} (local.get 0) (local.get 1))))"
        ));
        let call = src
            .lines()
            .find(|l| l.contains("self.memory.atomic_rmw("))
            .unwrap_or_else(|| panic!("no rmw call:\n{src}"));
        assert!(call.contains(&format!(", 4, {code}, ")), "{op}: {call}");
    }
}

// ----- dead code ---------------------------------------------------------------------------

/// After `unreachable` the rest of the arm is skipped, nested regions included; the live
/// `else` and `end` must still be recognised for the enclosing `if`.
const DEAD_NESTING: &str = r#"(module
  (func (export "f") (param i32) (result i32)
    (if (result i32) (local.get 0)
      (then
        (unreachable)
        (if (result i32) (i32.const 1) (then (i32.const 1)) (else (i32.const 2)))
        (block (nop))
        (loop (nop)))
      (else (i32.const 42)))))"#;

#[test]
fn nested_regions_in_dead_code_do_not_swallow_the_live_else_and_end() {
    compile_run(
        "dead_nesting_else",
        DEAD_NESTING,
        "assert_eq!(func0(0), 42);",
    );
    expect_trap("dead_nesting_then", DEAD_NESTING, "func0(1);");
}

#[test]
fn code_after_a_dead_nested_block_is_still_dead() {
    // If `end` of the nested block did not close it, the stray `i32.const` would be taken as
    // live and the function would fail validation or return 1.
    expect_trap(
        "dead_after_block",
        r#"(module (func (export "f") (result i32)
             (block (result i32)
               (unreachable)
               (block (nop))
               (i32.const 1))))"#,
        "func0();",
    );
}

/// Legacy exception handlers after a `throw`: a `catch`, `catch_all` or `delegate` that belongs
/// to a try nested inside the dead region must not be taken for the live handler.
const DEAD_HANDLERS: &str = r#"(module (tag $t)
  (func (export "nested_catch") (result i32)
    try (result i32)
      throw $t
      try nop catch $t nop catch_all nop end
    catch_all
      i32.const 7
    end)
  (func (export "nested_delegate") (result i32)
    try (result i32)
      throw $t
      try nop delegate 0
      i32.const 1
    catch_all
      i32.const 5
    end)
  (func (export "live_catch") (result i32)
    try (result i32)
      throw $t
    catch $t
      i32.const 3
    end)
  (func (export "live_delegate") (result i32)
    try (result i32)
      try (result i32)
        throw $t
      delegate 0
    catch_all
      i32.const 9
    end))"#;

#[test]
fn handlers_of_try_regions_nested_in_dead_code_are_skipped_and_the_live_ones_still_run() {
    compile_run(
        "dead_handlers",
        DEAD_HANDLERS,
        "assert_eq!(func0(), 7); assert_eq!(func1(), 5); assert_eq!(func2(), 3); assert_eq!(func3(), 9);",
    );
}

// ----- other render decisions --------------------------------------------------------------

#[test]
fn only_a_non_shared_imported_memory_is_lent_by_the_host() {
    let accessor = |memory: &str| {
        gen_src(&format!(
            "(module (import \"env\" \"memory\" {memory}) (func (export \"f\") (result i32) (i32.load (i32.const 0))))"
        ))
    };
    let lent = accessor("(memory 1)");
    let shared = accessor("(memory 1 1 shared)");
    assert!(lent.contains("fn memory(&self) -> &[u8];"), "{lent}");
    assert!(!shared.contains("fn memory(&self) -> &[u8];"), "{shared}");
}

#[test]
fn a_pthread_stack_is_set_up_only_when_the_module_exports_both_stack_functions() {
    let module = |exports: &str| {
        gen_src(&format!(
            r#"(module
              (import "env" "memory" (memory 1 1 shared))
              (import "env" "__pthread_create_js" (func (param i32 i32 i32 i32) (result i32)))
              (table 2 funcref)
              (func (export "_emscripten_thread_init") (param i32 i32 i32 i32 i32 i32))
              (func (export "_emscripten_tls_init") (result i32) i32.const 0)
              (func (export "_emscripten_thread_exit") (param i32))
              {exports})"#
        ))
    };
    let stack = r#"(func (export "emscripten_stack_set_limits") (param i32 i32))
                   (func (export "_emscripten_stack_restore") (param i32))"#;
    assert!(module(stack).contains("t.emscripten_stack_set_limits("));
    assert!(!module("").contains("emscripten_stack_set_limits"));
    // One of the two is not enough.
    let half = r#"(func (export "emscripten_stack_set_limits") (param i32 i32))"#;
    assert!(!module(half).contains("t.emscripten_stack_set_limits("));
}

#[test]
fn the_extern_funcref_registry_follows_the_host_trait_not_just_call_indirect() {
    let table = r#"(table 1 funcref) (type $t (func (result i32)))
        (func (export "f") (param i32) (result i32) (call_indirect (type $t) (local.get 0)))"#;
    // A call_indirect without imports has no host to hand it external funcs.
    assert!(!gen_src(&format!("(module {table})")).contains("struct ExternFuncs"));
    let with_import = gen_src(&format!("(module (import \"env\" \"h\" (func)) {table})"));
    assert!(
        with_import.contains("pub struct ExternFuncs"),
        "{with_import}"
    );
}

#[test]
fn the_continuation_handle_keeps_eleven_generation_bits() {
    let src = gen_src(
        r#"(module
          (elem declare func $gen)
          (type $ft (func (result i32)))
          (type $ct (cont $ft))
          (func $gen (result i32) i32.const 1)
          (func (export "f") (result i32)
            (resume $ct (cont.new $ct (ref.func $gen)))))"#,
    );
    // 31 bits below the tag bit, 20 of them for the slot index.
    assert!(src.contains("const CONT_IDX_BITS: u32 = 20;"), "{src}");
    assert!(src.contains("const CONT_GEN_BITS: u32 = 11;"), "{src}");
}
