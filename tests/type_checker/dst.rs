use crate::check;

#[test]
fn accepts_str_slice_param() {
    let result = check(
        r#"
        fun greet(name: &str) { }
        fun main() {
            greet("world");
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_str_slice_let_binding() {
    let result = check(
        r#"
        fun main() {
            let s: &str = "hello";
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_str_slice_return() {
    let result = check(
        r#"
        fun hello() -> &str {
            return "world";
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn str_variable_still_works() {
    // Backward compat: `str` as a fat pointer value type still allowed
    let result = check(
        r#"
        fun main() {
            let s: str = "hello";
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn rejects_str_to_int_ref_coercion() {
    let result = check(
        r#"
        fun main() {
            let x: &i32 = "hello";
        }
        "#,
    );
    assert!(!result.diagnostics.is_empty());
}

#[test]
fn accepts_str_slice_in_struct_field() {
    let result = check(
        r#"
        struct Message { label: &str }
        fun main() {
            let m = Message{ label: "greeting" };
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_nested_str_slice_ref() {
    let result = check(
        r#"
        fun show(s: &str) -> &str {
            return s;
        }
        fun main() {
            let s = "hi";
            show(s);
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}
