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
