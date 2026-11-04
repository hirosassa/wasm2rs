//! A module may define several tables. Table 0 keeps the `table` field and
//! accessors; table `i > 0` gets `table{i}`. (An imported table together with
//! other tables is not supported yet.)

use crate::common;

const TWO_TABLES: &str = r#"(module
  (type $ft (func (result i32)))
  (func $a (result i32) i32.const 1)
  (func $b (result i32) i32.const 2)
  (func $c (result i32) i32.const 3)
  (table $t0 2 funcref)
  (table $t1 3 funcref)
  (elem (table $t0) (i32.const 0) func $a $b)
  (elem (table $t1) (i32.const 1) func $c $a)
  (func (export "via0") (param i32) (result i32)
    local.get 0 call_indirect $t0 (type $ft))
  (func (export "via1") (param i32) (result i32)
    local.get 0 call_indirect $t1 (type $ft)))"#;

#[test]
fn call_indirect_selects_the_table() {
    // `$a $b $c` are func0..2; `via0`/`via1` are func3/func4.
    common::compile_run(
        "multi_table_call_indirect",
        TWO_TABLES,
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.func3(0), 1);\n assert_eq!(inst.func3(1), 2);\n \
         assert_eq!(inst.func4(1), 3);\n assert_eq!(inst.func4(2), 1);",
    );
}

#[test]
fn an_uninitialised_slot_of_the_second_table_traps() {
    common::expect_trap(
        "multi_table_null_slot",
        TWO_TABLES,
        "let mut inst = Instance::new(); inst.func4(0);",
    );
}

#[test]
fn table_ops_act_on_the_named_table_only() {
    // func0 is the helper; the exports follow.
    common::compile_run(
        "multi_table_ops",
        r#"(module
          (type $ft (func (result i32)))
          (func $a (result i32) i32.const 7)
          (table $t0 1 funcref)
          (table $t1 2 funcref)
          (func (export "set1") (param i32)
            local.get 0 ref.func $a table.set $t1)
          (func (export "is_null") (param i32 i32) (result i32)
            local.get 1
            (if (result i32) (then local.get 0 table.get $t1 ref.is_null)
                             (else local.get 0 table.get $t0 ref.is_null)))
          (func (export "size") (param i32) (result i32)
            local.get 0
            (if (result i32) (then table.size $t1) (else table.size $t0)))
          (func (export "grow1") (result i32)
            ref.null func i32.const 3 table.grow $t1)
          (func (export "fill1")
            i32.const 0 ref.func $a i32.const 2 table.fill $t1))"#,
        // set1=func1 is_null=func2 size=func3 grow1=func4 fill1=func5.
        "let mut inst = Instance::new();\n \
         assert_eq!(inst.func2(1, 1), 1);\n inst.func1(1);\n \
         assert_eq!(inst.func2(1, 1), 0);\n assert_eq!(inst.func2(0, 0), 1);\n \
         assert_eq!(inst.func3(0), 1);\n assert_eq!(inst.func3(1), 2);\n \
         assert_eq!(inst.func4(), 2);\n assert_eq!(inst.func3(1), 5);\n \
         assert_eq!(inst.func2(0, 1), 1);\n inst.func5();\n \
         assert_eq!(inst.func2(0, 1), 0);",
    );
}

#[test]
fn table_init_and_copy_target_the_named_tables() {
    common::compile_run(
        "multi_table_init_copy",
        r#"(module
          (type $ft (func (result i32)))
          (func $a (result i32) i32.const 4)
          (table $t0 2 funcref)
          (table $t1 2 funcref)
          (elem $e func $a)
          (func (export "init1")
            i32.const 0 i32.const 0 i32.const 1 table.init $t1 $e)
          (func (export "copy_1_to_0")
            i32.const 1 i32.const 0 i32.const 1 table.copy $t0 $t1)
          (func (export "call0") (param i32) (result i32)
            local.get 0 call_indirect $t0 (type $ft)))"#,
        // init1=func1 copy_1_to_0=func2 call0=func3.
        "let mut inst = Instance::new();\n inst.func1();\n inst.func2();\n \
         assert_eq!(inst.func3(1), 4);",
    );
}

#[test]
fn a_single_table_keeps_its_names() {
    let wasm = wat::parse_str("(module (table 1 funcref))").unwrap();
    let out = wasm2rs::transpile(&wasm).unwrap();
    assert!(out.contains("table: Vec<u32>,"), "{out}");
    assert!(!out.contains("table1"), "{out}");
}

#[test]
fn an_imported_table_with_another_table_is_still_rejected() {
    let wasm =
        wat::parse_str(r#"(module (import "e" "t" (table 1 funcref)) (table 1 funcref))"#).unwrap();
    let err = wasm2rs::transpile(&wasm).unwrap_err().to_string();
    assert!(err.contains("multiple tables"), "{err}");
}
