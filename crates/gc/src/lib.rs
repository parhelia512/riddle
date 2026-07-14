pub const RUNTIME_C: &str = include_str!("runtime.c");

#[cfg(test)]
mod tests {
    use super::RUNTIME_C;

    #[test]
    fn exports_runtime_api() {
        assert!(RUNTIME_C.contains("void rgc_init(void *stack_bottom)"));
        assert!(RUNTIME_C.contains("void *rgc_alloc(size_t size)"));
        assert!(RUNTIME_C.contains("void rgc_collect(void)"));
        assert!(!RUNTIME_C.contains("GC_MALLOC"));
        assert!(!RUNTIME_C.contains("<gc.h>"));
    }
}
