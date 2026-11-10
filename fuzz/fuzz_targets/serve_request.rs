#![no_main]
//! Arbitrary bytes into the request reader: no panic, no hang, no big allocation, and the
//! same verdict as the reference model (which is checked in `check_request`).

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = wasm2rs_fuzz_serve::model::check_request(data);
});
