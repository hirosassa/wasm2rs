//! The runtime that generated crates embed, kept as real Rust files.
//!
//! Each file is emitted verbatim (`include_str!` in the renderers) into
//! self-contained generated code, which depends on `std` only. Compiling them
//! here, under `cfg(test)`, puts them under rustc and clippy and lets them be
//! unit-tested directly. They are `include!`d into one module per file rather
//! than declared as modules, because the helpers are private items that the
//! tests next to them must reach; rustfmt does not follow `include!`, so
//! `files_are_rustfmt_clean` checks them instead.
//!
//! The exception is `mem_helpers.tpl`: a placeholder template (`@NAME`, `@R`, ...)
//! that `helpers::mem_helper_template` slices into per-method sections. It is not
//! valid Rust, so rustc and rustfmt never see it; the helpers tests check its
//! structure and the e2e tests compile what it renders.
//!
//! Conventions for a `.rs` file:
//! - It is a compiled *instance*, never a text template with holes. A helper
//!   that exists for several types is written for `f32`; the renderer derives the
//!   others by replacing the token `f32` (see `widen` in `runtime.rs`).
//! - Anything that depends on the module or the options (a trap that is typed or
//!   a string, a count, a name) stays in the renderer and is not moved here.
//! - It may call helpers the renderer emits next to it; a stub next to it stands
//!   in for those so the file still compiles.
#![allow(
    dead_code,
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::restriction,
    reason = "emitted verbatim; helpers unused by some modules"
)]

mod delegate {
    include!("delegate.rs");
}

mod tc {
    include!("tc.rs");

    #[cfg(test)]
    mod tests {
        use super::Tc;

        #[test]
        fn runs_chained_continuations_to_completion() {
            let mut tc: Tc<u32, u32> = Tc::new();
            tc.set(|s, tc| {
                *s += 1;
                tc.set(|s, _| *s * 10);
                0
            });
            let mut state = 4;
            assert_eq!(tc.run(&mut state, 0), 50);
            // Nothing left to run: the result passes through.
            assert_eq!(tc.run(&mut state, 7), 7);
        }
    }
}

mod call_depth {
    /// Stands in for the cold trap helper the renderer emits beside `CallDepth`.
    fn trap_call_stack_exhausted() -> ! {
        panic!("call stack exhausted")
    }

    include!("call_depth.rs");

    #[cfg(test)]
    mod tests {
        use super::CallDepth;

        #[test]
        fn traps_at_the_limit_and_unwinds_on_drop() {
            let a = CallDepth::enter(2);
            let b = CallDepth::enter(2);
            assert!(std::panic::catch_unwind(|| CallDepth::enter(2)).is_err());
            drop(b);
            drop(CallDepth::enter(2));
            drop(a);
            // Back at depth 0, so the limit applies from scratch.
            let _a = CallDepth::enter(1);
        }
    }
}

mod float_min {
    include!("float_min.rs");

    #[cfg(test)]
    mod tests {
        use super::f32_min;

        #[test]
        fn propagates_nan_and_prefers_negative_zero() {
            assert!(f32_min(f32::NAN, 1.0).is_nan());
            assert!(f32_min(1.0, f32::NAN).is_nan());
            assert!(f32_min(0.0, -0.0).is_sign_negative());
            assert!(f32_min(-0.0, 0.0).is_sign_negative());
            assert_eq!(f32_min(1.0, 2.0), 1.0);
        }
    }
}

mod float_max {
    include!("float_max.rs");

    #[cfg(test)]
    mod tests {
        use super::f32_max;

        #[test]
        fn propagates_nan_and_prefers_positive_zero() {
            assert!(f32_max(f32::NAN, 1.0).is_nan());
            assert!(f32_max(1.0, f32::NAN).is_nan());
            assert!(f32_max(0.0, -0.0).is_sign_positive());
            assert!(f32_max(-0.0, 0.0).is_sign_positive());
            assert_eq!(f32_max(1.0, 2.0), 2.0);
        }
    }
}

mod float_quiet {
    include!("float_quiet.rs");

    #[cfg(test)]
    mod tests {
        use super::quiet_f32;

