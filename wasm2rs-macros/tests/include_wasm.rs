//! `include_wasm!` expands to callable generated code.

mod plain {
    wasm2rs_macros::include_wasm!("$OUT_DIR/add.wasm");
}

mod named {
    wasm2rs_macros::include_wasm!("$OUT_DIR/add.wasm", named_exports,);
}

#[test]
fn the_expansion_is_callable() {
    assert_eq!(plain::func0(2, 3), 5);
}

#[test]
fn named_exports_option_adds_the_wrapper() {
    assert_eq!(named::add(40, 2), 42);
}
