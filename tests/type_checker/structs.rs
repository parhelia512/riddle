use crate::{check, messages};

#[test]
fn checks_struct_literals_and_field_access() {
    let result = check(
        r#"
        struct Point {
            x: i32,
            y: bool,
        }

        fun main() -> i32 {
            let p = Point{x: 1, y: false};
            p.x
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn checks_struct_literal_shorthand_and_associated_function_call() {
    let result = check(
        r#"
        struct Point {
            x: i32,
            y: i64,
            label: str,
        }

        impl Point {
            fun new(x: i32, y: i64, label: str) -> Point {
                Point{x, y, label}
            }
        }

        fun main() -> i32 {
            let p = Point::new(1, 2, "origin");
            p.x
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn checks_generic_struct_literals_and_field_access() {
    let result = check(
        r#"
        struct Box<T> {
            value: T,
        }

        fun main() -> i32 {
            let b: Box<i32> = Box { value: 1 };
            b.value
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn checks_generic_impl_receiver_substitution() {
    let result = check(
        r#"
        struct Box<T> {
            value: T,
        }

        impl<T> Box<T> {
            fun get(&self) -> T {
                self.value
            }
        }

        fun main() -> i32 {
            let b: Box<i32> = Box { value: 1 };
            b.get()
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn checks_generic_impl_ref_return_substitution() {
    let result = check(
        r#"
        struct Box<T> {
            value: T,
        }

        impl<T> Box<T> {
            fun get(&self) -> &T {
                &self.value
            }
        }

        fun main() {
            let b: Box<i32> = Box { value: 1 };
            let value = b.get();
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn checks_nested_generic_type_args_without_spaces() {
    let result = check(
        r#"
        struct Box<T> {
            value: T,
        }

        fun main() -> i32 {
            let b: Box<Box<Box<i32>>> = Box { value: Box { value: Box { value: 1 } } };
            b.value.value.value
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn reports_struct_literal_field_errors() {
    let result = check(
        r#"
        struct Point {
            x: i32,
            y: bool,
        }

        fun main() {
            Point{x: true, z: 1};
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("struct field type mismatch"))
    );
    assert!(msgs.iter().any(|msg| msg.contains("unknown field `z`")));
    assert!(msgs.iter().any(|msg| msg.contains("missing field `y`")));
}

#[test]
fn reports_invalid_field_access() {
    let result = check(
        r#"
        struct Point {
            x: i32,
        }

        fun main() {
            let p = Point{x: 1};
            p.y;
            (1).x;
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(msgs.iter().any(|msg| msg.contains("unknown field `y`")));
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("cannot access field `x` on type i32"))
    );
}

#[test]
fn reports_generic_type_arg_count_mismatch() {
    let result = check(
        r#"
        struct Box<T> {
            value: T,
        }

        fun main() {
            let b: Box<i32, bool> = Box { value: 1 };
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("expects 1 type argument(s), got 2"))
    );
}

#[test]
fn reports_direct_recursive_struct_with_infinite_size() {
    let result = check(
        r#"
        struct KSK {
            x: KSK,
        }
        "#,
    );

    assert!(result.diagnostics.iter().any(|diag| {
        diag.code == "E0072"
            && diag
                .message
                .contains("recursive type `KSK` has infinite size")
    }));
}

#[test]
fn reports_indirect_recursive_struct_with_infinite_size() {
    let result = check(
        r#"
        struct A {
            b: B,
        }

        struct B {
            a: A,
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("recursive type `A` has infinite size"))
    );
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("recursive type `B` has infinite size"))
    );
}

#[test]
fn accepts_recursive_struct_behind_indirection() {
    let result = check(
        r#"
        struct Node {
            next: &Node,
        }

        struct RawNode {
            next: *const RawNode,
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}
