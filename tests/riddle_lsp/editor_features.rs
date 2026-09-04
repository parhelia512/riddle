use super::*;

#[test]
fn formatting_normalizes_layout_without_touching_tokens() {
    let source = "fun add(left:i32,right:i32)->i32{left+right}\n";

    assert_eq!(
        format_source(source, 4, true),
        "fun add(left: i32, right: i32) -> i32 {\n    left + right\n}\n"
    );
}

#[test]
fn formatting_keeps_documentation_comments_and_crlf() {
    let source = "/// docs\r\nfun main(){/* details\r\n * stay */let value=1;}\r\n";

    assert_eq!(
        format_source(source, 4, true),
        "/// docs\r\nfun main() {\r\n    /* details\r\n * stay */ let value = 1;\r\n}\r\n"
    );
}

#[test]
fn signature_help_tracks_the_active_argument() {
    let source = "fun add(left: i32, right: i32) -> i32 { left + right } fun main() { add(1, 2); }";
    let cursor = position(source, source.rfind("2)").unwrap() + 1);

    let help = signature_help_for_source(source, cursor).unwrap();

    assert_eq!(help.active_signature, Some(0));
    assert_eq!(help.active_parameter, Some(1));
    assert_eq!(
        help.signatures[0].label,
        "fun add(left: i32, right: i32) -> i32"
    );
}

#[test]
fn signature_help_includes_function_documentation() {
    let source = "/// Adds two numbers.\nfun add(left: i32, right: i32) -> i32 { left + right } fun main() { add(1, 2); }";
    let cursor = position(source, source.rfind("2)").unwrap() + 1);

    let help = signature_help_for_source(source, cursor).unwrap();

    let documentation = help.signatures[0].documentation.as_ref().unwrap();
    let lsp_types::Documentation::MarkupContent(contents) = documentation else {
        panic!("expected markdown documentation")
    };
    assert!(contents.value.contains("Adds two numbers."));
}

#[test]
fn signature_help_keeps_nested_type_commas_inside_one_parameter() {
    let source =
        "fun first(pair: (i32, i32), right: i32) -> i32 { right } fun main() { first((1, 2), 3); }";
    let cursor = position(source, source.rfind("3)").unwrap() + 1);

    let help = signature_help_for_source(source, cursor).unwrap();
    let parameters = help.signatures[0].parameters.as_ref().unwrap();

    assert_eq!(help.active_parameter, Some(1));
    assert_eq!(parameters.len(), 2);
    assert_eq!(
        parameters[0].label,
        ParameterLabel::Simple("pair: (i32, i32)".into())
    );
    assert_eq!(
        parameters[1].label,
        ParameterLabel::Simple("right: i32".into())
    );
}

#[test]
fn method_signature_help_accounts_for_the_implicit_receiver() {
    let source = "struct Counter {} impl Counter { fun add(&self, amount: i32) -> i32 { amount } } fun main() { let counter = Counter {}; counter.add(1); }";
    let cursor = position(source, source.rfind("1)").unwrap() + 1);

    let help = signature_help_for_source(source, cursor).unwrap();

    assert_eq!(help.active_parameter, Some(1));
}

#[test]
fn document_symbols_include_type_members_and_functions() {
    let source = "struct Point { x: i32, y: i32 } fun origin() -> Point { Point { x: 0, y: 0 } }";

    let symbols = document_symbols_for_source(source);

    assert_eq!(
        symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>(),
        ["Point", "origin"]
    );
    assert_eq!(
        symbols[0]
            .children
            .as_ref()
            .unwrap()
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>(),
        ["x", "y"]
    );
}

#[test]
fn workspace_symbols_filter_by_query() {
    let source = "struct Point { x: i32 } fun origin() -> Point { Point { x: 0 } }";

    let symbols = workspace_symbols_for_source(source, "ori");

    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "origin");
}

#[test]
fn document_highlights_cover_declaration_and_uses() {
    let source = "fun main() { let value = 1; value; }";
    let cursor = position(source, source.rfind("value").unwrap() + 2);

    let highlights = document_highlights_for_source(source, cursor).unwrap();

    assert_eq!(highlights.len(), 2);
}

#[test]
fn folding_ranges_follow_multiline_braces() {
    let source = "fun main() {\n    if true {\n        1\n    }\n}\n";

    let ranges = folding_ranges(source);

    assert_eq!(ranges.len(), 2);
    assert_eq!((ranges[0].start_line, ranges[0].end_line), (0, 4));
    assert_eq!((ranges[1].start_line, ranges[1].end_line), (1, 3));
}

#[test]
fn inlay_hints_include_parameter_names_for_calls() {
    let source = "fun add(left: i32, right: i32) -> i32 { left + right } fun main() { add(1, 2); }";

    let hints = inlay_hints_for_source(
        source,
        Range::new(Position::new(0, 0), position(source, source.len())),
    );
    let labels = hints
        .iter()
        .filter_map(|hint| match &hint.label {
            InlayHintLabel::String(label) => Some(label.as_str()),
            InlayHintLabel::LabelParts(_) => None,
        })
        .collect::<Vec<_>>();

    assert!(labels.contains(&"left: "));
    assert!(labels.contains(&"right: "));
}

