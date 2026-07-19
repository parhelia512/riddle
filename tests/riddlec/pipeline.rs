use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_source_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "riddle-load-source-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn assert_same_check_result(left: &CompileResult, right: &CompileResult) {
    let parse_errors = |result: &CompileResult| {
        result
            .parse_errors
            .iter()
            .map(|error| (error.message.clone(), error.span))
            .collect::<Vec<_>>()
    };
    assert_eq!(parse_errors(left), parse_errors(right));
    assert_eq!(left.hir_diagnostics, right.hir_diagnostics);
    assert_eq!(left.type_result.diagnostics, right.type_result.diagnostics);
    assert_eq!(left.analysis_diagnostics, right.analysis_diagnostics);
}

#[test]
fn replacement_keeps_utf8_boundaries() {
    assert_eq!(replacement("a😀b", "a😀xyb"), (5, 0, "xy"));
    assert_eq!(replacement("a😀b", "ab"), (1, 4, ""));
}

#[test]
fn check_session_matches_stateless_checks_across_edits() {
    let mut session = CheckSession::new();
    let options = CompileOptions { use_std: true };
    let sources = [
        "fun stable() -> i32 { 1 }\nfun main() { let value = 1; value; }",
        "// 😀\nfun stable() -> i32 { 1 }\nfun main() { missing; }",
        "// 😀\nfun stable() -> i32 { 1 }\nfun main() { let value = 2; value; }",
    ];

    for source in sources {
        let expected = check_with_options(source, options);
        let actual = session.check_with_options(source, options);
        assert_same_check_result(&actual, &expected);
    }
}

#[test]
fn check_session_shifts_cached_diagnostics_with_their_bodies() {
    let source = r#"
struct Wrap<T> { inner: T }
struct Bad { value: str }
trait Flag { fun value() -> bool; }
struct Marker {}
impl Flag for Marker {}
fun f<T>(x: T) -> T { g(Wrap { inner: x }) }
fun g<T>(x: T) -> T { f(Wrap { inner: x }) }
fun bad() { let value: bool = 1; }
"#;
    let options = CompileOptions { use_std: false };
    let mut session = CheckSession::new();
    let first = session.check_with_options(source, options);
    assert_same_check_result(&first, &check_with_options(source, options));

    let shifted = format!("// 😀\n{source}");
    let actual = session.check_with_options(&shifted, options);
    let expected = check_with_options(&shifted, options);
    assert_same_check_result(&actual, &expected);
    assert!(
        actual
            .type_result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0033")
    );
    assert!(["E0026", "E0043"].iter().all(|code| {
        actual
            .type_result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == *code)
    }));
}

#[test]
fn check_session_does_not_shift_signature_diagnostics_with_body() {
    let options = CompileOptions { use_std: false };
    let mut session = CheckSession::new();
    let source = "fun bad(value: str) {}";
    let first = session.check_with_options(source, options);
    assert_same_check_result(&first, &check_with_options(source, options));

    let shifted = "fun bad(value: str)  {}";
    let actual = session.check_with_options(shifted, options);
    let expected = check_with_options(shifted, options);
    assert_same_check_result(&actual, &expected);

    let signature_label = actual
        .type_result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0043")
        .flat_map(|diagnostic| &diagnostic.labels)
        .find(|label| {
            let range = std::ops::Range::<usize>::from(label.range);
            shifted.get(range) == Some("str")
        });
    assert!(signature_label.is_some());
}

#[test]
fn check_session_invalidates_globals_when_declarations_change() {
    let options = CompileOptions { use_std: false };
    let mut session = CheckSession::new();
    let valid = "struct Value { field: &str }\nfun main() {}";
    let first = session.check_with_options(valid, options);
    assert_same_check_result(&first, &check_with_options(valid, options));

    let invalid = "struct Value { field: str }\nfun main() {}";
    let actual = session.check_with_options(invalid, options);
    let expected = check_with_options(invalid, options);
    assert_same_check_result(&actual, &expected);
    assert!(
        actual
            .type_result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0043")
    );
}

