//! The string helpers in `examples/wasip2_host/filter.rs` that shape the typed host
//! (`typed.rs`) to a component's generated trait: `keep_declared_methods`, `error_type_of`
//! and `type_aliases`. They are pure, so they are tested directly with hand-written input.

include!("../../examples/wasip2_host/filter.rs");

/// `a`, an optional block of `b` for method `foo`, then `c`.
const ONE_BLOCK: &str = "a\n// optional: foo\nb\n// end optional\nc";

#[test]
fn optional_block_is_kept_when_its_method_is_declared() {
    let host = ONE_BLOCK;
    let generated = "fn foo(&mut self);";
    assert_eq!(keep_declared_methods(host, generated), "a\nb\nc");
}

#[test]
fn optional_block_is_dropped_when_its_method_is_not_declared() {
    let host = ONE_BLOCK;
    assert_eq!(keep_declared_methods(host, "fn bar(&mut self);"), "a\nc");
}

#[test]
fn optional_method_match_needs_the_open_paren_so_a_longer_name_does_not_count() {
    let host = ONE_BLOCK;
    assert_eq!(
        keep_declared_methods(host, "fn foo_bar(&mut self);"),
        "a\nc"
    );
    assert_eq!(keep_declared_methods(host, "let foo = 1;"), "a\nc");
}

#[test]
fn optional_alternatives_are_ored_and_whitespace_around_names_is_ignored() {
    let host = "// optional: foo | bar\nx\n// end optional";
    assert_eq!(keep_declared_methods(host, "fn bar(&mut self);"), "x");
    assert_eq!(keep_declared_methods(host, "fn foo(&mut self);"), "x");
    assert_eq!(keep_declared_methods(host, "fn foo(); fn bar();"), "x");
    assert_eq!(keep_declared_methods(host, "fn baz(&mut self);"), "");
}

#[test]
fn consecutive_optional_blocks_are_decided_independently() {
    let host = "// optional: a\n1\n// end optional\n// optional: b\n2\n// end optional\n3";
    assert_eq!(keep_declared_methods(host, "fn b("), "2\n3");
    assert_eq!(keep_declared_methods(host, "fn a("), "1\n3");
}

#[test]
fn malformed_input_a_missing_end_optional_drops_everything_to_the_next_marker_or_the_end() {
    // Without `// end optional` an undeclared block swallows the rest of the file ...
    let host = "a\n// optional: foo\nb\nc";
    assert_eq!(keep_declared_methods(host, "fn bar("), "a");
    // ... unless a later `// optional:` line re-decides, which can resume output.
    let host = "a\n// optional: foo\nb\n// optional: bar\nc";
    assert_eq!(keep_declared_methods(host, "fn bar("), "a\nc");
    // A declared block with no end keeps everything after it.
    assert_eq!(
        keep_declared_methods("// optional: foo\nb\nc", "fn foo("),
        "b\nc"
    );
}

#[test]
fn marker_lines_match_after_trimming_indentation() {
    let host = "    // optional: foo\n    x\n    // end optional\ny";
    assert_eq!(keep_declared_methods(host, "fn bar("), "y");
    assert_eq!(keep_declared_methods(host, "fn foo("), "    x\ny");
}

#[test]
fn error_type_is_read_from_a_simple_result() {
    let generated = "    fn p_read(&mut self, a: u32) -> Result<u64, ErrorCode>;\n";
    assert_eq!(error_type_of(generated, "p_").as_deref(), Some("ErrorCode"));
    let generated = "fn p_sync(&mut self) -> Result<(), ErrorCode2>;";
    assert_eq!(
        error_type_of(generated, "p_").as_deref(),
        Some("ErrorCode2")
    );
}

#[test]
fn error_type_ignores_commas_in_the_ok_type() {
    let generated = "fn p_read(&mut self) -> Result<(Vec<u8>, bool), StreamError>;";
    assert_eq!(
        error_type_of(generated, "p_").as_deref(),
        Some("StreamError")
    );
}

#[test]
fn compound_error_types_give_none_rather_than_a_wrong_name() {
    for sig in [
        "fn p_a(&mut self) -> Result<u64, Vec<u8>>;",
        "fn p_a(&mut self) -> Result<u64, Option<ErrorCode>>;",
    ] {
        assert_eq!(error_type_of(sig, "p_"), None, "{sig}");
    }
    // and the scan goes on to a later method with a plain error type
    let generated = "fn p_a(&mut self) -> Result<u64, Vec<u8>>;\n\
                     fn p_b(&mut self) -> Result<u64, ErrorCode>;";
    assert_eq!(error_type_of(generated, "p_").as_deref(), Some("ErrorCode"));
}

