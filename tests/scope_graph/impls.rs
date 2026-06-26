use crate::{DefKind, build, resolve_paths};

#[test]
fn impl_method_body_references_are_encoded() {
    let sg = build(
        r#"
        struct S {}

        impl S {
            fun value(arg: i32) {
                arg;
            }
        }
        "#,
    );

    assert_eq!(resolve_paths(&sg, "arg"), vec![vec![DefKind::Param]]);
}

#[test]
fn associated_function_path_resolves_through_impl_scope() {
    let sg = build(
        r#"
        struct Point {}

        impl Point {
            fun new() -> Point {
                Point{}
            }
        }

        fun main() {
            Point::new();
        }
        "#,
    );

    assert_eq!(
        resolve_paths(&sg, "Point::new"),
        vec![vec![DefKind::Function]]
    );
}
