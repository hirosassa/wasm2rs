fn main() {
    let depth = 50;
    let mut body = String::new();
    for _ in 0..depth {
        body.push_str(
            "(block (local.set 0 (i32.add (local.get 0) (i32.const 1))) (br_if 0 (i32.const 0))\n",
        );
    }
    body.push_str(&")".repeat(depth));
    let wat =
        format!("(module (func (export \"f\") (result i32) (local i32)\n{body}\n(local.get 0)))");
    let wasm = wat::parse_str(&wat).expect("valid wat");
    let source = wasm2rs::transpile(&wasm).expect("transpile ok");
    println!("{}", source);
}