        #[test]
        fn sets_the_quiet_bit_only_on_nan() {
            // The payload is host-dependent (a folded add may canonicalise it), so
            // only the quiet bit is part of the contract.
            let signaling = f32::from_bits(0x7fa0_0000);
            let quieted = quiet_f32(signaling);
            assert!(quieted.is_nan());
            assert_ne!(quieted.to_bits() & 0x0040_0000, 0);
            assert_eq!(quiet_f32(1.5), 1.5);
        }
    }
}

mod i8x16_swizzle {
    include!("i8x16_swizzle.rs");

    #[cfg(test)]
    mod tests {
        use super::i8x16_swizzle;

        #[test]
        fn indexes_by_byte_and_zeroes_out_of_range() {
            let a = u128::from_le_bytes(core::array::from_fn(|i| 10 + i as u8));
            let b = u128::from_le_bytes([0, 15, 16, 255, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3]);
            let r = i8x16_swizzle(a, b).to_le_bytes();
            assert_eq!(&r[..5], &[10, 25, 0, 0, 13]);
        }
    }
}

mod i8x16_shuffle {
    include!("i8x16_shuffle.rs");

    #[cfg(test)]
    mod tests {
        use super::i8x16_shuffle;

        #[test]
        fn picks_from_the_concatenation_and_zeroes_indices_past_32() {
            let a = u128::from_le_bytes([1; 16]);
            let b = u128::from_le_bytes([2; 16]);
            let mut idx = [0u8; 16];
            idx[..5].copy_from_slice(&[0, 15, 16, 31, 32]);
            let r = i8x16_shuffle(a, b, u128::from_le_bytes(idx)).to_le_bytes();
            assert_eq!(&r[..5], &[1, 1, 2, 2, 0]);
        }
    }
}

mod i16x8_relaxed_dot {
    include!("i16x8_relaxed_dot.rs");

    #[cfg(test)]
    mod tests {
        use super::i16x8_relaxed_dot_i8x16_i7x16_s;

        #[test]
        fn sums_adjacent_products_and_saturates_into_i16() {
            let mut a = [0u8; 16];
            let mut b = [0u8; 16];
            a[..4].copy_from_slice(&[3, 4, 127, 127]);
            b[..4].copy_from_slice(&[5, 6, 127, 127]);
            let r = i16x8_relaxed_dot_i8x16_i7x16_s(u128::from_le_bytes(a), u128::from_le_bytes(b))
                .to_le_bytes();
            assert_eq!(i16::from_le_bytes([r[0], r[1]]), 39);
            assert_eq!(i16::from_le_bytes([r[2], r[3]]), 32258);
        }
    }
}

mod i32x4_relaxed_dot_add {
    include!("i32x4_relaxed_dot_add.rs");

    #[cfg(test)]
    mod tests {
        use super::i32x4_relaxed_dot_i8x16_i7x16_add_s;

        #[test]
        fn adds_the_accumulator_to_two_saturated_pair_sums_and_wraps() {
            let a = u128::from_le_bytes([0x80; 16]);
            let b = u128::from_le_bytes([0x80; 16]);
            let c = u128::from_le_bytes(
                [i32::MAX.to_le_bytes(), [1, 0, 0, 0], [0; 4], [0; 4]]
                    .concat()
                    .try_into()
                    .unwrap(),
            );
            let r = i32x4_relaxed_dot_i8x16_i7x16_add_s(a, b, c).to_le_bytes();
            let lane = |i: usize| i32::from_le_bytes(r[i * 4..i * 4 + 4].try_into().unwrap());
            // Each lane adds two sums saturated to 32767.
            assert_eq!(lane(0), i32::MAX.wrapping_add(65534));
            assert_eq!(lane(1), 65535);
        }
    }
}

mod shared_memory {
    include!("shared_memory.rs");

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn grow_raises_length_up_to_the_maximum_and_reports_old_pages() {
            let mem = SharedMemory::with_max(65536, 3 * 65536);
            assert_eq!(mem.grow(1), 1);
            assert_eq!(mem.grow(1), 2);
            assert_eq!(mem.grow(1), -1);
        }