#[test]
fn load_source_file_expands_external_mods() {
    let root = temp_source_root("external-mods");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("main.rid"),
        "mod util;\nfun main() -> i32 { util::one() }\n",
    )
    .unwrap();
    fs::write(root.join("util.rid"), "fun one() -> i32 { 1 }\n").unwrap();

    let loaded = load_source_file(root.join("main.rid")).unwrap();
    assert!(loaded.source.contains("mod util {"));
    assert!(loaded.source.contains("fun one() -> i32 { 1 }"));
    assert_eq!(loaded.files.len(), 2);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn source_map_points_into_external_module() {
    let root = temp_source_root("source-map");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("main.rid"),
        "mod util;\nfun main() -> i32 { util::value() }\n",
    )
    .unwrap();
    fs::write(
        root.join("util.rid"),
        "pub fun value() -> i32 { missing }\n",
    )
    .unwrap();

    let loaded = load_source_file(root.join("main.rid")).unwrap();
    let start = loaded.source.find("missing").unwrap();
    let mapped = loaded
        .source_map
        .map_range(rowan::TextRange::new(
            (start as u32).into(),
            ((start + "missing".len()) as u32).into(),
        ))
        .unwrap();

    assert_eq!(
        mapped.path,
        fs::canonicalize(root.join("util.rid")).unwrap()
    );
    assert_eq!(
        &mapped.source[usize::from(mapped.range.start())..usize::from(mapped.range.end())],
        "missing"
    );
    let generated_eof =
        loaded.source.find("pub fun").unwrap() + "pub fun value() -> i32 { missing }\n".len();
    let mapped_eof = loaded
        .source_map
        .map_range(rowan::TextRange::empty((generated_eof as u32).into()))
        .unwrap();
    assert_eq!(mapped_eof.path, mapped.path);
    assert_eq!(usize::from(mapped_eof.range.start()), mapped.source.len());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn source_map_keeps_empty_files() {
    let root = temp_source_root("empty-source-map");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("main.rid");
    fs::write(&path, "").unwrap();

    let loaded = load_source_file(&path).unwrap();
    let mapped = loaded
        .source_map
        .map_range(rowan::TextRange::empty(0.into()))
        .unwrap();

    assert_eq!(mapped.path, fs::canonicalize(path).unwrap());
    assert_eq!(mapped.source, "");
    assert!(mapped.range.is_empty());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn syntax_error_at_eof_stays_in_user_source_with_std_enabled() {
    let source = "fun main() {";
    let result = compile(source);

    assert!(!result.parse_errors.is_empty());
    assert!(
        result
            .parse_errors
            .iter()
            .all(|error| usize::from(error.span.end()) <= source.len()),
        "{:#?}",
        result.parse_errors
    );
    assert!(
        result
            .parse_errors
            .iter()
            .any(|error| usize::from(error.span.start()) == source.len()),
        "{:#?}",
        result.parse_errors
    );
}

#[test]
fn pipeline_stops_at_the_requested_stage() {
    let source = "fun main() { let value = 1; value; }";
    let options = CompileOptions { use_std: false };

    let resolved = resolve_with_options(source, options);
    assert!(resolved.hir.is_some());
    assert!(resolved.type_result.expr_types.is_empty());
    assert!(resolved.mir_module.is_none());

    let checked = check_with_options(source, options);
    assert!(!checked.type_result.expr_types.is_empty());
    assert!(checked.mir_module.is_none());

    let built = compile_with_options(source, options);
    assert!(built.success());
    assert!(built.mir_module.is_some());
}

#[test]
fn source_loader_uses_in_memory_overlays() {
    let root = temp_source_root("source-overlay");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("main.rid"), "mod util;\n").unwrap();
    fs::write(root.join("util.rid"), "pub fun value() -> i32 { 1 }\n").unwrap();
    let mut overlays = HashMap::new();
    overlays.insert(
        root.join("util.rid"),
        "pub fun value() -> i32 { 2 }\n".into(),
    );

    let loaded = load_source_file_with_overlays(root.join("main.rid"), &overlays).unwrap();

    assert!(loaded.source.contains("value() -> i32 { 2 }"));
    assert!(!loaded.source.contains("value() -> i32 { 1 }"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn load_source_file_uses_rust_style_mod_rid_tree() {
    let root = temp_source_root("mod-rid-tree");
    fs::create_dir_all(root.join("foo")).unwrap();
    fs::write(
        root.join("main.rid"),
        "mod foo;\nfun main() -> i32 { foo::value() }\n",
    )
    .unwrap();
    fs::write(
        root.join("foo").join("mod.rid"),
        "mod bar;\npub fun value() -> i32 { bar::value() }\n",
    )
    .unwrap();
    fs::write(
        root.join("foo").join("bar.rid"),
        "pub fun value() -> i32 { 1 }\n",
    )
    .unwrap();

    let loaded = load_source_file(root.join("main.rid")).unwrap();
    assert!(
        loaded
            .files
            .contains(&fs::canonicalize(root.join("foo").join("mod.rid")).unwrap())
    );
    assert!(
        loaded
            .files
            .contains(&fs::canonicalize(root.join("foo").join("bar.rid")).unwrap())
    );
    assert!(compile(&loaded.source).success());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn flat_modules_resolve_children_from_module_directory() {
    let root = temp_source_root("flat-module-children");
    fs::create_dir_all(root.join("foo")).unwrap();
    fs::write(
        root.join("main.rid"),
        "mod foo;\nfun main() -> i32 { foo::value() }\n",
    )
    .unwrap();
    fs::write(
        root.join("foo.rid"),
        "mod bar;\npub fun value() -> i32 { bar::value() }\n",
    )
    .unwrap();
    fs::write(
        root.join("foo").join("bar.rid"),
        "pub fun value() -> i32 { 1 }\n",
    )
    .unwrap();
    fs::write(root.join("bar.rid"), "pub fun value() -> i32 { 99 }\n").unwrap();

    let loaded = load_source_file(root.join("main.rid")).unwrap();
    assert!(loaded.source.contains("pub fun value() -> i32 { 1 }"));
    assert!(!loaded.source.contains("pub fun value() -> i32 { 99 }"));
    assert!(compile(&loaded.source).success());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn inline_modules_resolve_children_from_module_directory() {
    let root = temp_source_root("inline-module-children");
    fs::create_dir_all(root.join("foo")).unwrap();
    fs::write(
            root.join("main.rid"),
            "mod foo { mod bar; pub fun value() -> i32 { bar::value() } }\nfun main() -> i32 { foo::value() }\n",
        )
        .unwrap();
    fs::write(
        root.join("foo").join("bar.rid"),
        "pub fun value() -> i32 { 1 }\n",
    )
    .unwrap();

    let loaded = load_source_file(root.join("main.rid")).unwrap();
    assert!(
        loaded
            .files
            .contains(&fs::canonicalize(root.join("foo").join("bar.rid")).unwrap())
    );
    assert!(compile(&loaded.source).success());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn duplicate_flat_and_mod_rid_modules_are_rejected() {
    let root = temp_source_root("duplicate-module-files");
    fs::create_dir_all(root.join("foo")).unwrap();
    fs::write(root.join("main.rid"), "mod foo;\n").unwrap();
    fs::write(root.join("foo.rid"), "pub fun value() -> i32 { 1 }\n").unwrap();
    fs::write(
        root.join("foo").join("mod.rid"),
        "pub fun value() -> i32 { 2 }\n",
    )
    .unwrap();

    let error = load_source_file(root.join("main.rid")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("ambiguous"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn undeclared_directory_modules_are_not_loaded() {
    let root = temp_source_root("undeclared-module");
    fs::create_dir_all(root.join("foo")).unwrap();
    fs::write(root.join("main.rid"), "fun main() -> i32 { 0 }\n").unwrap();
    fs::write(root.join("foo").join("mod.rid"), "this is not parsed\n").unwrap();

    let loaded = load_source_file(root.join("main.rid")).unwrap();
    assert_eq!(
        loaded.files,
        vec![fs::canonicalize(root.join("main.rid")).unwrap()]
    );
    assert!(compile(&loaded.source).success());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn std_range_iterator_type_checks() {
    let result = compile(
        r#"
            fun main() {
                let mut iter = range(0, 3);
                let first = iter.next();
            }
            "#,
    );

    assert!(result.success(), "{:#?}", result.type_result.diagnostics);
}

#[test]
fn std_basic_value_methods_compile() {
    let result = compile(
        r#"
            struct Token {
                value: i32,
            }

            fun main() -> i32 {
                let some: Option<i32> = Some(2);
                let option_value = some.unwrap_or(0);
                let none: Option<i32> = None;
                let fallback = none.or(Some(4)).unwrap_or(0);

                let ok: Result<i32, bool> = Ok(3);
                let result_value = ok.unwrap_or(0);
                let has_value = ok.ok().is_some();
                let err: Result<i32, bool> = Err(true);
                let has_error = err.err().is_some();

                let token: Option<Token> = Some(Token { value: 5 });
                let token_is_present = token.is_some();
                let token_value = token.unwrap_or(Token { value: 0 }).value;

                let text: &str = "abc";
                let byte = text.byte_at(1usize).unwrap_or(0u8) as i32;

                if some.is_some() && none.is_none() && ok.is_ok() && err.is_err()
                    && has_value && has_error && text.byte_at(3usize).is_none()
                    && token_is_present && token_value == 5
                    && text.len() == 3usize && !text.is_empty() {
                    option_value + fallback + result_value + byte
                } else {
                    0
                }
            }
            "#,
    );

    assert!(
        result.success(),
        "hir: {:#?}\ntype: {:#?}\nanalysis: {:#?}",
        result.hir_diagnostics,
        result.type_result.diagnostics,
        result.analysis_diagnostics
    );
    let c = generate_c(result.mir_module.as_ref().unwrap()).unwrap();
    assert!(c.contains("return s.len"), "{c}");
    assert!(c.contains("s.ptr[i]"), "{c}");
    assert!(c.contains("is_some__Option_i32"), "{c}");
}

#[test]
fn std_string_and_vector_compile() {
    let result = compile(
        r#"
            fun main() -> i32 {
                let mut values: Vector<i32> = Vector::new();
                values.push(1);
                values.push(2);
                let fallback = 0;
                let first = *values.get(0usize).unwrap_or(&fallback);
                let missing = values.get(2usize).is_none();
                let last = values.pop().unwrap_or(0);

                let mut text = String::from_str("hello");
                text.push_str(" world");
                if first == 1 && last == 2 && missing
                    && text.len() == 11usize && text.as_str() == "hello world" {
                    0
                } else {
                    1
                }
            }
            "#,
    );

    assert!(
        result.success(),
        "parse: {:#?}\nhir: {:#?}\ntype: {:#?}\nanalysis: {:#?}",
        result.parse_errors,
        result.hir_diagnostics,
        result.type_result.diagnostics,
        result.analysis_diagnostics
    );
    let c = generate_c(result.mir_module.as_ref().unwrap()).unwrap();
    assert!(c.contains("new__Vector_i32"), "{c}");
    assert!(c.contains("vector_grow"), "{c}");
    assert!(c.contains("size_of_ptr__i32"), "{c}");
    assert!(c.contains("str_from_raw"), "{c}");
}

#[test]
fn vector_mutation_is_rejected_while_element_reference_is_live() {
    let result = compile(
        r#"
            fun main() {
                let mut values: Vector<i32> = Vector::new();
                values.push(1);
                let mut fallback = 0;
                let reference = values.get_mut(0usize).unwrap_or(&mut fallback);
                values.push(2);
                *reference = 3;
            }
            "#,
    );

    assert!(!result.success());
    assert!(result.mir_module.is_none());
    assert!(
        result
            .analysis_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0302"),
        "{:#?}",
        result.analysis_diagnostics
    );
}

#[test]
fn vector_mutation_is_rejected_while_shared_element_reference_is_live() {
    let result = compile(
        r#"
            fun main() {
                let mut values: Vector<i32> = Vector::new();
                values.push(1);
                let fallback = 0;
                let reference = values.get(0usize).unwrap_or(&fallback);
                values.push(2);
                *reference;
            }
            "#,
    );

    assert!(
        result
            .analysis_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0300"),
        "{:#?}",
        result.analysis_diagnostics
    );
}

#[test]
fn vector_mutation_is_allowed_after_element_reference_last_use() {
    let result = compile(
        r#"
            fun main() {
                let mut values: Vector<i32> = Vector::new();
                values.push(1);
                let mut fallback = 0;
                let reference = values.get_mut(0usize).unwrap_or(&mut fallback);
                *reference = 3;
                values.push(2);
            }
            "#,
    );

    assert!(result.success(), "{:#?}", result.analysis_diagnostics);
}

#[test]
fn std_clone_and_comparison_methods_are_callable() {
    let result = compile(
        r#"
            fun main() -> i32 {
                let value: i32 = 7;
                let cloned = value.clone();
                let equal = value.eq(&7);
                let ordering = value.cmp(&cloned);
                let partial = value.partial_cmp(&cloned);
                if equal { cloned } else { 0 }
            }
            "#,
    );

    assert!(
        result.success(),
        "hir: {:#?}\ntype: {:#?}\nanalysis: {:#?}",
        result.hir_diagnostics,
        result.type_result.diagnostics,
        result.analysis_diagnostics
    );
    let c = generate_c(result.mir_module.as_ref().unwrap()).unwrap();
    assert!(c.contains("clone__i32"), "{c}");
    assert!(c.contains("cmp__i32"), "{c}");
    assert!(c.contains("ref_tmp"), "{c}");
    assert!(!c.contains("&((int32_t)7)"), "{c}");
}

#[test]
fn std_operator_trait_methods_emit_native_c_without_wrappers() {
    let result = compile(
        r#"
            fun main() -> i64 {
                let mut value: i64 = 64i64;
                let added = value.add(2i64);
                let subtracted = added.sub(1i64);
                let multiplied = subtracted.mul(3i64);
                let divided = multiplied.div(2i64);
                let remainder = divided.rem(5i64);
                let masked = remainder.bitand(7i64);
                let combined = masked.bitor(8i64);
                let toggled = combined.bitxor(3i64);
                let shifted = toggled.shl(1i64).shr(1i64);
                let negated = shifted.not().neg();

                value.add_assign(3i64);
                value.sub_assign(1i64);
                value.mul_assign(2i64);
                value.div_assign(2i64);
                value.rem_assign(63i64);
                value.bitand_assign(31i64);
                value.bitor_assign(8i64);
                value.bitxor_assign(1i64);
                value.shl_assign(1i64);
                value.shr_assign(1i64);

                if true.not() { value } else { value + negated }
            }
            "#,
    );

    assert!(
        result.success(),
        "hir: {:#?}\ntype: {:#?}\nanalysis: {:#?}",
        result.hir_diagnostics,
        result.type_result.diagnostics,
        result.analysis_diagnostics
    );
    let c = generate_c(result.mir_module.as_ref().unwrap()).unwrap();
    for method in [
        "add__",
        "sub__",
        "mul__",
        "div__",
        "rem__",
        "neg__",
        "not__",
        "bitand__",
        "bitor__",
        "bitxor__",
        "shl__",
        "shr__",
        "add_assign__",
        "sub_assign__",
        "mul_assign__",
        "div_assign__",
        "rem_assign__",
        "bitand_assign__",
        "bitor_assign__",
        "bitxor_assign__",
        "shl_assign__",
        "shr_assign__",
    ] {
        assert!(!c.contains(method), "unexpected `{method}` wrapper:\n{c}");
    }
    assert!(c.contains(" + "), "expected native C addition:\n{c}");
    assert!(c.contains("(-("), "expected native C negation:\n{c}");
}

#[test]
fn compile_can_skip_std() {
    let result = compile_with_options(
        r#"
            fun main() {
                let value = range(0, 3);
            }
            "#,
        CompileOptions { use_std: false },
    );

    assert!(!result.success());
    assert!(
        result
            .hir_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unresolved name: `range`")),
        "{:#?}",
        result.hir_diagnostics
    );
}

#[test]
fn compile_without_std_accepts_basic_program() {
    let result = compile_with_options(
        r#"
            fun main() {
                let value = 1;
            }
            "#,
        CompileOptions { use_std: false },
    );

    assert!(result.success(), "{:#?}", result.type_result.diagnostics);
}

#[test]
fn unit_uses_empty_tuple_syntax() {
    let result = compile_with_options(
        r#"
            fun identity(value: ()) -> () {
                value
            }

            fun main() {
                identity(());
            }
            "#,
        CompileOptions { use_std: false },
    );

    assert!(result.success(), "{:#?}", result.type_result.diagnostics);
}

#[test]
fn std_prelude_reexports_core_items() {
    let result = compile(
        r#"
            fun main() {
                let value: Option<i32> = Option::Some(1);
                let mut iter = range(0, 3);
                let first = iter.next();
            }
            "#,
    );

    assert!(result.success(), "{:#?}", result.type_result.diagnostics);
}

#[test]
fn std_option_and_result_copy_depends_on_payloads() {
    let copy = compile(
        r#"
            fun main() {
                let option: Option<i32> = Some(1);
                let first_option = option;
                let second_option = option;
                let result: Result<i32, bool> = Ok(1);
                let first_result = result;
                let second_result = result;
            }
            "#,
    );
    assert!(copy.success(), "{:#?}", copy.analysis_diagnostics);
    assert!(
        !generate_c(copy.mir_module.as_ref().unwrap())
            .unwrap()
            .is_empty()
    );

    let moved = compile(
        r#"
            struct Token { value: i32 }

            fun main() {
                let option: Option<Token> = Option::Some(Token { value: 1 });
                let first = option;
                let second = option;
            }
            "#,
    );
    assert!(
        moved
            .analysis_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("use of moved value: `option`")),
        "{:#?}",
        moved.analysis_diagnostics
    );
}

#[test]
fn copy_impl_requires_every_payload_to_be_copy() {
    let invalid = compile(
        r#"
            struct Token { value: i32 }
            struct Wrapper<T> { value: T }
            enum TokenState { Empty, Full(Token) }

            impl<T> Copy for Wrapper<T> {}
            impl Copy for TokenState {}

            fun main() {}
            "#,
    );

    let copy_errors = invalid
        .type_result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "E0041")
        .collect::<Vec<_>>();
    assert_eq!(
        copy_errors.len(),
        2,
        "{:#?}",
        invalid.type_result.diagnostics
    );
    assert!(
        copy_errors
            .iter()
            .any(|diagnostic| diagnostic.message.contains("Wrapper<T>"))
    );
    assert!(
        copy_errors
            .iter()
            .any(|diagnostic| diagnostic.message.contains("TokenState"))
    );
}

#[test]
fn copy_impl_accepts_nested_conditional_copy_fields() {
    let result = compile(
        r#"
            struct Nested<T> { value: Option<T> }

            impl<T: Copy> Copy for Nested<T> {}

            fun main() {
                let value: Nested<i32> = Nested { value: Some(1) };
                let first = value;
                let second = value;
            }
            "#,
    );

    assert!(
        result.success(),
        "type: {:#?}\nanalysis: {:#?}",
        result.type_result.diagnostics,
        result.analysis_diagnostics
    );
}

#[test]
fn enum_match_lowers_variants_guards_bindings_and_values() {
    let result = compile(
        r#"
            enum Message {
                Quit,
                Number(i32),
                Pair { left: i32, right: i32 },
            }

            fun select(value: Message) -> i32 {
                match value {
                    Message::Quit => 0,
                    Message::Number(number) if number > 10 => number,
                    Message::Number(number) => number + 1,
                    Message::Pair { left, right: other } => left + other,
                }
            }

            fun main() -> i32 {
                let pair = Message::Pair { right: 22, left: 20 };
                select(pair)
            }
            "#,
    );

    assert!(
        result.success(),
        "hir: {:#?}\ntype: {:#?}\nanalysis: {:#?}",
        result.hir_diagnostics,
        result.type_result.diagnostics,
        result.analysis_diagnostics
    );
    let c = generate_c(result.mir_module.as_ref().unwrap()).unwrap();
    assert!(c.contains("Number_0;"), "{c}");
    assert!(c.contains(".Pair_left"), "{c}");
    assert!(c.contains("if ("), "{c}");
    assert!(c.contains("self->start < self->end"), "{c}");
}

#[test]
fn enum_constructor_uses_the_flattened_payload_offset() {
    let result = compile(
        r#"
            enum Value {
                First(i32),
                Second(i32),
            }

            fun main() -> i32 {
                match Value::Second(7) {
                    Value::First(value) => value,
                    Value::Second(value) => value,
                }
            }
            "#,
    );

    assert!(result.success(), "{:#?}", result.type_result.diagnostics);
    let c = generate_c(result.mir_module.as_ref().unwrap()).unwrap();
    assert!(c.contains(".Second_0 ="), "{c}");
    assert!(!c.contains(".First_0 = ((int32_t)7)"), "{c}");
}

#[test]
fn literal_match_preserves_values_and_string_comparison() {
    let result = compile(
        r#"
            fun classify(value: i32) -> i32 {
                match value {
                    0 => 10,
                    1 => 20,
                    other => other,
                }
            }

            fun is_yes(value: &str) -> bool {
                match value {
                    "yes" => true,
                    _ => false,
                }
            }

            fun main() -> i32 {
                classify(1)
            }
            "#,
    );

    assert!(result.success(), "{:#?}", result.type_result.diagnostics);
    let c = generate_c(result.mir_module.as_ref().unwrap()).unwrap();
    assert!(c.contains("memcmp"), "{c}");
    assert!(c.contains("== ((int32_t)0)"), "{c}");
}

#[test]
fn non_exhaustive_enum_match_is_rejected() {
    let result = compile(
        r#"
            enum State { Ready, Done }

            fun main() -> i32 {
                match State::Ready {
                    State::Ready => 1,
                }
            }
            "#,
    );

    assert!(!result.success());
    assert!(
        result
            .type_result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E0039"),
        "{:#?}",
        result.type_result.diagnostics
    );
}

#[test]
fn unit_return_does_not_hide_non_exhaustive_payload_match() {
    let result = compile(
        r#"
            enum State { Ready, Done(i32) }

            fun consume(state: State) {
                match state {
                    State::Ready => { return; },
                    State::Done(1) => {},
                }
            }

            fun main() {
                consume(State::Ready);
            }
        "#,
    );

    assert!(!result.success());
    let diagnostic = result
        .type_result
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "E0039")
        .unwrap();
    assert!(
        diagnostic.message.contains("State::Done(_)"),
        "{diagnostic:#?}"
    );
    assert!(
        diagnostic.notes.iter().any(|note| {
            note == "uncovered i32 ranges for `State::Done(_)`: `-2147483648..=0`, `2..=2147483647`"
        }),
        "{diagnostic:#?}"
    );
}

#[test]
fn std_modules_expose_core_items() {
    let result = compile(
        r#"
            use std::String;
            use std::Vector;

            fun main() {
                let value = std::option::Option::Some(1);
                let mut iter: Range = std::ops::range(0, 3);
                let first = iter.next();
                let text = String::new();
                let values: Vector<i32> = Vector::new();
            }
            "#,
    );

    assert!(result.success(), "{:#?}", result.type_result.diagnostics);
}

#[test]
fn std_array_into_iterator_accepts_non_copy_items() {
    let result = compile(
        r#"
            struct Token {
                value: i32,
            }

            fun main() {
                let values = [Token { value: 1 }, Token { value: 2 }];
                let mut iter = values.into_iter();
                let first = iter.next();

                for item in [Token { value: 3 }, Token { value: 4 }] {
                    let next = item.value + 1;
                }
            }
            "#,
    );

    assert!(
        result.success(),
        "type: {:#?}\nanalysis: {:#?}",
        result.type_result.diagnostics,
        result.analysis_diagnostics
    );
}

#[test]
fn std_range_for_loop_lowers_to_mir_loop() {
    let result = compile(
        r#"
            fun main() {
                let mut sum = 0;
                for item in range(0, 3) {
                    sum += item;
                }
            }
            "#,
    );

    assert!(result.success(), "{:#?}", result.type_result.diagnostics);
    let module = result
        .mir_module
        .expect("successful compile should lower MIR");
    let main_id = module
        .function_order
        .iter()
        .copied()
        .find(|id| module.functions[*id].name == "main")
        .expect("main function should be lowered");
    let main = &module.functions[main_id];
    let has_loop_branch = main
        .blocks
        .iter()
        .any(|(_, block)| matches!(block.terminator, mir::instr::Terminator::CondBranch(..)));
    assert!(has_loop_branch, "{main:#?}");
    assert!(
        !generate_c(&module)
            .expect("C backend should lower for loop")
            .is_empty()
    );
}
