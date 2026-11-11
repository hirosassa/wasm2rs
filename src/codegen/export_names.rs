//! Rust identifiers for `TranspileOptions::named_exports`: an export name is
//! sanitised into a snake_case identifier, then kept clear of Rust keywords and of
//! every name the generated code already defines, and made unique.

use std::collections::HashSet;

use super::helpers::{HELPER_ORDER, helper_name};
use super::runtime::is_rt_name;

/// Rust keywords (strict, reserved and `self`-like) that cannot be used as a
/// plain identifier.
const KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];

/// Names defined by the generated code itself, besides the pattern-based ones
/// handled in [`is_reserved`].
const FIXED: &[&str] = &[
    "new",
    "with_memory",
    "with_host",
    "memory",
    "shared_memory",
    "with_preopen_root",
    "imports",
    "main",
    "mem",
    "mem_mut",
    "table",
    "table_mut",
    "emscripten_call_start",
    "wasi_thread_start",
    "_emscripten_thread_init",
    "_emscripten_tls_init",
    "_emscripten_thread_exit",
];

/// `name` as a snake_case Rust identifier: camelCase words are split, anything
/// that is not alphanumeric becomes `_`, a leading digit gets a `_` prefix and an
/// empty or all-underscore result becomes `export`.
pub(super) fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_lower_or_digit = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() && prev_lower_or_digit {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower_or_digit = c.is_ascii_lowercase() || c.is_ascii_digit();
        } else {
            out.push('_');
            prev_lower_or_digit = false;
        }
    }
    if out.chars().all(|c| c == '_') {
        return "export".to_string();
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// `name` as a snake_case identifier that is also not a Rust keyword (a keyword gets a
/// trailing `_`), for a name that is not matched against the generated code's own
/// members, e.g. a parameter or a record field of a typed host trait.
pub(crate) fn plain_ident(name: &str) -> String {
    let ident = sanitize(name);
    if KEYWORDS.contains(&ident.as_str()) {
        format!("{ident}_")
    } else {
        ident
    }
}

/// Whether `name` starts with a prefix the generated code reserves.
fn has_reserved_prefix(name: &str) -> bool {
    ["wasi_", "cont_", "call_ref_t"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// Whether `name` is already a member the generated code defines (an inherent
/// method or a module-scope function), so a wrapper of that name would clash.
pub(super) fn is_reserved(name: &str) -> bool {
    if KEYWORDS.contains(&name) || FIXED.contains(&name) {
        return true;
    }
    // `func{N}` (and its `func{N}_part{k}` dispatch parts), `import{N}`,
    // `mem{N}`/`mem{N}_mut`, `memory{N}`, `table{N}`/`table{N}_mut`.
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let numbered = |prefix: &str| {
        name.strip_prefix(prefix).is_some_and(|rest| {
            let rest = rest.strip_suffix("_mut").unwrap_or(rest);
            // `func{N}_tc` is the trampolined body of a tail-calling function.
            let rest = match rest.strip_suffix("_tc") {
                Some(index) if prefix == "func" => index,
                _ => rest,
            };
            let rest = match rest.split_once("_part") {
                Some((index, part)) if prefix == "func" && all_digits(part) => index,
                _ => rest,
            };
            all_digits(rest)
        })
    };
    if ["func", "import", "mem", "memory", "table"]
        .iter()
        .any(|p| numbered(p))
    {
        return true;
    }
    if has_reserved_prefix(name) {
        return true;
    }
    // A memory helper (`r32`, `memory_grow`, …), possibly specialised to memory
    // `i > 0` (`r32_m1`), or a runtime helper.
    let base = name
        .rsplit_once("_m")
        .filter(|(_, d)| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        .map_or(name, |(b, _)| b);
    HELPER_ORDER.iter().any(|h| helper_name(*h) == base) || is_rt_name(name)
}

/// Assigns each export (name, function index) a unique, valid, non-clashing
/// identifier, in order. A keyword or generated-member clash appends `_` (or, for
/// a reserved prefix, prepends `export_`); a name already handed out appends
/// `_{function index}`.
pub(crate) fn assign<'a>(exports: impl IntoIterator<Item = (&'a str, u32)>) -> Vec<String> {
    assign_with(
        exports.into_iter().map(|(n, i)| (sanitize(n), i)),
        is_reserved,
        has_reserved_prefix,
        "export_",
    )
}

/// Unique identifiers for already-sanitised names: a name `reserved` rejects gets
/// `_` appended (or `moved_prefix` prepended when a trailing `_` would not help,
/// i.e. a reserved prefix), and a name already handed out appends `_{index}`.
fn assign_with(
    names: impl IntoIterator<Item = (String, u32)>,
    reserved: impl Fn(&str) -> bool,
    prefix_reserved: impl Fn(&str) -> bool,
    moved_prefix: &str,
) -> Vec<String> {
    let mut used: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (mut ident, index) in names {
        if reserved(&ident) {
            // A reserved prefix survives a trailing `_`, so move the name out of it.
            if prefix_reserved(&ident) {
                ident.insert_str(0, moved_prefix);
            } else {
                ident.push('_');
            }
        }
        while used.contains(&ident) {
            ident = format!("{ident}_{index}");
        }
        used.insert(ident.clone());
        out.push(ident);
    }
    out
}

/// An import's `Imports` method name: `{module}_{field}` with a version suffix
/// (`@0.2.0`) dropped from the module, snake_cased, runs of `_` collapsed and the
/// ends trimmed; `import` when nothing is left.
fn import_ident(module: &str, field: &str) -> String {
    let module = module.split('@').next().unwrap_or(module);
    let part = |s: &str| {
        // `sanitize` maps a symbol-only name to `export`; here it is just empty.
        if s.chars().any(|c| c.is_ascii_alphanumeric()) {
            sanitize(s)
        } else {
            String::new()
        }
    };
    let joined = format!("{}_{}", part(module), part(field));
    let mut out = String::with_capacity(joined.len());
    for c in joined.chars() {
        if c != '_' || !out.ends_with('_') {
            out.push(c);
        }
    }
    let out = out.trim_matches('_');
    if out.is_empty() {
        "import".to_string()
    } else if out.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{out}")
    } else {
        out.to_string()
    }
}

/// The prefix `import_ident` gives to the functions of the interface `module`
/// (`wasi:filesystem/types@0.2.0` is `wasi_filesystem_types`).
pub(crate) fn interface_ident(module: &str) -> String {
    import_ident(module, "")
}

/// Whether `name` is `{prefix}{digits}`, e.g. the generated `get_global0`.
fn numbered(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// `Imports` method names for `(module, field, function index)` imports, in order
/// (`TranspileOptions::named_imports`). Kept clear of keywords, the other trait
/// members (`memory`, `table`, `table{i}`, `get_global{k}`, `call_ref_t{n}`, ...; the `Instance`'s own
/// members are in another namespace and cannot clash) and each
/// other.
pub(crate) fn assign_import_names<'a>(
    imports: impl IntoIterator<Item = (&'a str, &'a str, u32)>,
) -> Vec<String> {
    assign_with(
        imports.into_iter().map(|(m, f, i)| (import_ident(m, f), i)),
        |n| {
            KEYWORDS.contains(&n)
                || ["memory", "memory_mut", "table", "table_mut"].contains(&n)
                || numbered(n, "get_global")
                || numbered(n, "set_global")
                || numbered(n, "table")
                || n.strip_suffix("_mut").is_some_and(|t| numbered(t, "table"))
                || n.starts_with("call_ref_t")
        },
        |n| n.starts_with("call_ref_t"),
        "import_",
    )
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn sanitize_makes_snake_case_identifiers() {
        assert_eq!(sanitize("add"), "add");
        assert_eq!(sanitize("my-func"), "my_func");
        assert_eq!(sanitize("myFunc"), "my_func");
        assert_eq!(sanitize("HTTPServer"), "httpserver");
        assert_eq!(sanitize("1st"), "_1st");
        assert_eq!(sanitize(""), "export");
        assert_eq!(sanitize("--"), "export");
        assert_eq!(sanitize("a.b"), "a_b");
    }

    #[test]
    fn import_names_avoid_the_imported_table_methods() {
        // Imported table `i > 0` adds `table{i}`/`table{i}_mut` to the `Imports` trait.
        let names = assign_import_names([("table1", "", 0), ("table2", "mut", 1)]);
        for n in &names {
            assert!(
                n != "table1" && n != "table2_mut",
                "import named like a table method: {names:?}"
            );
        }
    }

    #[test]
    fn reserved_covers_keywords_members_and_helpers() {
        for name in [
            "type",
            "new",
            "memory",
            "func0",
            "func12",
            "func0_tc",
            "trap_call_stack_exhausted",
            "table1_mut",
            "r32",
            "r32_m2",
        ] {
            assert!(is_reserved(name), "{name}");
        }
        for name in ["add", "func", "funcs", "table_x", "run"] {
            assert!(!is_reserved(name), "{name}");
        }
    }

    #[test]
    fn assign_terminates_for_prefix_reserved_names() {
        let got = assign([
            ("wasi_x", 1),
            ("cont_y", 2),
            ("call_ref_t3", 3),
            ("wasi_x", 4),
        ]);
        assert_eq!(
            got,
            [
                "export_wasi_x",
                "export_cont_y",
                "export_call_ref_t3",
                "export_wasi_x_4"
            ]
        );
        assert!(got.iter().all(|n| !is_reserved(n)));
    }

    #[test]
    fn import_names_of_symbol_only_parts_never_say_export() {
        let got = assign_import_names([("env", "--", 0), ("..", "f", 1), ("-", "-", 2)]);
        assert_eq!(got, ["env", "f", "import"]);
    }

    #[test]
    fn import_names_are_collapsed_versionless_and_unique() {
        let got = assign_import_names([
            ("wasi:io/streams@0.2.0", "[method]output-stream.write", 0),
            ("env", "log", 1),
            ("", "memory", 2),
            ("x", "get_global0", 3),
            ("a", "b", 4),
            ("a_b", "", 5),
            ("", "", 6),
        ]);
        assert_eq!(
            got,
            [
                "wasi_io_streams_method_output_stream_write",
                "env_log",
                "memory_",
                "x_get_global0",
                "a_b",
                "a_b_5",
                "import"
            ]
        );
    }

    #[test]
    fn assign_keeps_names_unique() {
        let got = assign([("a.b", 3), ("a_b", 4), ("type", 5), ("a_b", 6)]);
        assert_eq!(got, ["a_b", "a_b_4", "type_", "a_b_6"]);
    }
}