        #[test]
        fn atomic_rmw_wraps_at_the_access_width() {
            let mem = SharedMemory::new(65536);
            mem.atomic_store(0, 1, 0xff);
            assert_eq!(mem.atomic_rmw(0, 1, 0, 2), 0xff);
            assert_eq!(mem.atomic_load(0, 1), 1);
        }

        #[test]
        fn cmpxchg_stores_only_on_match() {
            let mem = SharedMemory::new(65536);
            mem.atomic_store(8, 4, 7);
            assert_eq!(mem.atomic_cmpxchg(8, 4, 6, 9), 7);
            assert_eq!(mem.atomic_load(8, 4), 7);
            assert_eq!(mem.atomic_cmpxchg(8, 4, 7, 9), 7);
            assert_eq!(mem.atomic_load(8, 4), 9);
        }

        // Generated helpers each bind `bytes()` once, and a `memory.copy` / host
        // data init can hold more than one view at a time; none may be a whole-memory
        // `&mut [u8]` (UB under Stacked Borrows, checked by `cargo +nightly miri test`).
        #[test]
        fn simultaneous_views_do_not_alias() {
            let mem = SharedMemory::new(65536);
            let mut a = mem.bytes();
            let mut b = mem.bytes();
            a[0] = 1;
            b[1] = 2;
            a[0] = 3;
            b[2..4].copy_from_slice(&[4, 5]);
            assert_eq!((a[0], a[1], b[2], b[3]), (3, 2, 4, 5));
        }

        #[test]
        fn plain_access_interleaves_with_atomics() {
            let mem = SharedMemory::new(65536);
            let mut m = mem.bytes();
            m[0..4].copy_from_slice(&7u32.to_le_bytes());
            assert_eq!(mem.atomic_load(0, 4), 7);
            mem.atomic_store(4, 4, 9);
            assert_eq!(u32::from_le_bytes(m[4..8].try_into().unwrap()), 9);
            m[0] = 1;
            assert_eq!(mem.atomic_rmw(0, 4, 0, 1), 1);
            assert_eq!(m[0], 2);
        }

        #[test]
        fn copy_within_moves_overlapping_ranges_and_traps_out_of_bounds() {
            let mem = SharedMemory::new(65536);
            let mut m = mem.bytes();
            m[0..4].copy_from_slice(&[1, 2, 3, 4]);
            m.copy_within(0..4, 2);
            assert_eq!(&m[0..6], &[1, 2, 1, 2, 3, 4]);
            assert!(std::panic::catch_unwind(move || m.copy_within(0..4, 65534)).is_err());
        }

        #[test]
        fn view_out_of_bounds_panics() {
            let mem = SharedMemory::new(65536);
            let m = mem.bytes();
            assert!(std::panic::catch_unwind(|| m[65536]).is_err());
            assert!(std::panic::catch_unwind(|| m[65530..65537].len()).is_err());
            assert!(std::panic::catch_unwind(|| m[usize::MAX..usize::MAX].len()).is_err());
        }

        #[test]
        fn wait_times_out_and_mismatch_returns_not_equal() {
            let mem = SharedMemory::new(65536);
            assert_eq!(mem.wait(0, 1, 4, 0), 1);
            assert_eq!(mem.wait(0, 0, 4, 1_000), 2);
        }
    }
}

/// Files that are emitted as they are and not yet rustfmt-clean: formatting
/// them would change the generated output, so that is done on its own.
const NOT_FORMATTED: [&str; 2] = ["shared_memory.rs", "i32x4_relaxed_dot_add.rs"];

#[test]
fn files_are_rustfmt_clean() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/codegen/rt_src");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        if name == "mod.rs" || path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        if NOT_FORMATTED.contains(&name.as_str()) {
            continue;
        }
        let status = std::process::Command::new("rustfmt")
            .args(["--check", "--edition", "2024"])
            .arg(&path)
            .status()
            .expect("rustfmt is part of the toolchain");
        assert!(status.success(), "{name} is not rustfmt-clean");
        checked += 1;
    }
    assert!(checked > 0);
}
