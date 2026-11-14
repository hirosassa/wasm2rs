//! The managed-object slot stays compact: a `GcSlot` must not be inflated by its
//! widest payloads (`v128`, an exception handle), since every struct field and
//! array element pays for the largest variant. These cover the boxed `v128`
//! slot and the double-`Arc` exception handle that keep it at two words.

use crate::common;

use common::compile_run;

#[test]
fn gc_slot_and_ref_stay_compact() {
    // `size_of` is only reachable from the generated module's scope, so assert
    // inside the program; the module declares a struct so `GcSlot` is emitted.
    compile_run(
        "gc_slot_size",
        r#"(module
            (type $s (struct (field i32)))
            (func (export "a") (result (ref null $s)) struct.new_default $s))"#,
        "assert_eq!(std::mem::size_of::<GcRef>(), 16);\n\
         assert_eq!(std::mem::size_of::<GcSlot>(), 16);",
    );
}

#[test]
fn v128_struct_field_and_array_element_round_trip() {
    compile_run(
        "gc_slot_v128",
        r#"(module
            (type $s (struct (field (mut v128))))
            (type $a (array (mut v128)))
            (func (export "field") (result i32)
              (local $o (ref null $s))
              (local.set $o (struct.new $s (v128.const i32x4 1 2 3 4)))
              (struct.set $s 0 (local.get $o) (v128.const i32x4 5 6 7 8))
              (i32x4.extract_lane 2 (struct.get $s 0 (local.get $o))))
            (func (export "default") (result i32)
              (i32x4.extract_lane 0 (struct.get $s 0 (struct.new_default $s))))
            (func (export "array") (result i32)
              (local $x (ref null $a))
              (local.set $x (array.new $a (v128.const i32x4 9 10 11 12) (i32.const 3)))
              (array.set $a (local.get $x) (i32.const 1) (v128.const i32x4 20 21 22 23))
              (i32.add
                (i32x4.extract_lane 3 (array.get $a (local.get $x) (i32.const 1)))
                (i32x4.extract_lane 1 (array.get $a (local.get $x) (i32.const 2))))))"#,
        "assert_eq!(func0(), 7); assert_eq!(func1(), 0); assert_eq!(func2(), 33);",
    );
}

#[test]
fn exnref_in_a_slot_survives_clone_and_rethrow() {
    // An exception handle stored in a struct field is cloned out and rethrown
    // with its payload intact.
    compile_run(
        "gc_slot_exnref",
        r#"(module
            (tag $e (param i32))
            (type $s (struct (field (mut exnref))))
            (func (export "run") (param i32) (result i32)
              (local $o (ref null $s))
              (local.set $o (struct.new_default $s))
              (block $h (result i32)
                (block $c (result exnref)
                  (struct.set $s 0 (local.get $o)
                    (block $caught (result exnref)
                      (try_table (catch_all_ref $caught) (throw $e (local.get 0)))
                      (unreachable)))
                  (try_table (catch $e $h)
                    (throw_ref (struct.get $s 0 (local.get $o))))
                  (unreachable))
                (drop)
                (unreachable))))"#,
        "assert_eq!(func0(41), 41);",
    );
}
