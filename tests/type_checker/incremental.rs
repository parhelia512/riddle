use crate::{lower_and_resolve, messages};
use frontend::incremental::{IncrementalParser, ReparseMode};
use type_checker::IncrementalTypeChecker;

#[test]
fn incremental_reports_unsized_declarations() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(
        r#"
        struct Bad { value: str }
        fun main() {}
        "#,
    );
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let mut checker = IncrementalTypeChecker::new();
    let hir = lower_and_resolve(parse);
    let result = checker.check(&hir);
    assert!(
        result
            .result
            .diagnostics
            .iter()
            .any(|diag| diag.code == "E0043")
    );
}

#[test]
fn incremental_type_checker_reuses_unchanged_bodies() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(
        r#"
        fun stable() -> i32 {
            1
        }

        fun edited() -> bool {
            let value: bool = true;
            value
        }
        "#,
    );
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let mut checker = IncrementalTypeChecker::new();
    let hir = lower_and_resolve(parse);
    let first = checker.check(&hir);
    assert_eq!(first.result.diagnostics, vec![]);
    assert_eq!(first.stats.checked_bodies, 2);
    assert_eq!(first.stats.reused_bodies, 0);

    let offset = parser.source().find("true").unwrap();
    parser.apply_edit(offset, "true".len(), "1");
    assert!(matches!(
        parser.last_reparse_mode(),
        ReparseMode::Incremental(_)
    ));
    let parse = parser.current_parse().unwrap();
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let hir = lower_and_resolve(parse);
    let second = checker.check(&hir);
    assert_eq!(second.stats.checked_bodies, 1);
    assert_eq!(second.stats.reused_bodies, 1);
    assert!(
        messages(&second.result)
            .iter()
            .any(|msg| msg.contains("let initializer type mismatch"))
    );
}

#[test]
fn incremental_trait_impl_edit_updates_contract_diagnostics() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(
        r#"
        trait Flag {
            fun value() -> bool;
        }

        struct Marker {}

        impl Flag for Marker {
            fun value() -> bool { 1 == 1 }
        }
        "#,
    );
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let mut checker = IncrementalTypeChecker::new();
    let hir = lower_and_resolve(parse);
    let first = checker.check(&hir);
    assert_eq!(first.result.diagnostics, vec![]);

    let offset = parser.source().find("bool").unwrap();
    parser.apply_edit(offset, "bool".len(), "i32");
    assert!(matches!(
        parser.last_reparse_mode(),
        ReparseMode::Incremental(_)
    ));
    let parse = parser.current_parse().unwrap();
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let hir = lower_and_resolve(parse);
    let second = checker.check(&hir);
    assert!(
        messages(&second.result)
            .iter()
            .any(|msg| msg.contains("impl method `value` for trait `Flag` return type mismatch"))
    );
}