#[test]
fn infallible_and_non_result_methods_are_skipped() {
    let generated = "fn p_a(&mut self) -> u64;\nfn p_b(&mut self) -> Option<u64>;\n\
                     fn p_c(&mut self) -> Result<u64, E>;";
    assert_eq!(error_type_of(generated, "p_").as_deref(), Some("E"));
    assert_eq!(error_type_of("fn p_a(&mut self) -> u64;", "p_"), None);
}

#[test]
fn known_limitation_a_signature_wrapped_over_several_lines_is_not_recognised() {
    // documents the current behavior: the generator emits one-line signatures
    let generated = "fn p_a(\n    &mut self,\n    n: u32,\n) -> Result<u64, ErrorCode>;";
    assert_eq!(error_type_of(generated, "p_"), None);
}

#[test]
fn error_type_prefix_is_a_plain_starts_with_on_the_method_name() {
    let generated = "fn p_long_name(&mut self) -> Result<(), Long>;\n\
                     fn p_n(&mut self) -> Result<(), Short>;";
    // a prefix is not a whole method name: `p_` also hits the longer method first ...
    assert_eq!(error_type_of(generated, "p_").as_deref(), Some("Long"));
    // ... so callers pass a prefix long enough to be unambiguous (`p_n` finds only `p_n`)
    assert_eq!(error_type_of(generated, "p_n").as_deref(), Some("Short"));
    assert_eq!(error_type_of(generated, "p_x"), None);
    // the match is on the `fn ` keyword, so a mention elsewhere does not count
    assert_eq!(error_type_of("// fn p_a -> Result<(), E>;", "p_"), None);
}

const FS: &str =
    "fn wasi_filesystem_types_method_descriptor_read(&mut self) -> Result<u64, ErrorCode>;";
const TCP: &str = "fn wasi_sockets_tcp_bind(&mut self) -> Result<(), ErrorCode>;";
const UDP: &str = "fn wasi_sockets_udp_bind(&mut self) -> Result<(), ErrorCode3>;";
const LOOKUP: &str =
    "fn wasi_sockets_ip_name_lookup_resolve(&mut self) -> Result<u32, ErrorCode4>;";

#[test]
fn aliases_for_fs_only_net_only_both_and_neither() {
    assert_eq!(type_aliases(FS), "type FsErrorCode = ErrorCode;\n");
    assert_eq!(type_aliases(TCP), "type NetErrorCode = ErrorCode;\n");
    assert_eq!(
        type_aliases(&format!("{FS}\n{TCP}")),
        "type FsErrorCode = ErrorCode;\ntype NetErrorCode = ErrorCode;\n"
    );
    assert_eq!(
        type_aliases("fn unrelated(&mut self) -> Result<(), E>;"),
        ""
    );
    assert_eq!(type_aliases(""), "");
}

#[test]
fn net_alias_prefers_tcp_then_udp_then_ip_name_lookup() {
    let all = format!("{LOOKUP}\n{UDP}\n{TCP}");
    assert_eq!(type_aliases(&all), "type NetErrorCode = ErrorCode;\n");
    assert_eq!(
        type_aliases(&format!("{LOOKUP}\n{UDP}")),
        "type NetErrorCode = ErrorCode3;\n"
    );
    assert_eq!(type_aliases(LOOKUP), "type NetErrorCode = ErrorCode4;\n");
}

#[test]
fn numeric_suffix_names_are_used_as_generated() {
    let generated = "fn wasi_filesystem_types_method_descriptor_read(&mut self) -> Result<u64, ErrorCode2>;\n\
                     fn wasi_sockets_tcp_bind(&mut self) -> Result<(), ErrorCode>;";
    assert_eq!(
        type_aliases(generated),
        "type FsErrorCode = ErrorCode2;\ntype NetErrorCode = ErrorCode;\n"
    );
}

#[test]
fn a_prefix_with_only_a_compound_error_falls_through_to_the_next_interface() {
    let tcp = "fn wasi_sockets_tcp_x(&mut self) -> Result<(), Vec<u8>>;";
    assert_eq!(
        type_aliases(&format!("{tcp}\n{UDP}")),
        "type NetErrorCode = ErrorCode3;\n"
    );
}
