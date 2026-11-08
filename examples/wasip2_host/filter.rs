/// The host source with the `// optional: <method>` blocks dropped unless the
/// generated trait declares that method (`// optional: a | b` keeps its block if it declares
/// any of them).
fn keep_declared_methods(host: &str, generated: &str) -> String {
    let mut out = Vec::new();
    let mut skipping = false;
    for line in host.lines() {
        if let Some(method) = line.trim().strip_prefix("// optional: ") {
            skipping = !method
                .split('|')
                .any(|m| generated.contains(&format!("fn {}(", m.trim())));
        } else if line.trim() == "// end optional" {
            skipping = false;
        } else if !skipping {
            out.push(line);
        }
    }
    out.join("\n")
}

/// The error type `X` of the first generated trait method whose name starts with `prefix`
/// and whose signature returns `Result<_, X>`, as the generator spelled it.
#[allow(dead_code, reason = "only the typed host callers use it")]
fn error_type_of(generated: &str, prefix: &str) -> Option<String> {
    let start = format!("fn {prefix}");
    generated.lines().find_map(|line| {
        let line = line.trim();
        if !line.starts_with(&start) {
            return None;
        }
        let (_, result) = line.split_once("-> Result<")?;
        let (_, name) = result.strip_suffix(">;")?.rsplit_once(", ")?;
        name.chars()
            .all(|c| c.is_ascii_alphanumeric())
            .then(|| name.to_string())
    })
}

/// The `X` of the generated `filesystem-error-code`, which returns `Option<X>`.
#[allow(dead_code, reason = "only the typed host callers use it")]
fn option_type_of(generated: &str) -> Option<String> {
    generated.lines().find_map(|line| {
        let line = line.trim();
        if !line.starts_with("fn wasi_filesystem_types_filesystem_error_code(") {
            return None;
        }
        let (_, option) = line.split_once("-> Option<")?;
        let name = option.strip_suffix(">;")?;
        name.chars().all(|c| c.is_ascii_alphanumeric()).then(|| name.to_string())
    })
}

/// `type FsErrorCode = ...;` and `type NetErrorCode = ...;` for the interfaces the generated
/// code declares (`NetErrorCode` from tcp, udp or ip-name-lookup, whichever it imports). The
/// `error-code` enums of `wasi:filesystem` and `wasi:sockets` get a numeric suffix when one
/// component imports both, so the typed host (`typed.rs`) names them through these aliases
/// instead of the generated spelling.
#[allow(dead_code, reason = "only the typed host callers use it")]
fn type_aliases(generated: &str) -> String {
    let mut out = String::new();
    // the first prefix the generated code has a fallible method for names the enum
    for (alias, prefixes) in [
        ("FsErrorCode", &["wasi_filesystem_types_method_descriptor_"][..]),
        (
            "NetErrorCode",
            &["wasi_sockets_tcp_", "wasi_sockets_udp_", "wasi_sockets_ip_name_lookup_"][..],
        ),
    ] {
        let name = prefixes.iter().find_map(|prefix| error_type_of(generated, prefix));
        let name = name.or_else(|| {
            // a component may import only `filesystem-error-code` (`-> Option<X>`)
            (alias == "FsErrorCode").then(|| option_type_of(generated)).flatten()
        });
        if let Some(name) = name {
            out.push_str(&format!("type {alias} = {name};\n"));
        }
    }
    out
}
