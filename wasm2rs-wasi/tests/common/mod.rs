//! Helpers shared by the host tests. Each test binary uses only some of them.
#![allow(dead_code, reason = "each test binary uses a subset of the helpers")]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use wasm2rs_wasi::{RandomError, RandomSource, WasiHost, WasiHostBuilder};

/// A random source of zeros: the tests do not depend on randomness, and with it they build
/// without the `os-random` feature.
pub struct Zeros;

impl RandomSource for Zeros {
    fn fill_random(&mut self, buf: &mut [u8]) -> Result<(), RandomError> {
        buf.fill(0);
        Ok(())
    }
    fn insecure_u64(&mut self) -> u64 {
        0
    }
    fn insecure_seed(&mut self) -> (u64, u64) {
        (0, 0)
    }
}

pub fn builder() -> WasiHostBuilder {
    WasiHost::builder().random(Zeros)
}

/// A fresh, empty directory under the system temp directory, unique to this process.
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wasm2rs-wasi-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whether `f` traps (panics).
pub fn traps<R>(f: impl FnOnce() -> R) -> bool {
    catch_unwind(AssertUnwindSafe(f)).is_err()
}
