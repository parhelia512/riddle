use crate::check;
use type_checker::Type;

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
fn rejects_str_value_binding() {
    let result = check(
        r#"
        fun main() {
            let s: str = "hello";
        }
        "#,
    );
    assert!(result.diagnostics.iter().any(|diag| diag.code == "E0043"));
}

#[test]
fn string_literal_has_shared_str_reference_type() {
    let result = check(
        r#"
        fun main() {
            let s = "hello";
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
    assert!(
        result
            .expr_types
            .values()
            .any(|ty| { ty == &Type::Ref(Box::new(Type::Str), false) })
    );
}

#[test]
fn rejects_string_literal_as_mutable_str_reference() {
    let result = check(
        r#"
        fun main() {
            let s: &mut str = "hello";
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diag| { diag.message.contains("expected &mut str, got &str") })
    );
}

#[test]
fn rejects_mutable_str_reference_from_shared_or_const_pointer() {
    let result = check(
        r#"
        fun invalid(shared: &str, pointer: *const str) {
            let from_shared: &mut str = &mut *shared;
            let from_const: &mut str = &mut *pointer;
        }
        "#,
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|diag| diag.code == "E0031")
            .count(),
        2
    );
}

#[test]
fn accepts_mutable_str_reference_from_mutable_sources() {
    let result = check(
        r#"
        fun valid(reference: &mut str, pointer: *mut str) {
            let from_reference: &mut str = &mut *reference;
            let from_pointer: &mut str = unsafe { &mut *pointer };
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn rejects_bare_str_in_value_type_declarations() {
    let result = check(
        r#"
        struct BadField { value: str }
        enum BadPayload { Tuple(str), Named { value: [str; 1] } }

        trait BadTrait {
            fun method(value: str) -> str;
            type Item = str;
        }

        extern "C" fun external(value: str) -> str;

        fun bad(value: (str, i32)) -> [str; 1] {
            return [""; 1];
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .filter(|diag| diag.code == "E0043")
            .count()
            >= 9
    );
}

#[test]
fn string_slices_support_builtin_equality() {
    let result = check(
        r#"
        fun main() {
            let lhs = "left";
            let rhs = "right";
            let same = lhs == rhs;
            let deref_same = *lhs == *rhs;
            let borrowed = &*lhs;
        }
        "#,
    );
    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn rejects_bare_str_rvalues() {
    let result = check(
        r#"
        fun invalid(left: &mut str, right: &str, cond: bool) {
            "value" as str;
            *left = *right;
            if cond { *left } else { *right };
            match *left { value => { value; } }
            *left = { return; };
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .filter(|diag| diag.code == "E0043")
            .count()
            >= 5
    );
}

#[test]
fn string_slice_match_requires_wildcard() {
    let result = check(
        r#"
        fun main() -> i32 {
            let value = "left";
            match value {
                "left" => 1,
            }
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diag| { diag.code == "E0039" && diag.message.contains("add a wildcard arm") })
    );
}

#[test]
fn rejects_bare_str_in_inferred_aggregate_and_generic_values() {
    let result = check(
        r#"
        fun take<T>(value: T) {}

        fun invalid(value: &str) {
            take(*value);
            [*value];
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .filter(|diag| diag.code == "E0043")
            .count()
            >= 2
    );
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
