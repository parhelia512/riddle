use gc::RUNTIME_C;

#[test]
fn exports_runtime_api() {
    assert!(RUNTIME_C.contains("void rgc_init(void *stack_bottom)"));
    assert!(RUNTIME_C.contains("void *rgc_alloc(size_t size)"));
    assert!(RUNTIME_C.contains("void *rgc_realloc(void *ptr, size_t size)"));
    assert!(RUNTIME_C.contains("void rgc_free(void *ptr)"));
    assert!(RUNTIME_C.contains("void rgc_collect(void)"));
    assert!(!RUNTIME_C.contains("GC_MALLOC"));
    assert!(!RUNTIME_C.contains("<gc.h>"));
    assert!(!RUNTIME_C.contains("abort()"));
}
