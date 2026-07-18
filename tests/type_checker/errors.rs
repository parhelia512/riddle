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
fn validates_cast_source_and_target_types() {
    let accepted = check(
        r#"
        fun accepted(pointer: *const i32) {
            let integer = 1 as i64;
            let float = 1 as f64;
            let truncated = 1.5 as i32;
            let boolean = 1 as bool;
            let from_boolean = true as i32;
            let raw = 0 as *const i32;
            let mutable = pointer as *mut i32;
        }
        "#,
    );
    assert!(
        accepted
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "E0012"),
        "{:#?}",
        accepted.diagnostics
    );

    let rejected = check(
        r#"
        struct Point { x: i32 }

        fun rejected(point: Point) {
            let float = true as f64;
            let boolean = 1.5 as bool;
            let character = 'a' as i32;
            let aggregate = point as i32;
        }
        "#,
    );
    let diagnostics = rejected
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0012")
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 4, "{:#?}", rejected.diagnostics);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.message.starts_with("cannot cast `"))
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
fn checks_open_scalar_match_exhaustiveness() {
    let complete = check(
        r#"
        fun signed(value: i32) -> i32 {
            match value {
                0 => 0,
                _ => 1,
            }
        }

        fun unsigned(value: u8) -> i32 {
            match value {
                0 => 0,
                other => 1,
            }
        }

        fun decimal(value: f64) -> i32 {
            match value {
                0.0 => 0,
                _ => 1,
            }
        }

        fun character(value: char) -> i32 {
            match value {
                'a' => 0,
                _ => 1,
            }
        }
        "#,
    );
    assert_eq!(complete.diagnostics, vec![]);

    let incomplete = check(
        r#"
        fun signed(value: i32) -> i32 {
            match value { 0 => 0 }
        }

        fun unsigned(value: u8) -> i32 {
            match value { 0 => 0 }
        }

        fun decimal(value: f64) -> i32 {
            match value { 0.0 => 0 }
        }

        fun character(value: char) -> i32 {
            match value { 'a' => 0 }
        }
        "#,
    );
    let missing = incomplete
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0039")
        .collect::<Vec<_>>();
    assert_eq!(missing.len(), 4, "{:#?}", incomplete.diagnostics);
    assert!(
        missing
            .iter()
            .all(|diagnostic| diagnostic.message.contains("missing pattern `_`")),
        "{missing:#?}"
    );
}

#[test]
fn reports_uncovered_integer_ranges() {
    let result = check(
        r#"
        fun signed(value: i32) -> i32 {
            match value {
                0 => 0,
                2 => 2,
                2147483647 => 3,
            }
        }

        fun unsigned(value: u8) -> i32 {
            match value {
                0 => 0,
                2 => 2,
                4 => 4,
                255 => 5,
            }
        }

        fun guarded(value: u8, condition: bool) -> i32 {
            match value {
                _ if condition => 1,
            }
        }
        "#,
    );

    let diagnostics = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0039")
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 3, "{:#?}", result.diagnostics);

    assert_eq!(
        diagnostics[0].notes[0],
        "uncovered i32 ranges for `_`: `-2147483648..=-1`, `1`, `3..=2147483646`"
    );
    assert_eq!(
        diagnostics[1].notes[0],
        "uncovered u8 ranges for `_`: `1`, `3`, `5..=254`"
    );
    assert_eq!(
        diagnostics[2].notes[0],
        "uncovered u8 ranges for `_`: `0..=255`"
    );
}

#[test]
fn invalid_integer_patterns_do_not_cover_values() {
    let result = check(
        r#"
        fun wrong_suffix(value: u8) -> i32 {
            match value {
                0i32 => 0,
            }
        }

        fun overflow(value: u8) -> i32 {
            match value {
                256 => 0,
                _ => 1,
            }
        }
        "#,
    );

    let non_exhaustive = result
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "E0039")
        .unwrap();
    assert_eq!(
        non_exhaustive.notes[0],
        "uncovered u8 ranges for `_`: `0..=255`"
    );
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("integer literal `256` is out of range for `u8`")
    }));
}

