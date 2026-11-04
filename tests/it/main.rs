//! Single integration-test harness.
//!
//! Every former `tests/<name>.rs` binary is now a submodule here, so the
//! whole suite links once instead of 70 times. `common` holds the shared
//! end-to-end helpers; each submodule reaches it via `use crate::common;`.
//!
//! Run a single former file with `cargo test --test it <name>` (the module
//! name prefixes each test, e.g. `cont::`).

mod common;

mod atomics;
mod atomics_threads;
mod bit_count;
mod block_type;
mod bounds_bench;
mod br_function_label;
mod br_table_values;
mod build_cache;
mod build_helper;
mod bulk;
mod call_indirect;
mod calls;
mod cli;
mod codegen_limits;
mod compile_and_run;
mod component_input;
mod condition_simplify;
mod cont;
mod control_flow;
mod convert;
mod data;
mod dump;
mod emscripten_pthread;
mod entry_exports;
mod exceptions;
mod export_names;
mod extern_registry;
mod external_funcref;
mod externref;
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
mod global_init;
mod helper_naming;
mod host_memory_access;
mod i64;
mod imported_memory;
mod imported_table;
mod imports;
mod inline_consumed;
mod int_arith;
mod labeled_block;
mod layout;
mod local_batching;
mod loop_params;
mod manifest;
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
mod reference;
mod rotate_neg_receiver;
mod scenarios;
mod select_op;
mod simd;
mod simd_coverage;
mod spec_wast;
mod split;
mod split_dispatch;
mod state;
mod streaming;
mod table_ops;
mod tail_call;
mod tier2;
mod traps;
mod typed_funcref_table;
mod unreachable;
mod unsafe_memory;
mod unsupported;
mod wasi;
mod wasi_fs;
mod wasi_poll;
mod wasi_sdk_e2e;
mod wasi_shared_memory;
mod wasi_threads;
