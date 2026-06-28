use crate::lower;
use mir::Backend;
use mir::backend::cranelift::CraneliftBackend;

#[test]
fn cranelift_simple_function() {
    let module = lower(
        r#"
        fun main() -> i32 {
            let x = 42;
            return x;
        }
        "#,
    );
    let mut backend = CraneliftBackend::new().expect("create cranelift backend");
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("wrote"), "unexpected output: {}", result);
    assert!(result.contains(".o"), "missing .o file: {}", result);
}

#[test]
fn cranelift_return_value() {
    let module = lower(
        r#"
        fun answer() -> i32 {
            return 42;
        }
        "#,
    );
    let mut backend = CraneliftBackend::new().expect("create cranelift backend");
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("wrote"), "compilation failed: {}", result);
}

#[test]
fn cranelift_arithmetic() {
    let module = lower(
        r#"
        fun add(a: i32, b: i32) -> i32 {
            return a + b;
        }
        "#,
    );
    let mut backend = CraneliftBackend::new().expect("create cranelift backend");
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("wrote"), "compilation failed: {}", result);
}

#[test]
fn cranelift_basic_blocks() {
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
    let mut backend = CraneliftBackend::new().expect("create cranelift backend");
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("wrote"), "compilation failed: {}", result);
}

#[test]
fn cranelift_comparison() {
    let module = lower(
        r#"
        fun lt(a: i32, b: i32) -> bool {
            return a < b;
        }
        "#,
    );
    let mut backend = CraneliftBackend::new().expect("create cranelift backend");
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("wrote"), "compilation failed: {}", result);
}

#[test]
fn cranelift_function_call() {
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
    let mut backend = CraneliftBackend::new().expect("create cranelift backend");
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("wrote"), "compilation failed: {}", result);
}

#[test]
fn cranelift_multiple_functions() {
    let module = lower(
        r#"
        fun a() -> i32 { return 1; }
        fun b() -> i32 { return 2; }
        "#,
    );
    let mut backend = CraneliftBackend::new().expect("create cranelift backend");
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("2 functions"), "expected 2 functions: {}", result);
}

#[test]
fn cranelift_struct_value() {
    let module = lower(
        r#"
        struct Point { x: i32, y: i32 }

        fun use_point() -> i32 {
            let p = Point { x: 1, y: 2 };
            return p.x;
        }
        "#,
    );
    let mut backend = CraneliftBackend::new().expect("create cranelift backend");
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("wrote"), "compilation failed: {}", result);
}
