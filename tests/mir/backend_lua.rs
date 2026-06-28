use crate::lower;
use mir::Backend;
use mir::backend::lua::LuaBackend;

#[test]
fn lua_simple_function() {
    let module = lower(
        r#"
        fun main() {
            let x = 42;
        }
        "#,
    );
    let mut backend = LuaBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("function main()"),
        "missing function: {}",
        result
    );
    assert!(result.contains("local"), "missing local: {}", result);
}

#[test]
fn lua_return_value() {
    let module = lower(
        r#"
        fun answer() -> i32 {
            return 42;
        }
        "#,
    );
    let mut backend = LuaBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("return"), "missing return: {}", result);
}

#[test]
fn lua_arithmetic() {
    let module = lower(
        r#"
        fun mul(a: i32, b: i32) -> i32 {
            return a * b;
        }
        "#,
    );
    let mut backend = LuaBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("function mul(a, b)"),
        "wrong signature: {}",
        result
    );
    assert!(result.contains("*"), "missing multiply: {}", result);
}

#[test]
fn lua_comparison() {
    let module = lower(
        r#"
        fun eq(a: i32, b: i32) -> bool {
            return a == b;
        }
        "#,
    );
    let mut backend = LuaBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("=="), "missing equality: {}", result);
}

#[test]
fn lua_bool_literal() {
    let module = lower(
        r#"
        fun truth() -> bool {
            return true;
        }
        "#,
    );
    let mut backend = LuaBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("true"), "missing bool: {}", result);
}

#[test]
fn lua_string_literal() {
    let module = lower(
        r#"
        fun greeting() -> str {
            return "hello";
        }
        "#,
    );
    let mut backend = LuaBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("hello"), "missing string: {}", result);
}

#[test]
fn lua_nil_as_unit() {
    let module = lower(
        r#"
        fun nothing() {}
        "#,
    );
    let mut backend = LuaBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("end"), "missing end: {}", result);
}

#[test]
fn lua_unary_not() {
    let module = lower(
        r#"
        fun negate(b: bool) -> bool {
            return !b;
        }
        "#,
    );
    let mut backend = LuaBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("not"), "missing 'not': {}", result);
}

#[test]
fn lua_multiple_functions() {
    let module = lower(
        r#"
        fun a() {}
        fun b() {}
        "#,
    );
    let mut backend = LuaBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("function a("), "missing a: {}", result);
    assert!(result.contains("function b("), "missing b: {}", result);
}

#[test]
fn lua_escape_analysis_produces_output() {
    let module = lower(
        r#"
        struct Data { value: i32 }

        fun escape() -> &Data {
            let local = Data { value: 1 };
            return &local;
        }
        "#,
    );
    let mut backend = LuaBackend::new();
    let result = backend.compile(&module).unwrap();
    // GC box: { _box = nil }
    assert!(result.contains("_box"), "missing GC box: {}", result);
    assert!(
        result.contains("function escape()"),
        "missing function: {}",
        result
    );
}
