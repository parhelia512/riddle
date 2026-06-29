use crate::{check, messages};

#[test]
fn accepts_matching_trait_impl_required_items() {
    let result = check(
        r#"
        trait Show {
            fun show(value: i32) -> str;
            type Output;
            type Default = bool;
        }

        struct Widget {}

        impl Show for Widget {
            fun show(value: i32) -> str {
                "ok"
            }

            type Output = i32;

            fun helper() -> i32 {
                1
            }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn reports_trait_impl_contract_mismatches() {
    let result = check(
        r#"
        trait Convert {
            fun value(input: i32) -> bool;
            type Item;
        }

        struct Source {}

        impl Convert for Source {
            fun value(input: bool) -> i32 {
                1
            }
        }
        "#,
    );

    let msgs = messages(&result);
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("parameter 1 type mismatch"))
    );
    assert!(msgs.iter().any(|msg| msg.contains("return type mismatch")));
    assert!(
        msgs.iter()
            .any(|msg| msg.contains("missing associated type `Item`"))
    );
}

#[test]
fn accepts_inherent_method_call_with_self_receiver() {
    let result = check(
        r#"
        enum Foo {
            A,
            B,
        }

        impl Foo {
            fun get(&self) -> &str {
                if *self == Foo::A {
                    "A"
                } else {
                    "B"
                }
            }
        }

        fun main() {
            let x = Foo::A;
            let t = x.get();
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}

#[test]
fn mutable_self_method_requires_mutable_receiver_binding() {
    let result = check(
        r#"
        struct Cell {
            value: i32,
        }

        impl Cell {
            fun set(&mut self, value: i32) {
                self.value = value;
            }
        }

        fun main() {
            let cell = Cell { value: 1 };
            cell.set(42);
        }
        "#,
    );

    assert!(result.diagnostics.iter().any(|diag| diag.code == "E0031"));
}

#[test]
fn mutable_reference_requires_mutable_binding() {
    let result = check(
        r#"
        struct Cell {
            value: i32,
        }

        fun main() {
            let cell = Cell { value: 1 };
            let ref_cell = &mut cell;
        }
        "#,
    );

    assert!(result.diagnostics.iter().any(|diag| diag.code == "E0031"));
}

#[test]
fn resolves_impl_associated_type_path() {
    let result = check(
        r#"
        struct Foo {}

        trait Bar {
            type X;
        }

        impl Bar for Foo {
            type X = i32;
        }

        fun main() {
            let r = 10 as Foo::X;
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
    assert!(
        result
            .expr_types
            .values()
            .any(|ty| matches!(ty, type_checker::Type::Int(type_checker::IntTy::I32)))
    );
}

#[test]
fn accepts_outer_attributes_on_common_ast_nodes() {
    let result = check(
        r#"
        #[item]
        struct Boxed {
            #[field]
            value: i32,
        }

        #[item]
        enum Maybe {
            #[variant]
            Some(#[variant_ty] i32),
            None,
        }

        #[item]
        fun id(#[param] value: #[ty] i32) -> i32 {
            let x = #[expr] value;
            match x {
                #[arm] #[pat] other => other,
            }
        }
        "#,
    );

    assert_eq!(result.diagnostics, vec![]);
}
