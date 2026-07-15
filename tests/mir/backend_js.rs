use crate::lower;
use mir::Backend;
use mir::backend::js::JsBackend;

#[test]
fn js_simple_function() {
    let module = lower(
        r#"
        fun main() {
            let x = 42;
        }
        "#,
    );
    let mut backend = JsBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("function main()"),
        "missing function: {}",
        result
    );
    assert!(result.contains("let"), "missing let binding: {}", result);
}

#[test]
fn js_return_value() {
    let module = lower(
        r#"
        fun answer() -> i32 {
            return 42;
        }
        "#,
    );
    let mut backend = JsBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("return"), "missing return: {}", result);
}

#[test]
fn js_arithmetic() {
    let module = lower(
        r#"
        fun add(a: i32, b: i32) -> i32 {
            return a + b;
        }
        "#,
    );
    let mut backend = JsBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("function add(a, b)"),
        "wrong signature: {}",
        result
    );
}

#[test]
fn js_comparison() {
    let module = lower(
        r#"
        fun lt(a: i32, b: i32) -> bool {
            return a < b;
        }
        "#,
    );
    let mut backend = JsBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("<"), "missing comparison: {}", result);
}

#[test]
fn js_bool_literal() {
    let module = lower(
        r#"
        fun truth() -> bool {
            return true;
        }
        "#,
    );
    let mut backend = JsBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("true"), "missing bool literal: {}", result);
}

#[test]
fn js_string_literal() {
    let module = lower(
        r#"
        fun hello() -> &str {
            return "world";
        }
        "#,
    );
    let mut backend = JsBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("world"),
        "missing string literal: {}",
        result
    );
}

#[test]
fn js_function_call() {
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
    let mut backend = JsBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("square"), "missing callee name: {}", result);
}

#[test]
fn js_escape_analysis_uses_heap_alloc() {
    let module = lower(
        r#"
        struct Data { value: i32 }

        fun escape() -> &Data {
            let local = Data { value: 1 };
            return &local;
        }
        "#,
    );
    let mut backend = JsBackend::new();
    let result = backend.compile(&module).unwrap();
    // GC box: { _box: null }
    assert!(result.contains("_box"), "missing GC box: {}", result);
    assert!(
        result.contains("function escape"),
        "missing function: {}",
        result
    );
}

#[test]
fn js_multiple_functions() {
    let module = lower(
        r#"
        fun a() {}
        fun b() {}
        "#,
    );
    let mut backend = JsBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("function a()"), "missing a: {}", result);
    assert!(result.contains("function b()"), "missing b: {}", result);
}

#[test]
fn js_empty_function_output() {
    let module = lower(
        r#"
        fun nothing() {}
        "#,
    );
    let mut backend = JsBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("return;"),
        "empty function should have return: {}",
        result
    );
}
