//! `include_component!` includes the `{name}.rs` that a build script wrote into `OUT_DIR`.

mod included {
    wasm2rs_macros::include_component!("answer");
}

#[test]
fn the_generated_file_is_included() {
    assert_eq!(included::answer(), 42);
}
