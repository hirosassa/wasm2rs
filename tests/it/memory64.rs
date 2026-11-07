//! End-to-end tests for the memory64 proposal (a linear memory addressed by
//! 64-bit integers). Loads, stores, `memory.size`/`grow` and bulk operations on
//! such a memory take and return `i64` instead of `i32`.

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

use common::compile_run;

#[test]
fn i64_addressed_store_then_load_round_trips() {
    // The address operand of an i64-indexed memory is an i64 constant.
    compile_run(
        "mem64_roundtrip",
        r#"(module
            (memory i64 1)
            (func (export "w") (param i64 i32)
              local.get 0 local.get 1 i32.store)
            (func (export "r") (param i64) (result i32)
              local.get 0 i32.load))"#,
        "let mut inst = Instance::new(); \
         inst.func0(16, 0x1234_5678); \
         assert_eq!(inst.func1(16), 0x1234_5678);",
    );
}

use common::expect_trap;

#[test]
fn every_load_and_store_width_accepts_an_i64_address() {
    // Each scalar access width (8/16/32/64-bit integer, f32, f64), with a static
    // offset, round-trips through an i64-addressed memory.
    compile_run(
        "mem64_widths",
        r#"(module
            (memory i64 1)
            (func (export "i64rt") (param i64 i64) (result i64)
              local.get 0 local.get 1 i64.store offset=8
              local.get 0 i64.load offset=8)
            (func (export "f32rt") (param i64 f32) (result f32)
              local.get 0 local.get 1 f32.store
              local.get 0 f32.load)
            (func (export "f64rt") (param i64 f64) (result f64)
              local.get 0 local.get 1 f64.store
              local.get 0 f64.load)
            (func (export "narrow") (param i64) (result i64)
              local.get 0 i32.const -2 i32.store8
              local.get 0 i64.load8_s)
            (func (export "narrow16u") (param i64) (result i32)
              local.get 0 i32.const -2 i32.store16
              local.get 0 i32.load16_u))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(32, -7_000_000_000), -7_000_000_000); \
         assert_eq!(inst.func1(64, 1.5), 1.5); \
         assert_eq!(inst.func2(80, -2.25), -2.25); \
         assert_eq!(inst.func3(96), -2); \
         assert_eq!(inst.func4(112), 0xfffe);",
    );
}

#[test]
fn static_offset_is_added_to_the_i64_address() {
    compile_run(
        "mem64_offset",
        r#"(module
            (memory i64 1)
            (data (i64.const 24) "\07")
            (func (export "r") (param i64) (result i32)
              local.get 0 i32.load8_u offset=16))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(8), 7);",
    );
}

#[test]
fn memory_size_and_grow_are_i64() {
    // `memory.size` and `memory.grow` take and return i64 for a 64-bit memory;
    // grow returns the old size in pages.
    compile_run(
        "mem64_size_grow",
        r#"(module
            (memory i64 1)
            (func (export "size") (result i64) memory.size)
            (func (export "grow") (param i64) (result i64)
              local.get 0 memory.grow))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(), 1); \
         assert_eq!(inst.func1(2), 1); \
         assert_eq!(inst.func0(), 3);",
    );
}

#[test]
fn growing_beyond_any_possible_size_returns_minus_one() {
    // A delta whose byte size overflows u64 cannot be satisfied: the result is
    // -1, not a panic or a wrapped small allocation.
    compile_run(
        "mem64_grow_fail",
        r#"(module
            (memory i64 1)
            (func (export "grow") (param i64) (result i64)
              local.get 0 memory.grow)
            (func (export "size") (result i64) memory.size))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(i64::MAX), -1); \
         assert_eq!(inst.func0(-1), -1); \
         assert_eq!(inst.func1(), 1);",
    );
}

#[test]
fn memory64_grow_respects_the_declared_maximum() {
    // `(memory i64 1 2)`: growing to exactly two pages works, one more fails and
    // preserves the size; grow by zero returns the current size.
    compile_run(
        "mem64_grow_max",
        r#"(module
            (memory i64 1 2)
            (func (export "grow") (param i64) (result i64)
              local.get 0 memory.grow)
            (func (export "size") (result i64) memory.size))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(2), -1); \
         assert_eq!(inst.func1(), 1); \
         assert_eq!(inst.func0(1), 1); \
         assert_eq!(inst.func0(0), 2); \
         assert_eq!(inst.func0(1), -1); \
         assert_eq!(inst.func1(), 2);",
    );
}

