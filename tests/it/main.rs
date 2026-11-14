//! Single integration-test harness.
//!
//! Every former `tests/<name>.rs` binary is now a submodule here, so the
//! whole suite links once instead of 70 times. `common` holds the shared
//! end-to-end helpers; each submodule reaches it via `use crate::common;`.
//!
//! Run a single former file with `cargo test --test it <name>` (the module
//! name prefixes each test, e.g. `cont::`).

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

mod bottom_ref_types;
mod common;
mod cont_casts;
mod cont_handles;
mod func_ref_casts;
mod golden;
mod single_file_pieces;

mod active_segment_init;
mod atomics;
mod atomics_threads;
mod bit_count;
mod block_type;
mod bounds_bench;
mod br_function_label;
mod br_table_try;
mod br_table_values;
mod build_cache;
mod build_helper;
mod bulk;
mod call_depth_limit;
mod call_indirect;
mod calls;
mod cli;
mod codegen_limits;
mod compile_and_run;
mod component_core_import_versions;
mod component_input;
mod component_link_adapter;
mod component_link_dylink;
mod component_link_exports;
mod component_link_rejections;
mod component_local_types;
mod component_p3_builtins;
mod component_real_async_guest;
mod component_real_custom_world;
mod component_real_moonbit;
mod component_wit_edges;
mod condition_simplify;
mod cont;
mod control_flow;
mod convert;
mod data;
mod data_compact;
mod dispatch_targets;
mod dump;
mod elem_exprs;
mod emscripten_pthread;
mod entry_exports;
mod exceptions;
mod exnref;
mod export_names;
mod extern_registry;
mod external_funcref;
mod externref;
mod extract_core;
mod flatten;
mod float;
mod funcref_dispatch;
mod gc_aggregate;
mod gc_array_seg;
mod gc_cast;
mod gc_convert;
mod gc_fields_funcref;
mod gc_heap;
mod gc_i31_unify;
mod gc_null;
mod gc_ref_reuse;
mod gc_refs;
mod gc_slot_size;
mod global_init;
mod global_offsets;
mod helper_naming;
mod host_memory_access;
mod i64;
mod imported_memory;
mod imported_table;
mod imports;
mod include_component_project;
mod inline_consumed;
mod int_arith;
mod labeled_block;
mod layout;
mod local_batching;
mod loop_params;
mod manifest;
mod memory32_wrap;
mod memory64;
mod memory_load;
mod module_linking;
mod multi_memory;
mod multi_table;
mod multivalue;
mod named_exports;
mod named_imports;
mod nan_rounding;
mod parallel_codegen;
mod passive;
mod programs;
mod redundant_parens;
mod reentrant_imports;
mod reference;
mod rotate_neg_receiver;
mod scenarios;
mod select_op;
mod simd;
mod simd_coverage;
mod simd_intrinsics;
mod spec_wast;
mod split;
mod split_dispatch;
mod state;
mod streaming;
mod table64;
mod table_max;
mod table_ops;
mod tail_call;
mod testcache_prune;
mod tier2;
mod trap_before_branch;
mod traps;
mod try_table;
mod typed_funcref_table;
mod unreachable;
mod unsafe_memory;
mod unsupported;
mod wasi;
mod wasi_fs;
mod wasi_generated;
mod wasi_guest_pointer_wrap;
mod wasi_poll;
mod wasi_sdk_e2e;
mod wasi_shared_memory;
mod wasi_threads;
mod wasip2_run_example;
mod wasip2_serve_example;
mod wasip2_typed_both;
mod wasip2_typed_cli;
mod wasip2_typed_clocks;
mod wasip2_typed_fs;
mod wasip2_typed_fs_edges;
mod wasip2_typed_fs_gaps;
mod wasip2_typed_fs_identity;
mod wasip2_typed_fs_symlink;
mod wasip2_typed_fs_write;
mod wasip2_typed_host;
mod wasip2_typed_http_fields;
mod wasip2_typed_http_incoming;
mod wasip2_typed_io_error;
mod wasip2_typed_name_lookup;
mod wasip2_typed_random;
mod wasip2_typed_send_queue;
mod wasip2_typed_sockets;
mod wasip2_typed_splice;
mod wasip2_typed_splice_permit;
mod wasip2_typed_stdin;
mod wasip2_typed_streams;
mod wasip2_typed_tcp_options;
mod wasip2_typed_udp;
mod wasip2_wit_regression;
mod wit_exports;
mod wit_glue;
mod wit_glue_exports;
mod wit_glue_memory;
mod wit_glue_utf16_imports;
mod wit_host;
mod wit_qualified;
