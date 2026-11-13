//! The generated source drops redundant parentheses (they make `rustc` quadratic on
//! huge modules, see `docs/f4-rustc-time-spike.md`), but must not drop any that
//! change the parse. The first test runs expressions whose meaning depends on
//! their parentheses (negative receivers, casts before comparisons, mixed
//! precedence, ranges-free unary chains, block tails starting with `if`/`match`)
//! through `rustc` and checks the values; the second checks the emitted text.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::float_cmp,
    clippy::lossy_float_literal,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::unwrap_in_result,
    reason = "test code"
)]

use crate::common;

#[test]
fn precedence_sensitive_expressions_keep_their_meaning() {
    let wat = r#"
        (module
          (memory 1)
          ;; func0: nested arithmetic as a call argument: (a+b)*(a-b) stored then reloaded
          (func (param i32 i32) (result i32)
            i32.const 0
            local.get 0 local.get 1 i32.add
            local.get 0 local.get 1 i32.sub
            i32.mul
            i32.store
            i32.const 0
            i32.load)
          ;; func1: negative literal receivers
          (func (param i32) (result i32)
            i32.const -8 local.get 0 i32.shl
            i32.const -2 local.get 0 i32.rotl
            i32.xor)
          ;; func2: unsigned compare of two sums (cast must stay parenthesised)
          (func (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.add
            local.get 1 i32.const 3 i32.sub
            i32.lt_u)
          ;; func3: if with a result whose value is a sum, as the block tail
          (func (param i32 i32) (result i32)
            local.get 0
            if (result i32)
              local.get 0 local.get 1 i32.add
            else
              local.get 0 local.get 1 i32.sub
            end
            i32.const 1
            i32.add)
          ;; func4: select between two computed values, then a unary-ish negate
          (func (param i32 i32) (result i32)
            i32.const 0
            local.get 0 local.get 1 i32.mul
            i32.sub
            local.get 0 local.get 1 i32.add
            local.get 1
            select)
          ;; func5: i64 mix with wrap/extend casts
          (func (param i64) (result i32)
            local.get 0 i64.const -3 i64.mul
            i32.wrap_i64
            local.get 0 i32.wrap_i64
            i32.add)
          ;; func6: a loop whose carried value is an expression, via local.set
          (func (param i32) (result i32) (local i32)
            block
              loop
                local.get 1 local.get 0 i32.add
                local.set 1
                local.get 0 i32.const 1 i32.sub
                local.tee 0
                br_if 0
              end
            end
            local.get 1)
          ;; func7: i32 comparison chain: (a<b) == (c<d)
          (func (param i32 i32 i32 i32) (result i32)
            local.get 0 local.get 1 i32.lt_s
            local.get 2 local.get 3 i32.lt_s
            i32.eq))
    "#;

    let main_body = r#"
        let mut i = Instance::new();
        assert_eq!(i.func0(7, 3), (7 + 3) * (7 - 3));
        assert_eq!(i.func1(2), (-8i32).wrapping_shl(2) ^ (-2i32).rotate_left(2));
        assert_eq!(i.func1(33), (-8i32).wrapping_shl(33) ^ (-2i32).rotate_left(33));
        assert_eq!(i.func2(1, 2), i32::from(3u32 < (2u32.wrapping_sub(3))));
        assert_eq!(i.func2(-5, 2), i32::from((-3i32 as u32) < ((-1i32) as u32)));
        assert_eq!(i.func3(1, 2), 4);
        assert_eq!(i.func3(0, 5), -4);
        assert_eq!(i.func4(3, 4), -12);
        assert_eq!(i.func4(3, 0), 3);
        assert_eq!(i.func5(10), (10i64 * -3) as i32 + 10);
        assert_eq!(i.func6(4), 4 + 3 + 2 + 1);
        assert_eq!(i.func7(1, 2, 3, 4), 1);
        assert_eq!(i.func7(1, 2, 4, 3), 0);
    "#;

    common::compile_run("redundant_parens_semantics", wat, main_body);
}

/// Arguments, assigned values and block tails are not wrapped; a receiver is.
#[test]
fn emitted_source_has_no_redundant_paren_sites() {
    let wat = r#"
        (module
          (memory 1)
          (func (param i32 i32) (result i32)
            i32.const 0
            local.get 0 local.get 1 i32.add
            i32.store
            local.get 0
            if (result i32)
              local.get 0 local.get 1 i32.add
            else
              i32.const -2 local.get 1 i32.rotl
            end))
    "#;
    let wasm = wat::parse_str(wat).unwrap();
    let rust = wasm2rs::transpile(&wasm).unwrap();

    // The receiver paren that protects the negative literal must survive.
    assert!(rust.contains("(-2i32).rotate_left("), "{rust}");
    // No doubled parentheses, and the stored sum is passed unwrapped.
    assert!(!rust.contains("(("), "{rust}");
    assert!(
        rust.contains("self.w32(0i32, 0, (l0).wrapping_add(l1));"),
        "{rust}"
    );
}
