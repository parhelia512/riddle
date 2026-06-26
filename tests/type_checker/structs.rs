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