#[test]
fn memory_fill_and_copy_take_i64_operands() {
    compile_run(
        "mem64_bulk",
        r#"(module
            (memory i64 1)
            (func (export "fill") (param i64 i32 i64)
              local.get 0 local.get 1 local.get 2 memory.fill)
            (func (export "copy") (param i64 i64 i64)
              local.get 0 local.get 1 local.get 2 memory.copy)
            (func (export "r8") (param i64) (result i32)
              local.get 0 i32.load8_u))"#,
        "let mut inst = Instance::new(); \
         inst.func0(10, 0xab, 4); \
         inst.func1(100, 10, 4); \
         assert_eq!(inst.func2(100), 0xab); \
         assert_eq!(inst.func2(103), 0xab); \
         assert_eq!(inst.func2(104), 0);",
    );
}

#[test]
fn memory_init_into_a_64bit_memory() {
    // `memory.init` of a passive segment: the destination is an i64 address,
    // the source offset and length stay i32.
    compile_run(
        "mem64_init",
        r#"(module
            (memory i64 1)
            (data $d "\01\02\03\04")
            (func (export "init") (param i64)
              local.get 0 i32.const 1 i32.const 2 memory.init $d)
            (func (export "r8") (param i64) (result i32)
              local.get 0 i32.load8_u))"#,
        "let mut inst = Instance::new(); \
         inst.func0(50); \
         assert_eq!(inst.func1(50), 2); \
         assert_eq!(inst.func1(51), 3); \
         assert_eq!(inst.func1(52), 0);",
    );
}

#[test]
fn address_at_4gib_traps_instead_of_wrapping_to_zero() {
    // The defining property of a 64-bit address: 2^32 is *not* address 0.
    expect_trap(
        "mem64_no_wrap",
        r#"(module
            (memory i64 1)
            (func (export "r") (param i64) (result i32)
              local.get 0 i32.load))"#,
        "let mut inst = Instance::new(); inst.func0(0x1_0000_0000);",
    );
}

#[test]
fn address_plus_offset_overflowing_u64_traps() {
    // -1 is u64::MAX; adding any offset overflows. It must trap, not wrap to a
    // small in-bounds address.
    expect_trap(
        "mem64_ea_overflow",
        r#"(module
            (memory i64 1)
            (func (export "r") (param i64) (result i32)
              local.get 0 i32.load offset=16))"#,
        "let mut inst = Instance::new(); inst.func0(-1);",
    );
}

#[test]
fn bulk_fill_with_huge_length_traps() {
    expect_trap(
        "mem64_fill_oob",
        r#"(module
            (memory i64 1)
            (func (export "fill") (param i64 i32 i64)
              local.get 0 local.get 1 local.get 2 memory.fill))"#,
        "let mut inst = Instance::new(); inst.func0(8, 0, -1);",
    );
}

use common::compile_run_raw;

#[test]
fn atomic_rmw_and_cmpxchg_take_an_i64_address() {
    // Atomics on a (non-shared) 64-bit memory: the address is i64, the access is
    // an ordinary read-modify-write on the single instance.
    compile_run(
        "mem64_atomics",
        r#"(module
            (memory i64 1)
            (func (export "add") (param i64 i32) (result i32)
              local.get 0 local.get 1 i32.atomic.rmw.add)
            (func (export "cas") (param i64 i32 i32) (result i32)
              local.get 0 local.get 1 local.get 2 i32.atomic.rmw.cmpxchg)
            (func (export "r") (param i64) (result i32)
              local.get 0 i32.load))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func0(8, 5), 0); \
         assert_eq!(inst.func0(8, 2), 5); \
         assert_eq!(inst.func1(8, 7, 100), 7); \
         assert_eq!(inst.func1(8, 7, 200), 100); \
         assert_eq!(inst.func2(8), 100);",
    );
}

#[test]
fn memory32_and_memory64_coexist() {
    // multi-memory: a 32-bit memory (i32 addresses) next to a 64-bit one (i64
    // addresses). Each access uses its own memory's address type, and a copy
    // between them takes an i32 length (the narrower of the two).
    compile_run(
        "mem64_mixed",
        r#"(module
            (memory $m32 1)
            (memory $m64 i64 1)
            (data (memory $m32) (i32.const 0) "\aa\bb\cc")
            (func (export "r64") (param i64) (result i32)
              local.get 0 i32.load8_u $m64)
            (func (export "r32") (param i32) (result i32)
              local.get 0 i32.load8_u $m32)
            (func (export "copy_32_to_64")
              i64.const 40 i32.const 0 i32.const 3 memory.copy $m64 $m32)
            (func (export "copy_64_to_32")
              i32.const 100 i64.const 40 i32.const 3 memory.copy $m32 $m64)
            (func (export "size64") (result i64) memory.size $m64)
            (func (export "size32") (result i32) memory.size $m32))"#,
        "let mut inst = Instance::new(); \
         inst.func2(); \
         assert_eq!(inst.func0(40), 0xaa); \
         assert_eq!(inst.func0(42), 0xcc); \
         inst.func3(); \
         assert_eq!(inst.func1(101), 0xbb); \
         assert_eq!(inst.func4(), 1); \
         assert_eq!(inst.func5(), 1);",
    );
}

