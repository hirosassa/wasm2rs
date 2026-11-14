#[allow(dead_code)]
struct Wasm2RsDelegate {
    skip: u32,
    payload: Box<dyn ::std::any::Any + Send>,
}