#[test]
fn inlay_hints_resolve_same_named_methods_by_receiver_type() {
    let source = "struct Left {} struct Right {} impl Left { fun set(&self, left_value: i32) {} } impl Right { fun set(&self, right_value: i32) {} } fun main() { let left = Left {}; left.set(1); }";

    let hints = inlay_hints_for_source(
        source,
        Range::new(Position::new(0, 0), position(source, source.len())),
    );
    let labels = hints
        .iter()
        .filter_map(|hint| match &hint.label {
            InlayHintLabel::String(label) => Some(label.as_str()),
            InlayHintLabel::LabelParts(_) => None,
        })
        .collect::<Vec<_>>();

    assert!(labels.contains(&"left_value: "));
    assert!(!labels.contains(&"right_value: "));
}

#[test]
fn inlay_hints_show_inferred_lambda_parameter_types() {
    let source = "fun apply(f: impl Fn(i32) -> i32) -> i32 { f(1) } fun main() { let doubled = apply([v -> v * 2]); }";
    let v_name = source.rfind("[v ->").unwrap() + 2;

    let hints = inlay_hints_for_source(
        source,
        Range::new(Position::new(0, 0), position(source, source.len())),
    );

    assert!(hints.iter().any(|hint| {
        hint.position == position(source, v_name)
            && matches!(&hint.label, InlayHintLabel::String(label) if label == ": i32")
    }));
}

#[test]
fn inlay_hints_skip_annotated_lambda_parameters() {
    let source =
        "fun apply(f: impl Fn(i32) -> i32) -> i32 { f(1) } fun main() { apply([v: i32 -> v]); }";
    let v_name = source.rfind("[v: i32").unwrap() + 2;

    let hints = inlay_hints_for_source(
        source,
        Range::new(Position::new(0, 0), position(source, source.len())),
    );

    assert!(
        !hints
            .iter()
            .any(|hint| hint.position == position(source, v_name))
    );
}

#[test]
fn inlay_hints_show_result_types_on_multiline_chains() {
    let source = "struct Counter { value: i32 } impl Counter { fun inc(self) -> Counter { Counter { value: self.value + 1 } } } fun main() {\n    let c = Counter { value: 0 }\n        .inc()\n        .inc();\n}\n";
    let chain_end = source.rfind(")").unwrap() + 1;

    let hints = inlay_hints_for_source(
        source,
        Range::new(Position::new(0, 0), position(source, source.len())),
    );

    assert!(hints.iter().any(|hint| {
        hint.position == position(source, chain_end)
            && matches!(&hint.label, InlayHintLabel::String(label) if label == ": Counter")
    }));
}

#[test]
fn inlay_hints_skip_result_types_on_single_line_chains() {
    let source = "struct Counter { value: i32 } impl Counter { fun inc(self) -> Counter { Counter { value: self.value + 1 } } } fun main() { let c = Counter { value: 0 }.inc().inc(); }";
    let chain_end = source.rfind(")").unwrap();

    let hints = inlay_hints_for_source(
        source,
        Range::new(Position::new(0, 0), position(source, source.len())),
    );

    assert!(
        !hints
            .iter()
            .any(|hint| hint.position == position(source, chain_end))
    );
}

#[test]
fn selection_ranges_nest_from_identifier_to_file() {
    let source = "fun main() {\n    let value = 1 + 2;\n}\n";
    let cursor = Position::new(1, 10);

    let ranges = selection_ranges_for_source(source, &[cursor]);

    let Some(Some(selection)) = ranges.first() else {
        panic!("expected a selection range")
    };
    let mut chain = vec![selection.range];
    let mut current = selection.parent.as_deref();
    while let Some(ancestor) = current {
        chain.push(ancestor.range);
        current = ancestor.parent.as_deref();
    }
    assert!(chain.windows(2).all(|pair| {
        let (inner, outer) = (&pair[0], &pair[1]);
        outer.start.line < inner.start.line
            || (outer.start.line == inner.start.line
                && outer.start.character <= inner.start.character)
    }));
    let innermost = &chain[0];
    assert_eq!(innermost.start, Position::new(1, 8));
    let outermost = chain.last().unwrap();
    assert_eq!(outermost.start, Position::new(0, 0));
}

#[test]
fn selection_ranges_skip_unresolvable_positions() {
    let source = "fun main() {}\n";
    let ranges = selection_ranges_for_source(source, &[Position::new(0, 5), Position::new(9, 0)]);
    assert_eq!(ranges.len(), 2);
    assert!(ranges[0].is_some());
    assert!(ranges[1].is_none());
}

#[test]
fn document_links_point_at_module_files() {
    let root = temp_root("document-links");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("util.rid"), "fun helper() {}\n").unwrap();
    let source = "mod util;\nmod missing;\nmod nested {\n}\n";

    let links = document_links_for_source(source, Some(&root));

    assert_eq!(links.len(), 1);
    let target = links[0].target.as_ref().unwrap().as_str();
    assert!(target.ends_with("util.rid"));
    assert_eq!(links[0].range.start.character, 4);

    let _ = fs::remove_dir_all(root);
}
