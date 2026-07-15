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
fn c_static_impl_method_call_uses_mangled_name() {
    let module = lower(
        r#"
        struct Point {
            x: i32,
        }

        impl Point {
            fun new(x: i32) -> Point {
                Point { x }
            }
        }

        fun main() -> i32 {
            let p = Point::new(1);
            return p.x;
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();

    assert!(
        result.contains("new__Point"),
        "static impl method should be mangled:\n{}",
        result
    );
    assert!(
        !result.contains(" new("),
        "static impl method call used bare name:\n{}",
        result
    );
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
    // GC promotion: escaping local -> bundled Riddle GC.
    assert!(
        result.contains("rgc_alloc"),
        "missing rgc_alloc: {}",
        result
    );
    assert!(
        result.contains("void rgc_collect(void)"),
        "missing bundled GC runtime: {}",
        result
    );
    assert!(
        !result.contains("GC_MALLOC") && !result.contains("#include <gc.h>"),
        "Boehm GC should not be emitted: {}",
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
    // Non-escaping struct should stay on the stack and not pull in the GC runtime.
    assert!(
        !result.contains("rgc_alloc"),
        "non-escaping struct should not use rgc_alloc, got:\n{}",
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
fn c_raw_string_return_escapes_content() {
    let module = lower(
        r####"
        fun hello() -> &str {
            return r###"say "hi"
"###;
        }
        "####,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();

    assert!(
        result.contains("(riddle_str){ \"say \\\"hi\\\"\\n\", 9 }"),
        "raw string not escaped as C string:\n{}",
        result
    );
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
        extern "C" fun str_len(s: &str) -> usize;
        extern "C" fun str_byte(s: &str, idx: usize) -> u8;

        fun main() -> u8 {
            let s: &str = "abc";
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
fn c_backend_wraps_string_extern_returns() {
    let module = lower(
        r#"
        extern "C" fun greeting() -> &str;

        fun hello() -> &str {
            return greeting();
        }
        "#,
    );
    let result = CBackend::new().compile(&module).unwrap();
    assert!(
        result.contains("extern const char* greeting(void);")
            && result.contains("const char* ffi_str")
            && result.contains("(riddle_str){ ffi_str"),
        "extern string return was not wrapped:\n{result}"
    );
}

#[test]
fn c_backend_separates_defined_extern_string_abi_from_imports() {
    let module = lower(
        r#"
        extern "C" fun echo(value: &str) -> &str {
            value
        }

        fun call_echo() -> &str {
            echo("hello")
        }
        "#,
    );
    assert!(module.externs.iter().all(|ext| ext.name != "echo"));

    let result = CBackend::new().compile(&module).unwrap();
    assert!(
        result.contains("riddle_str echo(riddle_str p0);")
            && !result.contains("extern const char* echo")
            && !result.contains("const char* ffi_str"),
        "defined extern string function used the import ABI:\n{result}"
    );
}

#[test]
fn c_backend_compares_string_pattern_by_contents() {
    let module = lower(
        r#"
        fun is_hello(value: &str) -> bool {
            match value {
                "hello" => true,
                _ => false,
            }
        }
        "#,
    );
    let result = CBackend::new().compile(&module).unwrap();
    assert!(
        result.contains("memcmp("),
        "string comparison missing:\n{result}"
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

#[test]
fn c_backend_dispatches_trait_bound_method_in_generic_function() {
    let module = lower(
        r#"
        trait Named {
            fun name(&self) -> i32;
        }

        trait Tagged {
            fun tag(&self) -> i32;
        }

        struct User {
            id: i32,
            tag_value: i32,
        }

        impl Named for User {
            fun name(&self) -> i32 {
                self.id
            }
        }

        impl Tagged for User {
            fun tag(&self) -> i32 {
                self.tag_value
            }
        }

        fun read<T: Named + Tagged>(value: T) -> i32 {
            value.name() + value.tag()
        }

        fun main() -> i32 {
            let user = User { id: 7, tag_value: 2 };
            return read(user);
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("read__User"),
        "missing generic function monomorph:\n{}",
        result
    );
    assert!(
        result.contains("name__User("),
        "generic body should call concrete Named impl method:\n{}",
        result
    );
    assert!(
        result.contains("tag__User("),
        "generic body should call concrete Tagged impl method:\n{}",
        result
    );
}

#[test]
fn c_backend_lowers_non_copy_array_into_iterator() {
    let module = lower(
        r#"
        enum Option<T> {
            Some(T),
            None,
        }

        trait Iterator {
            type Item;
            fun next(&mut self) -> Option<Self::Item>;
        }

        trait IntoIterator {
            type Item;
            type IntoIter;
            fun into_iter(self) -> Self::IntoIter;
        }

        struct ArrayIter<T, const N: usize> {
            values: [T; N],
            index: usize,
        }

        impl<T, const N: usize> Iterator for ArrayIter<T, N> {
            type Item = T;

            fun next(&mut self) -> Option<Self::Item> {
                if self.index < N {
                    let value = self.values[self.index];
                    self.index += 1usize;
                    Option::Some(value)
                } else {
                    Option::None
                }
            }
        }

        impl<T, const N: usize> IntoIterator for [T; N] {
            type Item = T;
            type IntoIter = ArrayIter<T, N>;

            fun into_iter(self) -> Self::IntoIter {
                ArrayIter {
                    values: self,
                    index: 0usize,
                }
            }
        }

        struct Token {
            value: i32,
        }

        fun main() {
            for item in [Token { value: 1 }, Token { value: 2 }] {
                let next = item.value + 1;
            }
        }
        "#,
    );
    let mut backend = CBackend::new();
    let result = backend.compile(&module).unwrap();
    assert!(
        result.contains("into_iter__arr2_Token"),
        "missing array IntoIterator monomorph:\n{}",
        result
    );
    assert!(
        result.contains("next__ArrayIter_Token_2"),
        "missing ArrayIter::next monomorph:\n{}",
        result
    );
    assert!(
        result.contains("  ArrayIter_Token_2 s"),
        "array iterator construction lost its const argument:\n{}",
        result
    );
    assert!(
        result.contains("next__ArrayIter_Token_2((&"),
        "Iterator::next should receive the iterator slot by reference:\n{}",
        result
    );
    assert!(
        result.contains("Token values[2];"),
        "array field should use C array declarator:\n{}",
        result
    );
    assert!(
        result.contains("memcpy("),
        "array field initialization should copy array storage:\n{}",
        result
    );
}
