use crate::analyze;

fn has_code(source: &str, code: &str) -> bool {
    analyze(source)
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == code)
}

fn is_clean(source: &str) {
    let result = analyze(source);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn multiple_shared_borrows_are_allowed() {
    is_clean(
        r#"
        fun f() {
            let value = 1;
            let first = &value;
            let second = &value;
            let third = first;
            *second;
            *third;
        }
        "#,
    );
}

#[test]
fn shared_then_mutable_borrow_is_rejected() {
    assert!(has_code(
        r#"
        fun f() {
            let mut value = 1;
            let shared = &value;
            let mutable = &mut value;
            *shared;
            *mutable = 2;
        }
        "#,
        "E0300"
    ));
}

#[test]
fn mutable_then_shared_borrow_is_rejected() {
    assert!(has_code(
        r#"
        fun f() {
            let mut value = 1;
            let mutable = &mut value;
            let shared = &value;
            *mutable = 2;
            *shared;
        }
        "#,
        "E0301"
    ));
}

#[test]
fn two_mutable_borrows_are_rejected() {
    assert!(has_code(
        r#"
        fun f() {
            let mut value = 1;
            let first = &mut value;
            let second = &mut value;
            *first = 2;
            *second = 3;
        }
        "#,
        "E0302"
    ));
}

#[test]
fn assigning_while_shared_borrowed_is_rejected() {
    assert!(has_code(
        r#"
        fun f() {
            let mut value = 1;
            let shared = &value;
            value = 2;
            *shared;
        }
        "#,
        "E0303"
    ));
}

#[test]
fn moving_while_mutably_borrowed_is_rejected() {
    assert!(has_code(
        r#"
        struct Token { value: i32 }

        fun f() {
            let mut token = Token { value: 1 };
            let reference = &mut token;
            let moved = token;
            *reference;
            moved.value;
        }
        "#,
        "E0304"
    ));
}

#[test]
fn disjoint_struct_fields_can_be_mutably_borrowed() {
    is_clean(
        r#"
        struct Pair { left: i32, right: i32 }

        fun f() {
            let mut pair = Pair { left: 1, right: 2 };
            let left = &mut pair.left;
            let right = &mut pair.right;
            *left = 3;
            *right = 4;
        }
        "#,
    );
}

#[test]
fn whole_struct_borrow_overlaps_a_field_borrow() {
    assert!(has_code(
        r#"
        struct Pair { left: i32, right: i32 }

        fun f() {
            let mut pair = Pair { left: 1, right: 2 };
            let field = &mut pair.left;
            let mut whole = &mut pair;
            *field = 3;
            whole.left = 4;
        }
        "#,
        "E0302"
    ));
}

#[test]
fn known_array_indices_can_be_mutably_borrowed_separately() {
    is_clean(
        r#"
        fun f() {
            let mut values = [1, 2, 3];
            let first = &mut values[0];
            let second = &mut values[1];
            *first = 4;
            *second = 5;
        }
        "#,
    );
}

#[test]
fn dynamic_array_indices_are_conservatively_overlapping() {
    assert!(has_code(
        r#"
        fun f(index: usize) {
            let mut values = [1, 2, 3];
            let first = &mut values[index];
            let second = &mut values[index];
            *first = 4;
            *second = 5;
        }
        "#,
        "E0302"
    ));
}

#[test]
fn mutable_reference_move_transfers_its_loan() {
    assert!(has_code(
        r#"
        fun f() {
            let mut value = 1;
            let first = &mut value;
            let second = first;
            *first = 2;
            *second = 3;
        }
        "#,
        "E0100"
    ));
}

#[test]
fn shared_reference_copy_keeps_both_names_usable() {
    is_clean(
        r#"
        fun f() {
            let value = 1;
            let first = &value;
            let second = first;
            *first;
            *second;
        }
        "#,
    );
}

#[test]
fn mutable_reference_by_value_parameter_moves_it() {
    assert!(has_code(
        r#"
        fun consume<T>(value: T) {}

        fun f() {
            let mut value = 1;
            let reference = &mut value;
            consume(reference);
            *reference = 2;
        }
        "#,
        "E0100"
    ));
}

#[test]
fn mutable_reference_ref_parameter_reborrows_it() {
    is_clean(
        r#"
        fun update(value: &mut i32) { *value += 1; }

        fun f() {
            let mut value = 1;
            let reference = &mut value;
            update(reference);
            update(reference);
        }
        "#,
    );
}

#[test]
fn shared_reborrow_blocks_mutation_until_last_shared_use() {
    assert!(has_code(
        r#"
        fun update(value: &mut i32) { *value += 1; }

        fun f() {
            let mut value = 1;
            let reference = &mut value;
            let shared = &*reference;
            update(reference);
            *shared;
        }
        "#,
        "E0300"
    ));
}

#[test]
fn shared_reborrow_allows_parent_after_last_use() {
    is_clean(
        r#"
        fun update(value: &mut i32) { *value += 1; }

        fun f() {
            let mut value = 1;
            let reference = &mut value;
            let shared = &*reference;
            *shared;
            update(reference);
        }
        "#,
    );
}

#[test]
fn mutable_reborrow_keeps_parent_frozen() {
    assert!(has_code(
        r#"
        fun update(value: &mut i32) { *value += 1; }

        fun f() {
            let mut value = 1;
            let parent = &mut value;
            let child = &mut *parent;
            update(parent);
            *child = 3;
        }
        "#,
        "E0302"
    ));
}

#[test]
fn mutable_reborrow_allows_parent_after_child_last_use() {
    is_clean(
        r#"
        fun update(value: &mut i32) { *value += 1; }

        fun f() {
            let mut value = 1;
            let parent = &mut value;
            let child = &mut *parent;
            *child = 3;
            update(parent);
        }
        "#,
    );
}

#[test]
fn shared_method_borrow_conflicts_with_live_mutable_return() {
    assert!(has_code(
        r#"
        struct Boxed { value: i32 }

        impl Boxed {
            fun get_mut(&mut self) -> &mut i32 { &mut self.value }
            fun read(&self) -> i32 { self.value }
        }

        fun f() {
            let mut boxed = Boxed { value: 1 };
            let reference = boxed.get_mut();
            boxed.read();
            *reference;
        }
        "#,
        "E0301"
    ));
}

#[test]
fn mutable_method_borrow_conflicts_with_live_shared_return() {
    assert!(has_code(
        r#"
        struct Boxed { value: i32 }

        impl Boxed {
            fun get(&self) -> &i32 { &self.value }
            fun set(&mut self) { self.value = 2; }
        }

        fun f() {
            let mut boxed = Boxed { value: 1 };
            let reference = boxed.get();
            boxed.set();
            *reference;
        }
        "#,
        "E0300"
    ));
}

#[test]
fn returned_reference_from_free_function_keeps_argument_borrowed() {
    assert!(has_code(
        r#"
        fun identity(value: &mut i32) -> &mut i32 { value }
        fun update(value: &mut i32) { *value += 1; }

        fun f() {
            let mut value = 1;
            let reference = identity(&mut value);
            update(&mut value);
            *reference;
        }
        "#,
        "E0302"
    ));
}

#[test]
fn returned_shared_reference_from_free_function_keeps_argument_shared() {
    assert!(has_code(
        r#"
        fun identity(value: &i32) -> &i32 { value }

        fun f() {
            let mut value = 1;
            let reference = identity(&value);
            let mutable = &mut value;
            *reference;
            *mutable;
        }
        "#,
        "E0300"
    ));
}

#[test]
fn returned_reference_through_nested_array_is_tracked() {
    assert!(has_code(
        r#"
        fun nested(value: &mut i32) -> [[&mut i32; 1]; 1] { [[value]] }
        fun update(value: &mut i32) { *value += 1; }

        fun f() {
            let mut value = 1;
            let result = nested(&mut value);
            update(&mut value);
            *result[0][0];
        }
        "#,
        "E0302"
    ));
}

#[test]
fn returned_reference_through_array_is_tracked() {
    assert!(has_code(
        r#"
        fun array(value: &mut i32) -> [&mut i32; 1] { [value] }
        fun update(value: &mut i32) { *value += 1; }

        fun f() {
            let mut value = 1;
            let result = array(&mut value);
            update(&mut value);
            *result[0];
        }
        "#,
        "E0302"
    ));
}

#[test]
fn branch_return_reference_keeps_all_possible_sources_borrowed() {
    assert!(has_code(
        r#"
        fun choose(flag: bool, left: &mut i32, right: &mut i32) -> &mut i32 {
            if flag { left } else { right }
        }
        fun update(value: &mut i32) { *value += 1; }

        fun f(flag: bool) {
            let mut left = 1;
            let mut right = 2;
            let reference = choose(flag, &mut left, &mut right);
            update(&mut left);
            *reference;
        }
        "#,
        "E0302"
    ));
}

#[test]
fn match_return_reference_keeps_possible_source_borrowed() {
    assert!(has_code(
        r#"
        enum Choice<T> { Some(T), None }

        impl<T> Choice<T> {
            fun unwrap_or(self, fallback: T) -> T {
                match self {
                    Choice::Some(value) => value,
                    Choice::None => fallback,
                }
            }
        }

        fun f() {
            let mut value = 1;
            let mut fallback = 2;
            let choice: Choice<&mut i32> = Choice::Some(&mut value);
            let reference = choice.unwrap_or(&mut fallback);
            let second = &mut value;
            *reference;
            *second;
        }
        "#,
        "E0302"
    ));
}

#[test]
fn assignment_replaces_reference_provenance() {
    is_clean(
        r#"
        fun update(value: &mut i32) { *value += 1; }

        fun f() {
            let mut left = 1;
            let mut right = 2;
            let mut reference = &mut left;
            reference = &mut right;
            *reference;
            update(&mut left);
        }
        "#,
    );
}

#[test]
fn two_mutable_call_arguments_to_same_place_are_rejected() {
    assert!(has_code(
        r#"
        fun pair(left: &mut i32, right: &mut i32) {}

        fun f() {
            let mut value = 1;
            pair(&mut value, &mut value);
        }
        "#,
        "E0302"
    ));
}

#[test]
fn two_mutable_call_arguments_to_disjoint_fields_are_allowed() {
    is_clean(
        r#"
        struct Pair { left: i32, right: i32 }
        fun pair(left: &mut i32, right: &mut i32) {}

        fun f() {
            let mut value = Pair { left: 1, right: 2 };
            pair(&mut value.left, &mut value.right);
        }
        "#,
    );
}

#[test]
fn two_shared_call_arguments_to_same_place_are_allowed() {
    is_clean(
        r#"
        fun pair(left: &i32, right: &i32) {}

        fun f() {
            let value = 1;
            pair(&value, &value);
        }
        "#,
    );
}

#[test]
fn inner_block_borrow_does_not_escape_without_a_value() {
    is_clean(
        r#"
        fun update(value: &mut i32) { *value += 1; }

        fun f() {
            let mut value = 1;
            {
                let reference = &mut value;
                *reference = 2;
            }
            update(&mut value);
        }
        "#,
    );
}

#[test]
fn inner_block_returned_borrow_does_escape() {
    assert!(has_code(
        r#"
        fun update(value: &mut i32) { *value += 1; }

        fun f() {
            let mut value = 1;
            let reference = {
                &mut value
            };
            update(&mut value);
            *reference;
        }
        "#,
        "E0302"
    ));
}

#[test]
fn unknown_external_reference_return_is_conservative() {
    let result = analyze(
        r#"
        extern "C" {
            fun choose(value: &mut i32) -> &mut i32;
        }
        fun update(value: &mut i32) { *value += 1; }

        fun f() {
            let mut value = 1;
            let reference = unsafe { choose(&mut value) };
            update(&mut value);
            *reference;
        }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0302"),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn borrow_conflict_reports_the_original_borrow_as_secondary() {
    let result = analyze(
        r#"
        fun f() {
            let mut value = 1;
            let first = &mut value;
            let second = &mut value;
            *first;
            *second;
        }
        "#,
    );
    let diagnostic = result
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "E0302")
        .expect("missing E0302");
    assert!(diagnostic.labels.len() >= 2, "{:?}", diagnostic.labels);
    assert!(diagnostic.labels.iter().any(|label| {
        label.style == type_checker::LabelStyle::Secondary && label.message.contains("first borrow")
    }));
}
