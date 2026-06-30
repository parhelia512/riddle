use crate::{check, messages};

#[test]
fn reports_let_initializer_mismatch() {
    let result = check(
        r#"
        fun f() {
            let x: bool = 1;
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|msg| msg.contains("let initializer type mismatch"))
    );
}

#[test]
fn reports_return_type_mismatch() {
    let result = check(
        r#"
        fun f() -> bool {
            return 1;
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|msg| msg.contains("return value type mismatch"))
    );
}

#[test]
fn checks_function_call_arguments() {
    let result = check(
        r#"
        fun takes_bool(flag: bool) -> bool {
            flag
        }

        fun main() {
            takes_bool(1);
            takes_bool(true, false);
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("function argument type mismatch"))
    );
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("expects 1 argument(s), got 2"))
    );
}

#[test]
fn ordered_comparison_reports_one_error_for_bad_operand_pair() {
    let result = check(
        r#"
        fun main() {
            let c = 'a';
            if c >= 1 { }
        }
        "#,
    );

    let msgs = messages(&result);
    assert_eq!(
        msgs.iter()
            .filter(|msg| msg.contains("ordered comparison requires compatible"))
            .count(),
        1
    );
}

#[test]
fn accepts_char_ordered_comparison() {
    let result = check(
        r#"
        fun main() {
            let c = 'a';
            if c >= '0' && c <= '9' { }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn compound_assignment_requires_mutable_lhs() {
    let result = check(
        r#"
        fun main() {
            let n = 1;
            n += 2;
        }
        "#,
    );

    assert!(result.diagnostics.iter().any(|diag| diag.code == "E0031"));
}

#[test]
fn array_literal_length_must_match_expected_array_type() {
    let result = check(
        r#"
        fun main() {
            let xs: [i32; 3] = [1, 2];
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|msg| msg.contains("array length mismatch: expected 3, got 2"))
    );
}

#[test]
fn array_repeat_length_must_match_expected_array_type() {
    let result = check(
        r#"
        fun main() {
            let xs: [i32; 2] = [1; 3];
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|msg| msg.contains("array length mismatch: expected 2, got 3"))
    );
}

#[test]
fn array_repeat_length_must_be_non_negative_literal() {
    let result = check(
        r#"
        fun main() {
            let n = 3;
            let xs = [1; n];
            let ys = [1; -1];
        }
        "#,
    );

    assert_eq!(
        messages(&result)
            .iter()
            .filter(|msg| {
                msg.contains("array repeat length must be a non-negative integer literal")
            })
            .count(),
        2
    );
}

#[test]
fn array_type_length_must_be_literal() {
    let result = check(
        r#"
        fun main() {
            let n = 3;
            let xs: [i32; n] = [1, 2, 3];
        }
        "#,
    );

    assert!(!result.diagnostics.is_empty());
    assert!(
        messages(&result)
            .iter()
            .any(|msg| msg.contains("invalid type annotation"))
    );
}

#[test]
fn nested_array_type_length_must_be_literal() {
    let result = check(
        r#"
        fun main() {
            let n = 3;
            let xs: ([i32; n]) = ([1, 2, 3]);
        }
        "#,
    );

    assert!(!result.diagnostics.is_empty());
    assert!(
        messages(&result)
            .iter()
            .any(|msg| msg.contains("invalid type annotation"))
    );
}

#[test]
fn unknown_type_annotation_is_reported() {
    let source = r#"
        fun main() {
            let value: KSK;
        }
        "#;
    let result = check(source);

    let diag = result
        .diagnostics
        .iter()
        .find(|diag| diag.message == "unknown type `KSK`")
        .expect("missing unknown type diagnostic");
    let label = diag.labels.first().expect("unknown type diagnostic label");
    let start = u32::from(label.range.start()) as usize;
    let end = u32::from(label.range.end()) as usize;
    assert_eq!(&source[start..end], "KSK");
}

#[test]
fn array_repeat_requires_copy_value() {
    let result = check(
        r#"
        struct Point { x: i32 }

        fun main() {
            let point = Point { x: 1 };
            let points: [Point; 3] = [point; 3];
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|msg| msg.contains("array repeat value must be Copy"))
    );
}

#[test]
fn nested_array_repeat_requires_copy_leaf_values() {
    let result = check(
        r#"
        struct Point { x: i32 }

        fun main() {
            let point = Point { x: 1 };
            let points: [[Point; 2]; 3] = [[point; 2]; 3];
        }
        "#,
    );

    assert!(
        messages(&result)
            .iter()
            .any(|msg| msg.contains("array repeat value must be Copy"))
    );
}
