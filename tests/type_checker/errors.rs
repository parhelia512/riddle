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
fn reports_missing_return_path() {
    let result = check(
        r#"
        fun choose(flag: bool) -> i32 {
            if flag {
                return 1;
            }
            let done = true;
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("function return type mismatch")),
        "{msgs:#?}"
    );
}

#[test]
fn accepts_functions_when_every_path_returns() {
    let result = check(
        r#"
        fun choose(flag: bool) -> i32 {
            if flag {
                return 1;
            } else {
                return 2;
            }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn accepts_returning_and_value_branches_together() {
    let result = check(
        r#"
        fun choose(flag: bool) -> i32 {
            if flag {
                return 1;
            } else {
                2
            }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn validates_break_and_continue_loop_context() {
    let result = check(
        r#"
        fun valid() {
            while true {
                if true {
                    continue;
                }
                break;
            }

            for item in [1, 2, 3] {
                continue;
                break;
            }
        }

        fun invalid() {
            break;
            continue;
        }
        "#,
    );

    let diagnostics = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0042")
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 2, "{:#?}", result.diagnostics);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("`break`"))
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("`continue`"))
    );
}

#[test]
fn checks_boolean_match_return_paths() {
    let complete = check(
        r#"
        fun choose(flag: bool) -> i32 {
            match flag {
                true => { return 1; },
                false => { return 2; },
            }
        }
        "#,
    );
    assert_eq!(complete.diagnostics, vec![]);

    let incomplete = check(
        r#"
        fun choose(flag: bool) -> i32 {
            match flag {
                true => { return 1; },
            }
        }
        "#,
    );
    assert!(
        incomplete
            .diagnostics
            .iter()
            .any(|diag| diag.code == "E0039")
    );

    let integer = check(
        r#"
        fun choose(value: i32) -> i32 {
            match value {
                1 => { return 1; },
            }
        }
        "#,
    );
    assert!(integer.diagnostics.iter().any(|diag| diag.code == "E0039"));
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
fn reversed_array_type_order_reports_rust_style_suggestion() {
    let result = check(
        r#"
        struct Foo {
            x: [3; i32]
        }

        fun main() {
            let t = Foo {
                x: [1, 2, 3]
            };
        }
        "#,
    );

    let diag = result
        .diagnostics
        .iter()
        .find(|diag| diag.message.contains("invalid array type syntax"))
        .expect("missing reversed array type diagnostic");
    assert!(
        diag.notes
            .iter()
            .any(|note| note.contains("array types use `[T; N]`") && note.contains("`[i32; 3]`")),
        "{diag:?}"
    );
    let msgs = messages(&result);
    assert!(
        !msgs
            .iter()
            .any(|msg| msg.contains("struct field type mismatch")),
        "{msgs:?}"
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
