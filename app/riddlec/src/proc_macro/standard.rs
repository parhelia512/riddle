use super::{document::classify_literal, *};

pub(super) fn expand_standard_derive_macro(
    name: &str,
    item: &SyntaxNode,
    call_site: &Range<usize>,
) -> Result<ProcMacroTokenStream, String> {
    if name != "Debug" {
        return Err(format!("unknown standard derive macro `{name}`"));
    }

    let source = if let Some(item) = ast::StructDecl::cast(item.clone()) {
        expand_standard_debug_struct(&item)?
    } else if let Some(item) = ast::EnumDecl::cast(item.clone()) {
        expand_standard_debug_enum(&item)?
    } else {
        return Err("Debug can only be derived for structs and enums".into());
    };
    let mut output = ProcMacroTokenStream::from_source(&source, 0)
        .map_err(|message| format!("failed to build Debug implementation: {message}"))?;
    output.set_span(call_site.clone());
    Ok(output)
}

pub(super) fn expand_standard_debug_struct(item: &ast::StructDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "Debug derive input is missing a struct name".to_string())?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    let mut output = debug_impl_header(&name, generic_params.as_ref(), where_clause.as_ref());
    let fields = item
        .field_list()
        .map(|fields| {
            fields
                .fields()
                .filter_map(|field| field.name().map(|name| name.text().to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if fields.is_empty() {
        let _ = write!(output, "formatter.write_str({name:?})");
    } else {
        let _ = write!(output, "formatter.write_str({:?})?;", format!("{name} {{ "));
        for (index, field) in fields.iter().enumerate() {
            if index > 0 {
                output.push_str("formatter.write_str(\", \")?;");
            }
            let _ = write!(output, "formatter.write_str({:?})?;", format!("{field}: "));
            let _ = write!(
                output,
                "crate::std::fmt::write_debug(&self.{field}, &mut *formatter)?;"
            );
        }
        output.push_str("formatter.write_str(\" }\")");
    }
    output.push_str(" } }");
    Ok(output)
}

pub(super) fn expand_standard_debug_enum(item: &ast::EnumDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "Debug derive input is missing an enum name".to_string())?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    let mut output = debug_impl_header(&name, generic_params.as_ref(), where_clause.as_ref());
    output.push_str("match self {");
    for variant in item.variants() {
        let variant_name = variant
            .name()
            .map(|name| name.text().to_string())
            .ok_or_else(|| "Debug derive input contains an unnamed enum variant".to_string())?;
        let fields = variant
            .field_list()
            .map(|fields| {
                fields
                    .fields()
                    .filter_map(|field| field.name().map(|name| name.text().to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let tuple_fields = variant.tuple_types().count();
        let has_tuple_fields = variant
            .syntax()
            .children_with_tokens()
            .any(|element| element.kind() == SyntaxKind::LParen);

        if !fields.is_empty() {
            let _ = write!(output, "{name}::{variant_name} {{");
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                let _ = write!(output, "{field}:__riddle_debug_{index}");
            }
            let _ = write!(
                output,
                "}}=>{{formatter.write_str({:?})?;",
                format!("{variant_name} {{ ")
            );
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    output.push_str("formatter.write_str(\", \")?;");
                }
                let _ = write!(output, "formatter.write_str({:?})?;", format!("{field}: "));
                let _ = write!(
                    output,
                    "crate::std::fmt::write_debug(__riddle_debug_{index}, &mut *formatter)?;"
                );
            }
            output.push_str("formatter.write_str(\" }\")},");
        } else if has_tuple_fields && tuple_fields > 0 {
            let _ = write!(output, "{name}::{variant_name}(");
            for index in 0..tuple_fields {
                if index > 0 {
                    output.push(',');
                }
                let _ = write!(output, "__riddle_debug_{index}");
            }
            let _ = write!(
                output,
                ")=>{{formatter.write_str({:?})?;",
                format!("{variant_name}(")
            );
            for index in 0..tuple_fields {
                if index > 0 {
                    output.push_str("formatter.write_str(\", \")?;");
                }
                let _ = write!(
                    output,
                    "crate::std::fmt::write_debug(__riddle_debug_{index}, &mut *formatter)?;"
                );
            }
            output.push_str("formatter.write_str(\")\")},");
        } else {
            let braces = variant.field_list().is_some();
            let tuple = has_tuple_fields;
            let _ = write!(output, "{name}::{variant_name}");
            if braces {
                output.push_str(" {}");
            } else if tuple {
                output.push_str("()");
            }
            let _ = write!(output, "=>formatter.write_str({variant_name:?}),");
        }
    }
    output.push_str("} } }");
    Ok(output)
}

pub(super) fn debug_impl_header(
    name: &str,
    generic_params: Option<&ast::GenericParams>,
    where_clause: Option<&ast::WhereClause>,
) -> String {
    let declaration = generic_params
        .map(|params| params.syntax().text().to_string())
        .unwrap_or_default();
    let params = generic_params
        .map(|params| params.params().collect::<Vec<_>>())
        .unwrap_or_default();
    let type_arguments = if params.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            params
                .iter()
                .map(|param| param.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let debug_bounds = params
        .iter()
        .filter(|param| !param.is_const)
        .map(|param| format!("{}: crate::std::fmt::Debug", param.name))
        .collect::<Vec<_>>();
    let mut where_clause = where_clause
        .map(|clause| clause.syntax().text().to_string())
        .unwrap_or_default();
    if !debug_bounds.is_empty() {
        if where_clause.is_empty() {
            where_clause.push_str("where ");
        } else if where_clause.trim_end().ends_with(',') {
            where_clause.push(' ');
        } else {
            where_clause.push_str(", ");
        }
        where_clause.push_str(&debug_bounds.join(", "));
    }

    format!(
        "impl{declaration} crate::std::fmt::Debug for {name}{type_arguments} {where_clause} {{ fun fmt(&self, formatter: &mut crate::std::fmt::Formatter) -> crate::std::fmt::Result {{"
    )
}

pub(super) fn expand_standard_print_macro(
    name: &str,
    input: &ProcMacroTokenStream,
    call_site: &Range<usize>,
) -> Result<ProcMacroTokenStream, (Range<usize>, String)> {
    let arguments = split_macro_arguments(input).map_err(|message| (call_site.clone(), message))?;
    let newline = name == "println";
    let mut body = ProcMacroTokenStream::default();
    if arguments.is_empty() {
        if newline {
            emit_print_call(&mut body, string_token_stream("\n", call_site), call_site);
        }
        return Ok(grouped_stream(ProcMacroDelimiter::Brace, body, call_site));
    }

    let format = &arguments[0];
    let format_span = token_stream_span(format).unwrap_or_else(|| call_site.clone());
    let [ProcMacroTokenTree::Literal { text, .. }] = format.trees.as_slice() else {
        return Err((
            format_span,
            "format argument must be a string literal".into(),
        ));
    };
    if classify_literal(text) != Some(SyntaxKind::String) {
        return Err((
            format_span,
            "format argument must be a string literal".into(),
        ));
    }
    let format = parse_format_literal(text, &format_span)
        .map_err(|message| (format_span.clone(), message))?;
    let values = &arguments[1..];
    let placeholders = format.arguments.len();
    if placeholders != values.len() {
        return Err((
            format_span,
            format!(
                "format string contains {placeholders} placeholder(s), but {} argument(s) were supplied",
                values.len()
            ),
        ));
    }

    for (index, segment) in format.segments.iter().enumerate() {
        if !segment.is_empty() {
            emit_print_call(
                &mut body,
                string_token_stream(segment, call_site),
                call_site,
            );
        }
        if let Some(value) = values.get(index) {
            let span = token_stream_span(value).unwrap_or_else(|| call_site.clone());
            let argument = &format.arguments[index];
            match argument.trait_kind {
                StandardFormatTrait::Display => {
                    emit_io_call(&mut body, "print", value.clone(), &argument.span, &span);
                }
                StandardFormatTrait::Debug => {
                    emit_io_call(
                        &mut body,
                        "print_debug",
                        value.clone(),
                        &argument.span,
                        &span,
                    );
                }
            }
        }
    }
    if newline {
        emit_print_call(&mut body, string_token_stream("\n", call_site), call_site);
    }
    Ok(grouped_stream(ProcMacroDelimiter::Brace, body, call_site))
}

pub(super) fn split_macro_arguments(
    input: &ProcMacroTokenStream,
) -> Result<Vec<ProcMacroTokenStream>, String> {
    let mut arguments = Vec::new();
    let mut current = ProcMacroTokenStream::default();
    for tree in &input.trees {
        if matches!(tree, ProcMacroTokenTree::Punct { value: ',', .. }) {
            if current.is_empty() {
                return Err("expected an argument before `,`".into());
            }
            arguments.push(current);
            current = ProcMacroTokenStream::default();
        } else {
            current.trees.push(tree.clone());
        }
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    Ok(arguments)
}

#[derive(Clone, Copy)]
enum StandardFormatTrait {
    Display,
    Debug,
}

struct StandardFormatString {
    segments: Vec<String>,
    arguments: Vec<StandardFormatArgument>,
}

struct StandardFormatArgument {
    trait_kind: StandardFormatTrait,
    span: Range<usize>,
}

struct DecodedLiteralChar {
    value: char,
    source: Range<usize>,
}

fn parse_format_literal(
    text: &str,
    literal_span: &Range<usize>,
) -> Result<StandardFormatString, String> {
    let chars = decode_string_literal(text).ok_or("invalid format string literal")?;
    let mut segments = vec![String::new()];
    let mut arguments = Vec::new();
    let mut index = 0;
    while let Some(character) = chars.get(index) {
        let next = |offset: usize| chars.get(index + offset).map(|character| character.value);
        match character {
            DecodedLiteralChar { value: '{', .. } if next(1) == Some('{') => {
                segments.last_mut().unwrap().push('{');
                index += 2;
            }
            DecodedLiteralChar { value: '{', source } if next(1) == Some('}') => {
                arguments.push(StandardFormatArgument {
                    trait_kind: StandardFormatTrait::Display,
                    span: literal_source_range(
                        text,
                        literal_span,
                        source.start..chars[index + 1].source.end,
                    ),
                });
                segments.push(String::new());
                index += 2;
            }
            DecodedLiteralChar { value: '{', source } if next(1) == Some(':') => {
                if next(2) != Some('?') || next(3) != Some('}') {
                    return Err(unsupported_format_placeholder_message());
                }
                arguments.push(StandardFormatArgument {
                    trait_kind: StandardFormatTrait::Debug,
                    span: literal_source_range(
                        text,
                        literal_span,
                        source.start..chars[index + 3].source.end,
                    ),
                });
                segments.push(String::new());
                index += 4;
            }
            DecodedLiteralChar { value: '{', .. } => {
                return Err(unsupported_format_placeholder_message());
            }
            DecodedLiteralChar { value: '}', .. } if next(1) == Some('}') => {
                segments.last_mut().unwrap().push('}');
                index += 2;
            }
            DecodedLiteralChar { value: '}', .. } => {
                let close = '}';
                return Err(format!("unmatched `{close}` in format string"));
            }
            character => {
                segments.last_mut().unwrap().push(character.value);
                index += 1;
            }
        }
    }
    Ok(StandardFormatString {
        segments,
        arguments,
    })
}

fn unsupported_format_placeholder_message() -> String {
    let open = '{';
    let close = '}';
    format!("only `{open}{close}` and `{open}:?{close}` format placeholders are supported")
}

pub(super) fn literal_source_range(
    text: &str,
    literal_span: &Range<usize>,
    relative: Range<usize>,
) -> Range<usize> {
    if literal_span.end.saturating_sub(literal_span.start) == text.len() {
        literal_span.start + relative.start..literal_span.start + relative.end
    } else {
        literal_span.clone()
    }
}

fn decode_string_literal(text: &str) -> Option<Vec<DecodedLiteralChar>> {
    let (body, raw) = raw_string_body_range(text)
        .map(|body| (body, true))
        .or_else(|| {
            (text.starts_with('"') && text.ends_with('"') && text.len() >= 2)
                .then_some((1..text.len() - 1, false))
        })?;
    let mut output = Vec::new();
    let mut chars = text[body.clone()].char_indices();
    while let Some((offset, character)) = chars.next() {
        let start = body.start + offset;
        if raw || character != '\\' {
            output.push(DecodedLiteralChar {
                value: character,
                source: start..start + character.len_utf8(),
            });
            continue;
        }
        let (end, value) = match chars.next() {
            Some((offset, 'n')) => (body.start + offset + 1, '\n'),
            Some((offset, 'r')) => (body.start + offset + 1, '\r'),
            Some((offset, 't')) => (body.start + offset + 1, '\t'),
            Some((offset, '0')) => (body.start + offset + 1, '\0'),
            Some((offset, '\\')) => (body.start + offset + 1, '\\'),
            Some((offset, '\'')) => (body.start + offset + 1, '\''),
            Some((offset, '"')) => (body.start + offset + 1, '"'),
            Some((offset, character)) => (body.start + offset + character.len_utf8(), character),
            None => (start + 1, '\\'),
        };
        output.push(DecodedLiteralChar {
            value,
            source: start..end,
        });
    }
    Some(output)
}

pub(super) fn raw_string_body_range(text: &str) -> Option<Range<usize>> {
    let rest = text.strip_prefix('r')?;
    let hashes = rest.bytes().take_while(|&byte| byte == b'#').count();
    let opening_quote = 1 + hashes;
    if text.as_bytes().get(opening_quote) != Some(&b'"') {
        return None;
    }
    let suffix_start = text.len().checked_sub(1 + hashes)?;
    if suffix_start <= opening_quote || text.as_bytes().get(suffix_start) != Some(&b'"') {
        return None;
    }
    text.as_bytes()[suffix_start + 1..]
        .iter()
        .all(|&byte| byte == b'#')
        .then_some(opening_quote + 1..suffix_start)
}

pub(super) fn string_token_stream(value: &str, span: &Range<usize>) -> ProcMacroTokenStream {
    let mut text = String::from("\"");
    for character in value.chars() {
        match character {
            '\0' => text.push_str("\\0"),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            '\\' => text.push_str("\\\\"),
            '"' => text.push_str("\\\""),
            character => text.push(character),
        }
    }
    text.push('"');
    ProcMacroTokenStream {
        trees: vec![ProcMacroTokenTree::Literal {
            text,
            span: span.clone(),
        }],
    }
}

pub(super) fn token_stream_span(stream: &ProcMacroTokenStream) -> Option<Range<usize>> {
    Some(stream.trees.first()?.span().start..stream.trees.last()?.span().end)
}

pub(super) fn grouped_stream(
    delimiter: ProcMacroDelimiter,
    stream: ProcMacroTokenStream,
    span: &Range<usize>,
) -> ProcMacroTokenStream {
    ProcMacroTokenStream {
        trees: vec![ProcMacroTokenTree::Group {
            delimiter,
            stream,
            span: span.clone(),
        }],
    }
}

pub(super) fn emit_print_call(
    output: &mut ProcMacroTokenStream,
    value: ProcMacroTokenStream,
    span: &Range<usize>,
) {
    emit_io_call(output, "print", value, span, span);
}

pub(super) fn emit_io_call(
    output: &mut ProcMacroTokenStream,
    function: &str,
    value: ProcMacroTokenStream,
    callee_span: &Range<usize>,
    value_span: &Range<usize>,
) {
    push_path(output, &["crate", "std", "io", function], callee_span);
    let mut arguments = ProcMacroTokenStream::default();
    arguments.trees.push(ProcMacroTokenTree::Punct {
        value: '&',
        spacing: ProcMacroSpacing::Alone,
        span: value_span.clone(),
    });
    arguments.trees.push(ProcMacroTokenTree::Group {
        delimiter: ProcMacroDelimiter::Parenthesis,
        stream: value,
        span: value_span.clone(),
    });
    output.trees.push(ProcMacroTokenTree::Group {
        delimiter: ProcMacroDelimiter::Parenthesis,
        stream: arguments,
        span: value_span.clone(),
    });
    output.trees.push(ProcMacroTokenTree::Punct {
        value: ';',
        spacing: ProcMacroSpacing::Alone,
        span: callee_span.clone(),
    });
}

pub(super) fn push_path(output: &mut ProcMacroTokenStream, path: &[&str], span: &Range<usize>) {
    for (index, segment) in path.iter().enumerate() {
        if index > 0 {
            output.trees.push(ProcMacroTokenTree::Punct {
                value: ':',
                spacing: ProcMacroSpacing::Joint,
                span: span.clone(),
            });
            output.trees.push(ProcMacroTokenTree::Punct {
                value: ':',
                spacing: ProcMacroSpacing::Alone,
                span: span.clone(),
            });
        }
        output.trees.push(ProcMacroTokenTree::Ident {
            text: (*segment).into(),
            span: span.clone(),
        });
    }
}
