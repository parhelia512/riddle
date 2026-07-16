use crate::{lower_and_resolve, messages};
use frontend::incremental::{IncrementalParser, ReparseMode};
use type_checker::{Diagnostic, IncrementalTypeChecker, TypeCheckResult, check_hir};

fn diagnostics_with_code(result: &TypeCheckResult, code: &str) -> Vec<Diagnostic> {
    result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == code)
        .cloned()
        .collect()
}

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
fn incremental_public_check_invalidates_moved_spans() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source("fun bad(value: str) {}");
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let mut checker = IncrementalTypeChecker::new();
    let hir = lower_and_resolve(parse);
    let first = checker.check(&hir);
    assert_eq!(
        diagnostics_with_code(&first.result, "E0043"),
        diagnostics_with_code(&check_hir(&hir), "E0043")
    );

    parser.apply_edit(0, 0, "// moved\n");
    let parse = parser.current_parse().unwrap();
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);
    let hir = lower_and_resolve(parse);
    let second = checker.check(&hir);

    assert_eq!(second.stats.checked_bodies, 1);
    assert_eq!(second.stats.reused_bodies, 0);
    assert_eq!(
        diagnostics_with_code(&second.result, "E0043"),
        diagnostics_with_code(&check_hir(&hir), "E0043")
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

#[test]
fn incremental_generic_recursion_matches_full_check_before_and_after_reuse() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(
        r#"
        struct Wrap<T> { inner: T }

        fun f<T>(x: T) -> T {
            g(Wrap { inner: x })
        }

        fun g<T>(x: T) -> T {
            f(Wrap { inner: x })
        }

        fun edited() {
            0;
        }
        "#,
    );
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let mut checker = IncrementalTypeChecker::new();
    let hir = lower_and_resolve(parse);
    let full = check_hir(&hir);
    let first = checker.check(&hir);
    let expected = diagnostics_with_code(&full, "E0033");

    assert!(!expected.is_empty());
    assert_eq!(diagnostics_with_code(&first.result, "E0033"), expected);
    assert_eq!(first.stats.checked_bodies, 3);
    assert_eq!(first.stats.reused_bodies, 0);

    let offset = parser.source().find("0;").unwrap();
    parser.apply_edit(offset, 1, "1");
    assert!(matches!(
        parser.last_reparse_mode(),
        ReparseMode::Incremental(_)
    ));
    let parse = parser.current_parse().unwrap();
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);

    let hir = lower_and_resolve(parse);
    let full = check_hir(&hir);
    let second = checker.check(&hir);

    assert_eq!(second.stats.checked_bodies, 1);
    assert_eq!(second.stats.reused_bodies, 2);
    assert_eq!(
        diagnostics_with_code(&second.result, "E0033"),
        diagnostics_with_code(&full, "E0033")
    );
}