#[test]
fn bulk_copy_within_a_64bit_memory_handles_overlap() {
    compile_run(
        "mem64_copy_overlap",
        r#"(module
            (memory i64 1)
            (data (i64.const 0) "\01\02\03\04")
            (func (export "copy") (param i64 i64 i64)
              local.get 0 local.get 1 local.get 2 memory.copy)
            (func (export "r8") (param i64) (result i32)
              local.get 0 i32.load8_u))"#,
        "let mut inst = Instance::new(); \
         inst.func0(2, 0, 4); \
         assert_eq!(inst.func1(2), 1); \
         assert_eq!(inst.func1(5), 4); \
         assert_eq!(inst.func1(1), 2);",
    );
}

#[test]
fn imported_64bit_memory_is_addressed_by_i64() {
    compile_run_raw(
        "mem64_imported",
        r#"(module
            (import "env" "mem" (memory i64 1))
            (data (i64.const 8) "\07")
            (func (export "r") (param i64) (result i32)
              local.get 0 i32.load8_u))"#,
        "struct Host { mem: Vec<u8> }\n\
         impl Imports for Host {\n\
             fn memory(&self) -> &[u8] { &self.mem }\n\
             fn memory_mut(&mut self) -> &mut Vec<u8> { &mut self.mem }\n\
         }\n\
         fn main() {\n\
             let mut inst = Instance::new(Host { mem: vec![0u8; 65536] });\n\
             assert_eq!(inst.func0(8), 7);\n\
             assert_eq!(inst.func0(9), 0);\n\
         }",
    );
}

#[test]
fn unsafe_memory_mode_reproduces_the_safe_result_on_a_64bit_memory() {
    // The opt-in unchecked lowering must accept i64 addresses/lengths too: the
    // in-bounds behaviour is identical to the safe path.
    let wat = r#"(module
        (memory i64 1)
        (func (export "w") (param i64 i32) local.get 0 local.get 1 i32.store)
        (func (export "r") (param i64) (result i32) local.get 0 i32.load)
        (func (export "fill") (param i64 i32 i64)
          local.get 0 local.get 1 local.get 2 memory.fill)
        (func (export "copy") (param i64 i64 i64)
          local.get 0 local.get 1 local.get 2 memory.copy))"#;
    let wasm = wat::parse_str(wat).unwrap();
    let unsafe_src = wasm2rs::transpile_with_options(
        &wasm,
        &wasm2rs::TranspileOptions {
            unsafe_memory: true,
            ..Default::default()
        },
    )
    .expect("transpile ok");
    assert!(unsafe_src.contains("get_unchecked"), "{unsafe_src}");
    let program = format!(
        "{unsafe_src}\nfn main() {{\n\
         let mut inst = Instance::new();\n\
         inst.func0(16, 0x0102_0304);\n\
         assert_eq!(inst.func1(16), 0x0102_0304);\n\
         inst.func2(32, 0x5a, 4);\n\
         inst.func3(64, 32, 4);\n\
         assert_eq!(inst.func1(64), 0x5a5a_5a5a);\n\
         }}\n"
    );
    let bin = common::build("mem64_unsafe", &program);
    let run = std::process::Command::new(&bin).status().unwrap();
    assert!(run.success());
}

#[test]
fn addresses_above_4gib_are_reachable() {
    // 65537 pages is 4 GiB + 64 KiB: the first memory size a 32-bit memory can
    // never have. The zeroed allocation is lazy, so only the touched pages cost
    // real memory. A store above 2^32 must land there and read back, while the
    // same low 32 bits (address 8) stay untouched — proof the address is not
    // truncated.
    compile_run(
        "mem64_above_4gib",
        r#"(module
            (memory i64 65537)
            (func (export "w") (param i64 i32)
              local.get 0 local.get 1 i32.store)
            (func (export "r") (param i64) (result i32)
              local.get 0 i32.load)
            (func (export "size") (result i64) memory.size))"#,
        "let mut inst = Instance::new(); \
         assert_eq!(inst.func2(), 65537); \
         inst.func0(0x1_0000_0008, 0x7eed_beef); \
         assert_eq!(inst.func1(0x1_0000_0008), 0x7eed_beef); \
         assert_eq!(inst.func1(8), 0);",
    );
}

#[test]
fn atomic_offset_above_4gib_is_accepted() {
    // A 64-bit memory's static offset is a u64, so an atomic access with an
    // offset above 2^32 must transpile like a plain load/store does. It traps at
    // run time here only because the memory is smaller than the offset.
    expect_trap(
        "mem64_atomic_big_offset",
        r#"(module
            (memory i64 1)
            (func (export "add") (param i64 i32) (result i32)
              local.get 0 local.get 1 i32.atomic.rmw.add offset=0x100000000))"#,
        "let mut inst = Instance::new(); inst.func0(0, 1);",
    );
}
