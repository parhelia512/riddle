use gc::{NO_GC_RUNTIME_C, RUNTIME_C};

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

#[test]
fn exports_an_allocator_only_runtime() {
    for symbol in ["riddle_alloc", "riddle_realloc", "riddle_free"] {
        assert!(NO_GC_RUNTIME_C.contains(symbol), "missing {symbol}");
    }
    for forbidden in ["rgc_", "RgcHeader", "collect", "stack_bottom"] {
        assert!(
            !NO_GC_RUNTIME_C.contains(forbidden),
            "no-GC runtime contains {forbidden}"
        );
    }
}
