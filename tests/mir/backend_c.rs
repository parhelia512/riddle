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
    assert!(result.contains("main"), "missing main: {}", result);
    assert!(result.contains("return"), "missing return: {}", result);
    // ponytail: const 42 is inlined; since x is dead, no int32_t variable is emitted.
}

#[test]
fn c_backend_unit_main_returns_zero() {
    let module = lower(
        r#"
        fun main() {}
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("int main"),
        "main should return int:\n{}",
        result
    );
    assert!(
        result.contains("return 0;"),
        "unit main should return zero:\n{}",
        result
    );
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
fn c_compound_assignment_uses_updated_value() {
    let module = lower(
        r#"
        fun main() {
            let mut n: i32 = 1;
            n += 2;
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("+"), "missing compound add:\n{}", result);
    assert!(
        !result.contains("= 0;"),
        "compound assignment should not lower to zero:\n{}",
        result
    );
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

#[test]
fn c_str_slice_return() {
    let module = lower(
        r#"
        fun hello() -> &str {
            return "world";
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("struct { const char* ptr; size_t len; }"),
        "fat pointer struct not found:\n{}",
        result
    );
    assert!(result.contains("world"), "missing string:\n{}", result);
}

#[test]
fn c_str_slice_let() {
    let module = lower(
        r#"
        fun show(s: &str) { }
        fun main() {
            let s: &str = "hello";
            show(s);
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("struct { const char* ptr; size_t len; }"),
        "fat pointer struct not found for &str local:\n{}",
        result
    );
    assert!(result.contains("hello"), "missing string:\n{}", result);
}

#[test]
fn c_backend_preserves_returning_branches_and_mut_locals() {
    let module = lower(
        r#"
        fun starts_a(ch: char) -> bool {
            if ch == 'a' { return true; }
            false
        }

        fun main() {
            let mut go: bool = true;
            while go {
                go = false;
            }
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("'a'"),
        "char literal lowered wrong:\n{}",
        result
    );
    assert!(!result.contains("if ()"), "empty condition:\n{}", result);
    assert!(!result.contains("= ;"), "empty assignment rhs:\n{}", result);
    assert!(
        !result.contains("0 = false"),
        "unit fallback used as lvalue:\n{}",
        result
    );
}

#[test]
fn c_backend_assigns_if_phi_inputs_before_branching() {
    let module = lower(
        r#"
        fun choose(flag: bool) -> bool {
            if flag { true } else { false }
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(result.contains("phi"), "missing phi variable:\n{}", result);
    assert!(
        result.contains(" = true;") && result.contains(" = false;"),
        "phi inputs should be assigned on predecessor edges:\n{}",
        result
    );
}

#[test]
fn c_backend_provides_string_builtins() {
    let module = lower(
        r#"
        extern "C" fun str_len(s: str) -> usize;
        extern "C" fun str_byte(s: str, idx: usize) -> u8;

        fun main() -> u8 {
            let s: str = "abc";
            let _len = str_len(s);
            return str_byte(s, 1usize);
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("static inline size_t str_len"),
        "missing str_len builtin:\n{}",
        result
    );
    assert!(
        result.contains("static inline uint8_t str_byte"),
        "missing str_byte builtin:\n{}",
        result
    );
    assert!(
        !result.contains("extern int64_t str_len"),
        "str_len should be builtin:\n{}",
        result
    );
    assert!(
        !result.contains("extern int8_t str_byte"),
        "str_byte should be builtin:\n{}",
        result
    );
}

#[test]
fn c_backend_assigns_struct_field_with_associated_type_cast() {
    let module = lower(
        r#"
        struct Foo {
            x: i32,
            y: i64,
        }

        trait Bar {
            type X;
        }

        impl Bar for Foo {
            type X = i32;
        }

        fun main() {
            let mut q = Foo { x: 10, y: 20 };
            let r = 10 as Foo::X;
            q.x = r;
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains(".x ="),
        "field store should use .x:\n{}",
        result
    );
    assert!(
        !result.contains(".f0"),
        "field name fallback leaked:\n{}",
        result
    );
    assert!(
        !result.contains("((void)"),
        "associated type cast lowered to void:\n{}",
        result
    );
}

#[test]
fn c_backend_monomorphizes_generic_structs() {
    let module = lower(
        r#"
        struct Box<T> {
            value: T,
        }

        fun main() {
            let a: Box<i32> = Box { value: 1 };
            let b: Box<bool> = Box { value: true };
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("} Box_i32;"),
        "missing i32 monomorph:\n{}",
        result
    );
    assert!(
        result.contains("} Box_bool;"),
        "missing bool monomorph:\n{}",
        result
    );
    assert!(
        result.contains("int32_t value;") && result.contains("bool value;"),
        "field types were not substituted:\n{}",
        result
    );
}

#[test]
fn c_backend_accepts_nested_generic_type_args_without_spaces() {
    let module = lower(
        r#"
        struct Box<T> {
            value: T,
        }

        impl<T> Box<T> {
            fun get(&self) -> T {
                self.value
            }
        }

        fun main() {
            let b: Box<Box<i32>> = Box { value: Box { value: 1 } };
            let n = b.value.get();
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("Box_Box_i32"),
        "missing nested monomorph:\n{}",
        result
    );
    assert!(
        result.contains("get__Box_i32"),
        "missing monomorphized generic method:\n{}",
        result
    );
    assert!(
        !result.contains("0.f0"),
        "method receiver lowering lost outer function state:\n{}",
        result
    );
}

#[test]
fn c_backend_monomorphizes_generic_functions() {
    let module = lower(
        r#"
        fun id<T>(value: T) -> T {
            value
        }

        fun main() -> i32 {
            let a = id(1);
            let b = id(true);
            return a;
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("id__i32"),
        "missing i32 instance:\n{}",
        result
    );
    assert!(
        result.contains("id__bool"),
        "missing bool instance:\n{}",
        result
    );
    assert!(
        !result.contains(" id ("),
        "generic template should not be emitted directly:\n{}",
        result
    );
}