#[test]
fn accepts_fully_enumerated_u8_match() {
    let arms = (0u16..=255)
        .map(|value| format!("{value} => {value},"))
        .collect::<String>();
    let result = check(&format!(
        "fun complete(value: u8) -> i32 {{ match value {{ {arms} }} }}"
    ));

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn reports_wide_integer_range_boundaries() {
    let result = check(
        r#"
        fun signed(value: i128) -> i32 {
            match value { 0 => 0 }
        }

        fun unsigned(value: u128) -> i32 {
            match value { 0 => 0 }
        }
        "#,
    );
    let diagnostics = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0039")
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 2, "{:#?}", result.diagnostics);
    assert_eq!(
        diagnostics[0].notes[0],
        "uncovered i128 ranges for `_`: `-170141183460469231731687303715884105728..=-1`, `1..=170141183460469231731687303715884105727`"
    );
    assert_eq!(
        diagnostics[1].notes[0],
        "uncovered u128 ranges for `_`: `1..=340282366920938463463374607431768211455`"
    );
}

#[test]
fn formats_nested_and_truncated_integer_ranges() {
    let result = check(
        r#"
        fun pair(value: (u8, u8)) -> i32 {
            match value {}
        }

        fun sparse(value: u8) -> i32 {
            match value {
                0 => 0,
                2 => 0,
                4 => 0,
                6 => 0,
                8 => 0,
                10 => 0,
                12 => 0,
                14 => 0,
                16 => 0,
                18 => 0,
            }
        }
        "#,
    );
    let diagnostics = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0039")
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 2, "{:#?}", result.diagnostics);
    assert_eq!(
        diagnostics[0].notes[0],
        "uncovered u8 ranges for integer position 1 in `(_, _)`: `0..=255`"
    );
    assert_eq!(
        diagnostics[0].notes[1],
        "uncovered u8 ranges for integer position 2 in `(_, _)`: `0..=255`"
    );
    assert_eq!(
        diagnostics[1].notes[0],
        "uncovered u8 ranges for `_`: `1`, `3`, `5`, `7`, `9`, `11`, `13`, `15`, and 2 more"
    );
}

