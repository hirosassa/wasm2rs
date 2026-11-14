thread_local! {
    static CALL_DEPTH: ::std::cell::Cell<u32> = const { ::std::cell::Cell::new(0) };
}
struct CallDepth;
impl CallDepth {
    #[inline]
    fn enter(limit: u32) -> CallDepth {
        let depth = CALL_DEPTH.get();
        if depth >= limit {
            trap_call_stack_exhausted();
        }
        CALL_DEPTH.set(depth + 1);
        CallDepth
    }
}
impl Drop for CallDepth {
    #[inline]
    fn drop(&mut self) {
        CALL_DEPTH.set(CALL_DEPTH.get() - 1);
    }
}
