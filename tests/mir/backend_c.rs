use crate::lower;
use mir::Backend;
use mir::backend::c::CBackend;

#[test]
fn c_simple_function() {
    let module = lower(
        r#"
        fun main() {
            let x = 42;
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("int32_t"), "missing C type: {}", result);
    assert!(result.contains("return"), "missing return: {}", result);
}

#[test]
fn c_return_value() {
    let module = lower(
        r#"
        fun answer() -> i32 {
            return 42;
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("int32_t"), "missing int type: {}", result);
    assert!(result.contains("return"), "missing return: {}", result);
}

#[test]
fn c_arithmetic() {
    let module = lower(
        r#"
        fun add(a: i32, b: i32) -> i32 {
            return a + b;
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("+"), "missing +: {}", result);
}

#[test]
fn c_basic_blocks() {
    let module = lower(
        r#"
        fun choose(flag: bool) -> i32 {
            if flag {
                return 1;
            }
            return 0;
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("if"), "missing if: {}", result);
    assert!(result.contains("goto"), "missing goto: {}", result);
}

#[test]
fn c_comparison() {
    let module = lower(
        r#"
        fun lt(a: i32, b: i32) -> bool {
            return a < b;
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("bool"), "missing bool type: {}", result);
    assert!(result.contains("<"), "missing <: {}", result);
}

#[test]
fn c_function_call() {
    let module = lower(
        r#"
        fun square(n: i32) -> i32 {
            return n * n;
        }

        fun main() -> i32 {
            return square(5);
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("square"), "missing callee: {}", result);
}

#[test]
fn c_heap_alloc() {
    let module = lower(
        r#"
        struct Data { value: i32 }

        fun escape() -> &Data {
            let local = Data { value: 1 };
            return &local;
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    // GC promotion: escaping local → GC_MALLOC
    assert!(
        result.contains("GC_MALLOC"),
        "missing GC_MALLOC: {}",
        result
    );
}

#[test]
fn c_multiple_functions() {
    let module = lower(
        r#"
        fun a() {}
        fun b() {}
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains(" a (void)"), "missing a: {}", result);
    assert!(result.contains(" b (void)"), "missing b: {}", result);
}

#[test]
fn c_backend_local_var_has_init_value() {
    let module = lower(
        r#"
        fun main() -> i32 {
            let x = 42;
            return x;
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("42"),
        "local should be initialized with 42, got:\n{}",
        result
    );
}

#[test]
fn c_backend_alloca_for_non_escaping_struct() {
    let module = lower(
        r#"
        struct Point { x: i32, y: i32 }

        fun use_point() -> i32 {
            let p = Point { x: 1, y: 2 };
            return p.x;
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    // Non-escaping struct should NOT use GC_MALLOC
    assert!(
        !result.contains("GC_MALLOC"),
        "non-escaping struct should not use GC_MALLOC, got:\n{}",
        result
    );
    assert!(result.contains("return"), "missing return");
}
