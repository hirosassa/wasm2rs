/// The host source with the `// optional: <method>` blocks dropped unless the
/// generated trait declares that method.
fn keep_declared_methods(host: &str, generated: &str) -> String {
    let mut out = Vec::new();
    let mut skipping = false;
    for line in host.lines() {
        if let Some(method) = line.trim().strip_prefix("// optional: ") {
            skipping = !generated.contains(&format!("fn {method}("));
        } else if line.trim() == "// end optional" {
            skipping = false;
        } else if !skipping {
            out.push(line);
        }
    }
    out.join("\n")
}