#[test]
fn rejects_uncovered_enum_payload_patterns() {
    let result = check(
        r#"
        enum State { Ready, Done(i32) }

        fun main() -> i32 {
            match State::Ready {
                State::Ready => 1,
                State::Done(1) => 2,
            }
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diag| { diag.code == "E0039" && diag.message.contains("State::Done(_)") }),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn checks_nested_and_product_pattern_exhaustiveness() {
    let result = check(
        r#"
        enum Bit { Zero, One }
        enum Value { Flag(bool), Bit(Bit), Empty }

        fun enum_payload(value: Value) -> i32 {
            match value {
                Value::Flag(true) => 1,
                Value::Bit(Bit::Zero) => 2,
                Value::Empty => 3,
            }
        }

        fun pair(value: (bool, bool)) -> i32 {
            match value {
                (true, _) => 1,
                (false, true) => 2,
            }
        }

        fun single(value: (bool,)) -> i32 {
            match value {
                (true,) => 1,
            }
        }
        "#,
    );

    let missing = result
        .diagnostics
        .iter()
        .filter(|diag| diag.code == "E0039")
        .map(|diag| diag.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        missing
            .iter()
            .any(|message| message.contains("Value::Flag(false)")),
        "{missing:#?}"
    );
    assert!(
        missing
            .iter()
            .any(|message| message.contains("(false, false)")),
        "{missing:#?}"
    );
    assert!(
        missing.iter().any(|message| message.contains("(false,)")),
        "{missing:#?}"
    );
}

#[test]
fn checks_struct_and_generic_payload_exhaustiveness() {
    let result = check(
        r#"
        struct Flags { left: bool, right: bool }
        enum Maybe<T> { Some(T), None }

        fun flags(value: Flags) -> i32 {
            match value {
                Flags { left: true } => 1,
                Flags { left: false, right: true } => 2,
            }
        }

        fun maybe(value: Maybe<bool>) -> i32 {
            match value {
                Maybe::Some(true) => 1,
                Maybe::None => 0,
            }
        }
        "#,
    );

    let missing = result
        .diagnostics
        .iter()
        .filter(|diag| diag.code == "E0039")
        .map(|diag| diag.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        missing
            .iter()
            .any(|message| message.contains("Flags { left: false, right: false }")),
        "{missing:#?}"
    );
    assert!(
        missing
            .iter()
            .any(|message| message.contains("Maybe::Some(false)")),
        "{missing:#?}"
    );
}

#[test]
fn guarded_patterns_do_not_make_a_match_exhaustive() {
    let result = check(
        r#"
        fun choose(value: bool, condition: bool) -> i32 {
            match value {
                true if condition => 1,
                false => 2,
            }
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diag| { diag.code == "E0039" && diag.message.contains("true") }),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn accepts_exhaustive_nested_patterns_as_returning() {
    let result = check(
        r#"
        enum State { Ready, Done(bool) }
        enum Maybe<T> { Some(T), None }
        enum Box<T> { Wrap(T) }
        enum Void {}
        struct Flags { left: bool, right: bool }

        fun choose(state: State) -> i32 {
            match state {
                State::Ready => { return 0; },
                State::Done(true) => { return 1; },
                State::Done(false) => { return 2; },
            }
        }

        fun pair(value: (bool, bool)) -> i32 {
            match value {
                (true, _) => { return 1; },
                (false, _) => { return 2; },
            }
        }

        fun flags(value: Flags) -> i32 {
            match value {
                Flags { left: true } => { return 1; },
                Flags { left: false } => { return 2; },
            }
        }

        fun maybe(value: Maybe<bool>) -> i32 {
            match value {
                Maybe::Some(_) => { return 1; },
                Maybe::None => { return 0; },
            }
        }

        fun impossible(value: Void) -> i32 {
            match value {}
        }

        fun unit_value(value: ()) -> i32 {
            match value {
                () => { return 0; },
            }
        }

        fun nested_box(value: Box<Box<bool>>) -> i32 {
            match value {
                Box::Wrap(Box::Wrap(true)) => { return 1; },
                Box::Wrap(Box::Wrap(false)) => { return 0; },
            }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn unit_uses_empty_tuple_syntax() {
    let valid = check(
        r#"
        fun identity(value: ()) -> () {
            value
        }

        fun make() -> () {
            ()
        }

        fun main() {
            identity(());
            make();
        }
        "#,
    );
    assert_eq!(valid.diagnostics, vec![]);

    let old_alias = check("fun invalid(value: unit) {}");
    assert!(old_alias.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E0034" && diagnostic.message.contains("unknown type `unit`")
    }));
}

#[test]
fn ignores_uninhabited_constructor_spaces() {
    let result = check(
        r#"
        enum Void {}
        enum Outcome { Impossible(Void), Done }
        struct NeverFlags { impossible: Void, flag: bool }

        fun outcome(value: Outcome) -> i32 {
            match value {
                Outcome::Done => { return 1; },
            }
        }

        fun pair(value: (Void, bool)) -> i32 {
            match value {}
        }

        fun flags(value: NeverFlags) -> i32 {
            match value {}
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn checks_array_inhabitability_for_coverage() {
    let result = check(
        r#"
        enum Void {}

        fun impossible(value: [Void; 1]) -> i32 {
            match value {}
        }

        fun empty(value: [Void; 0]) -> i32 {
            match value {}
        }
        "#,
    );

    let diagnostics = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0039")
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 1, "{:#?}", result.diagnostics);
    assert!(diagnostics[0].message.contains("missing pattern `_`"));
}

#[test]
fn recursive_generic_constructor_space_terminates() {
    let result = check(
        r#"
        enum Grow<T> { Next(Grow<(T, T)>) }
        enum Loop { Again(Loop) }

        fun impossible(value: Grow<bool>) -> i32 {
            match value {}
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "E0039"),
        "{:#?}",
        result.diagnostics
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "E0072")
            .count(),
        2,
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn invalid_patterns_do_not_contribute_to_coverage() {
    let result = check(
        r#"
        enum Left { Same, Tuple(bool), Named { flag: bool } }
        enum Right { Same, Tuple(bool), Named { flag: bool } }
        enum Singleton { Only }
        struct Flags { flag: bool }

        fun wrong_enum(value: Left) -> i32 {
            match value {
                Right::Same => 1,
                Right::Tuple(_) => 2,
                Right::Named { flag: _ } => 3,
            }
        }

        fun wrong_field(value: Flags) -> i32 {
            match value {
                Flags { missing: _ } => 1,
            }
        }

        fun unknown_enum_owner(value: Singleton) -> i32 {
            match value {
                Missing::Only => 1,
            }
        }
        "#,
    );

    assert!(
        result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "E0038")
            .count()
            >= 5,
        "{:#?}",
        result.diagnostics
    );
    assert!(
        result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "E0039")
            .count()
            >= 3,
        "{:#?}",
        result.diagnostics
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
