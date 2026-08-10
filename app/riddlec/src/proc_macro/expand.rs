use super::{
    AstNode, Diagnostic, ExpandedSource, HashMap, HashSet, MAX_DERIVE_EXPANSION_DEPTH,
    ProcMacroDefinition, ProcMacroDiagnostic, ProcMacroExpansion, ProcMacroKind,
    ProcMacroOccurrence, ProcMacroProvider, Range, STANDARD_DERIVE_MACROS,
    STANDARD_FUNCTION_MACROS, STANDARD_MACRO_PACKAGE, Severity, StandardMacroProvider, SyntaxKind,
    SyntaxNode, ast,
    document::{
        DocumentToken, ParsedDocument, TokenDocument, checked_output_span,
        document_tokens_from_output, token_stream_from_document,
    },
    lexer,
    scope::{
        build_macro_reexports, collect_scoped_statements, diagnostic, parse_derive_invocations,
        parse_derive_paths, parse_proc_macro_use_resolved, range,
    },
    standard::{
        expand_standard_assert_comparison_macro, expand_standard_assert_macro,
        expand_standard_derive_macro, expand_standard_format_macro, expand_standard_panic_macro,
        expand_standard_panic_shorthand_macro, expand_standard_print_macro,
        expand_standard_quote_macro,
    },
    token_stream::{ProcMacroDelimiter, ProcMacroTokenStream, ProcMacroTokenTree},
};

#[derive(Debug, Clone)]
pub(super) struct ImportedMacro {
    pub(super) package: String,
    pub(super) macro_name: String,
    pub(super) kind: ProcMacroKind,
    pub(super) helper_attributes: Vec<String>,
    pub(super) binding: Option<Range<usize>>,
    pub(super) definition: Option<ProcMacroDefinition>,
}

pub(super) type MacroScope = HashMap<String, ImportedMacro>;

#[derive(Debug, Clone)]
pub(super) struct ScopedStatement {
    pub(super) statement: ast::Stmt,
    pub(super) macros: MacroScope,
}

#[derive(Debug, Clone)]
pub(super) enum DeriveMacroPath {
    Imported(String),
    Qualified { package: String, macro_name: String },
}

#[derive(Debug, Clone)]
pub(super) struct UseBinding {
    pub(super) path: Vec<String>,
    pub(super) alias: Option<String>,
    pub(super) glob: bool,
    pub(super) local_range: Option<Range<usize>>,
}

pub(super) enum ProcMacroUse {
    Ordinary,
    Imports {
        imports: Vec<(String, ImportedMacro)>,
        replacement: Option<String>,
    },
    Invalid {
        message: String,
        replacement: Option<String>,
    },
}

pub(super) type MacroReexports = HashMap<Vec<String>, MacroScope>;

#[derive(Clone)]
enum ExpansionAction {
    Derive {
        item: SyntaxNode,
        attribute: ast::Attribute,
        macros: MacroScope,
    },
    Attribute {
        item: SyntaxNode,
        attribute: ast::Attribute,
        macros: MacroScope,
    },
    FunctionLike {
        call: ast::MacroCall,
        macros: MacroScope,
    },
}

struct ExpansionActionContext<'a> {
    document: &'a mut TokenDocument,
    parsed: &'a ParsedDocument,
    provider: &'a mut dyn ProcMacroProvider,
    original_source: &'a str,
    source_len: usize,
    output_depth: usize,
    diagnostics: &'a mut Vec<Diagnostic>,
}

struct PreparedDeriveAction {
    full_range: Range<usize>,
    input: ProcMacroTokenStream,
    imported: Vec<ImportedMacro>,
}

pub fn expand_source(source: &str, provider: &mut dyn ProcMacroProvider) -> ExpandedSource {
    let mut document = TokenDocument::from_source(source);
    let mut diagnostics = Vec::new();
    let initial = document.parse();
    let macro_occurrences = if initial.parse.errors.is_empty() {
        collect_macro_occurrences(&initial, provider)
    } else {
        Vec::new()
    };

    loop {
        let parsed = document.parse();
        if !parsed.parse.errors.is_empty() {
            let expanded = document.finish();
            return ExpandedSource {
                source: expanded.source,
                parse: Some(expanded.parse),
                mappings: expanded.mappings,
                macro_occurrences,
                diagnostics,
                ..ExpandedSource::default()
            };
        }
        let Some(action) = next_expansion_action(&parsed, provider) else {
            collect_macro_import_diagnostics(&parsed, provider, &mut diagnostics);
            erase_macro_imports(&mut document, &parsed, provider);
            break;
        };
        let action_range = action.range();
        let depth = document.depth_in(&parsed, action_range);
        if depth >= MAX_DERIVE_EXPANSION_DEPTH {
            diagnostics.push(diagnostic(
                action.call_site(&document, &parsed),
                format!(
                    "process macro expansion exceeded the maximum depth of {MAX_DERIVE_EXPANSION_DEPTH}"
                ),
                Severity::Error,
            ));
            if !consume_failed_action(&mut document, &parsed, &action) {
                break;
            }
            continue;
        }

        let context = ExpansionActionContext {
            document: &mut document,
            parsed: &parsed,
            provider,
            original_source: source,
            source_len: source.len(),
            output_depth: depth + 1,
            diagnostics: &mut diagnostics,
        };
        let progressed = match action {
            ExpansionAction::Derive {
                item,
                attribute,
                macros,
            } => expand_derive_action(context, &item, &attribute, &macros),
            ExpansionAction::Attribute {
                item,
                attribute,
                macros,
            } => expand_attribute_action(context, &item, &attribute, &macros),
            ExpansionAction::FunctionLike { call, macros } => {
                expand_function_action(context, &call, &macros)
            }
        };
        if !progressed {
            break;
        }
    }

    let expanded = document.finish();
    ExpandedSource {
        source: expanded.source,
        parse: Some(expanded.parse),
        mappings: expanded.mappings,
        insertions: Vec::new(),
        macro_occurrences,
        diagnostics,
    }
}

pub fn expand_standard_macros(source: &str) -> ExpandedSource {
    expand_source(source, &mut StandardMacroProvider)
}

impl ExpansionAction {
    fn range(&self) -> Range<usize> {
        match self {
            Self::Derive { attribute, .. } | Self::Attribute { attribute, .. } => {
                range(attribute.syntax().text_range())
            }
            Self::FunctionLike { call, .. } => range(call.syntax().text_range()),
        }
    }

    fn call_site(&self, document: &TokenDocument, parsed: &ParsedDocument) -> Range<usize> {
        document.origin_in(parsed, self.range())
    }
}

impl TokenDocument {
    fn origin_in(&self, parsed: &ParsedDocument, range: Range<usize>) -> Range<usize> {
        let mut origins = self
            .tokens
            .iter()
            .zip(&parsed.ranges)
            .filter(|(_, token)| range.start <= token.start && token.end <= range.end)
            .map(|(token, _)| token.original.clone());
        let Some(first) = origins.next() else {
            return range;
        };
        origins.fold(first, |combined, next| {
            combined.start.min(next.start)..combined.end.max(next.end)
        })
    }
}

fn next_expansion_action(
    parsed: &ParsedDocument,
    provider: &dyn ProcMacroProvider,
) -> Option<ExpansionAction> {
    let syntax = parsed.parse.syntax();
    let scopes = macro_scopes(&syntax, provider);
    let mut actions = Vec::new();

    for item in syntax
        .descendants()
        .filter(|node| is_attribute_item(node.kind()))
    {
        let macros = scope_for_range(&scopes, range(item.text_range()));
        for attribute in ast::attrs_for_node(&item) {
            if attribute.name().is_some_and(|name| name.text() == "derive") {
                actions.push(ExpansionAction::Derive {
                    item: item.clone(),
                    attribute,
                    macros: macros.clone(),
                });
                break;
            }
            let Ok((path, _)) = parse_attribute_invocation(&attribute.raw_text()) else {
                continue;
            };
            if is_macro_candidate(&path, ProcMacroKind::Attribute, &macros, provider) {
                actions.push(ExpansionAction::Attribute {
                    item: item.clone(),
                    attribute,
                    macros: macros.clone(),
                });
                break;
            }
        }
    }

    for node in syntax
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::MacroCall)
    {
        let Some(call) = ast::MacroCall::cast(node) else {
            continue;
        };
        let macros = scope_for_range(&scopes, range(call.syntax().text_range()));
        actions.push(ExpansionAction::FunctionLike { call, macros });
    }

    actions
        .into_iter()
        .min_by_key(|action| action.range().start)
}

pub(super) const fn is_attribute_item(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::FuncDecl
            | SyntaxKind::StructDecl
            | SyntaxKind::EnumDecl
            | SyntaxKind::ModDecl
            | SyntaxKind::UseDecl
            | SyntaxKind::TraitDecl
            | SyntaxKind::ImplDecl
            | SyntaxKind::ConstDecl
            | SyntaxKind::TypeAliasDecl
            | SyntaxKind::ExternBlock
            | SyntaxKind::ExternFnDecl
    )
}

pub(super) fn macro_scopes(
    syntax: &SyntaxNode,
    provider: &dyn ProcMacroProvider,
) -> Vec<ScopedStatement> {
    let Some(root) = ast::Root::cast(syntax.clone()) else {
        return Vec::new();
    };
    let reexports = build_macro_reexports(&root, provider);
    let mut scopes = Vec::new();
    collect_scoped_statements(
        root.stmts().collect(),
        &MacroScope::default(),
        provider,
        Some(&reexports),
        &[],
        &mut scopes,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    scopes
}

pub(super) fn collect_macro_occurrences(
    parsed: &ParsedDocument,
    provider: &dyn ProcMacroProvider,
) -> Vec<ProcMacroOccurrence> {
    let syntax = parsed.parse.syntax();
    let scopes = macro_scopes(&syntax, provider);
    let mut occurrences = Vec::new();
    let mut declarations = HashSet::new();

    for scoped in &scopes {
        for (name, imported) in &scoped.macros {
            let Some(binding) = imported.binding.clone() else {
                continue;
            };
            if declarations.insert((binding.start, binding.end)) {
                occurrences.push(macro_occurrence(name, imported, binding, true));
            }
        }
    }

    for item in syntax
        .descendants()
        .filter(|node| is_attribute_item(node.kind()))
    {
        let macros = scope_for_range(&scopes, range(item.text_range()));
        for attribute in ast::attrs_for_node(&item) {
            let attribute_start = usize::from(attribute.syntax().text_range().start());
            if attribute.name().is_some_and(|name| name.text() == "derive") {
                let Ok(invocations) = parse_derive_invocations(&attribute.raw_text()) else {
                    continue;
                };
                for (path, name_range) in invocations {
                    let path = match path {
                        DeriveMacroPath::Imported(name) => vec![name],
                        DeriveMacroPath::Qualified {
                            package,
                            macro_name,
                        } => vec![package, macro_name],
                    };
                    let Ok(imported) =
                        resolve_macro(&path, ProcMacroKind::Derive, &macros, provider)
                    else {
                        continue;
                    };
                    let name_range =
                        name_range.start + attribute_start..name_range.end + attribute_start;
                    let name = parsed.source[name_range.clone()].to_string();
                    occurrences.push(macro_occurrence(&name, &imported, name_range, false));
                }
                continue;
            }

            let Ok((path, name_range, _)) =
                parse_attribute_invocation_spanned(&attribute.raw_text())
            else {
                continue;
            };
            if !is_macro_candidate(&path, ProcMacroKind::Attribute, &macros, provider) {
                continue;
            }
            let Ok(imported) = resolve_macro(&path, ProcMacroKind::Attribute, &macros, provider)
            else {
                continue;
            };
            let name_range = name_range.start + attribute_start..name_range.end + attribute_start;
            let name = parsed.source[name_range.clone()].to_string();
            occurrences.push(macro_occurrence(&name, &imported, name_range, false));
        }
    }

    for node in syntax
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::MacroCall)
    {
        let Some(call) = ast::MacroCall::cast(node) else {
            continue;
        };
        let macros = scope_for_range(&scopes, range(call.syntax().text_range()));
        let Some(path) = call.path() else {
            continue;
        };
        let segments = path
            .segments()
            .filter_map(|segment| segment.name_token())
            .collect::<Vec<_>>();
        let names = segments
            .iter()
            .map(|token| token.text().to_string())
            .collect::<Vec<_>>();
        let Some(name) = segments.last() else {
            continue;
        };
        let Ok(imported) = resolve_macro(&names, ProcMacroKind::FunctionLike, &macros, provider)
        else {
            continue;
        };
        occurrences.push(macro_occurrence(
            name.text(),
            &imported,
            range(name.text_range()),
            false,
        ));
    }

    normalize_macro_occurrences(&mut occurrences);
    occurrences
}

fn normalize_macro_occurrences(occurrences: &mut Vec<ProcMacroOccurrence>) {
    occurrences.sort_by_key(|occurrence| {
        (
            occurrence.range.start,
            occurrence.range.end,
            !occurrence.is_declaration,
        )
    });
    occurrences.dedup_by(|left, right| {
        left.range == right.range
            && left.kind == right.kind
            && left.is_declaration == right.is_declaration
    });
}

pub(super) fn macro_occurrence(
    name: &str,
    imported: &ImportedMacro,
    range: Range<usize>,
    is_declaration: bool,
) -> ProcMacroOccurrence {
    ProcMacroOccurrence {
        name: name.into(),
        package: imported.package.clone(),
        macro_name: imported.macro_name.clone(),
        kind: imported.kind,
        range,
        binding: imported.binding.clone(),
        definition: imported.definition.clone(),
        is_declaration,
    }
}

pub(super) fn scope_for_range(scopes: &[ScopedStatement], target: Range<usize>) -> MacroScope {
    scopes
        .iter()
        .filter(|scoped| {
            let item = range(scoped.statement.syntax().text_range());
            item.start <= target.start && target.end <= item.end
        })
        .min_by_key(|scoped| {
            let item = range(scoped.statement.syntax().text_range());
            item.end - item.start
        })
        .map(|scoped| scoped.macros.clone())
        .unwrap_or_default()
}

pub(super) fn is_macro_candidate(
    path: &[String],
    kind: ProcMacroKind,
    macros: &MacroScope,
    provider: &dyn ProcMacroProvider,
) -> bool {
    match path {
        [name] => macros
            .get(name)
            .is_some_and(|imported| imported.kind == kind),
        [package, name] => provider.exports(package).is_some_and(|exports| {
            exports
                .iter()
                .any(|export| export.name == *name && export.kind == kind)
        }),
        _ => false,
    }
}

pub(super) fn resolve_macro(
    path: &[String],
    expected: ProcMacroKind,
    macros: &MacroScope,
    provider: &dyn ProcMacroProvider,
) -> Result<ImportedMacro, String> {
    let imported = match path {
        [name] => macros
            .get(name)
            .cloned()
            .or_else(|| standard_macro(name, expected))
            .ok_or_else(|| {
                if expected == ProcMacroKind::Derive {
                    format!(
                        "cannot find derive macro `{name}` in this scope; import it with `use package::{name};`"
                    )
                } else {
                    format!("cannot find macro `{name}` in this scope")
                }
            })?,
        [package, name] if package == STANDARD_MACRO_PACKAGE => standard_macro(name, expected)
            .ok_or_else(|| format!("unknown standard macro `{package}::{name}`"))?,
        [package, name] => {
            if let Some(exports) = provider.exports(package) {
                let export = exports
                    .into_iter()
                    .find(|export| export.name == *name)
                    .ok_or_else(|| match expected {
                        ProcMacroKind::Derive => {
                            format!("unknown proc-macro derive `{package}::{name}`")
                        }
                        ProcMacroKind::Attribute => {
                            format!("unknown proc-macro attribute `{package}::{name}`")
                        }
                        ProcMacroKind::FunctionLike => {
                            format!("unknown function-like proc macro `{package}::{name}`")
                        }
                    })?;
                ImportedMacro {
                    package: package.clone(),
                    macro_name: name.clone(),
                    kind: export.kind,
                    helper_attributes: export.helper_attributes,
                    binding: None,
                    definition: provider.definition(package, name),
                }
            } else {
                ImportedMacro {
                    package: package.clone(),
                    macro_name: name.clone(),
                    kind: expected,
                    helper_attributes: Vec::new(),
                    binding: None,
                    definition: provider.definition(package, name),
                }
            }
        }
        _ => return Err("process macro paths must contain one or two segments".into()),
    };
    if imported.kind != expected {
        return Err(format!(
            "{} is a {:?} macro, not a {:?} macro",
            path.join("::"),
            imported.kind,
            expected
        ));
    }
    Ok(imported)
}

pub(super) fn standard_macro(name: &str, kind: ProcMacroKind) -> Option<ImportedMacro> {
    ((kind == ProcMacroKind::FunctionLike && STANDARD_FUNCTION_MACROS.contains(&name))
        || (kind == ProcMacroKind::Derive && STANDARD_DERIVE_MACROS.contains(&name)))
    .then(|| ImportedMacro {
        package: STANDARD_MACRO_PACKAGE.into(),
        macro_name: name.into(),
        kind,
        helper_attributes: if name == "Default" {
            vec!["default".into()]
        } else {
            Vec::new()
        },
        binding: None,
        definition: None,
    })
}

pub(super) fn macro_call_input(tokens: &[DocumentToken]) -> Result<ProcMacroTokenStream, String> {
    let stream = token_stream_from_document(tokens)?;
    stream
        .trees
        .into_iter()
        .rev()
        .find_map(|tree| match tree {
            ProcMacroTokenTree::Group { stream, .. } => Some(stream),
            _ => None,
        })
        .ok_or_else(|| "function-like macro call has no delimited input".into())
}

pub(super) fn parse_attribute_invocation(
    raw: &str,
) -> Result<(Vec<String>, ProcMacroTokenStream), String> {
    parse_attribute_invocation_spanned(raw).map(|(path, _, args)| (path, args))
}

pub(super) fn parse_attribute_invocation_spanned(
    raw: &str,
) -> Result<(Vec<String>, Range<usize>, ProcMacroTokenStream), String> {
    let stream = ProcMacroTokenStream::from_source(raw, 0)?;
    let [
        ProcMacroTokenTree::Punct { value: '#', .. },
        ProcMacroTokenTree::Group {
            delimiter: ProcMacroDelimiter::Bracket,
            stream: attribute,
            ..
        },
    ] = stream.trees.as_slice()
    else {
        return Err("invalid attribute syntax".into());
    };
    let mut path = Vec::new();
    let mut name_range = None;
    let mut index = 0usize;
    while let Some(ProcMacroTokenTree::Ident { text, span }) = attribute.trees.get(index) {
        path.push(text.clone());
        name_range = Some(span.clone());
        index += 1;
        if !matches!(
            attribute.trees.get(index..index + 2),
            Some([
                ProcMacroTokenTree::Punct { value: ':', .. },
                ProcMacroTokenTree::Punct { value: ':', .. }
            ])
        ) {
            break;
        }
        index += 2;
    }
    if path.is_empty() {
        return Err("attribute has no name".into());
    }
    let args = match attribute.trees.get(index) {
        Some(ProcMacroTokenTree::Group {
            delimiter: ProcMacroDelimiter::Parenthesis,
            stream: arguments,
            ..
        }) if index + 1 == attribute.trees.len() => arguments.clone(),
        _ => ProcMacroTokenStream {
            trees: attribute.trees[index..].to_vec(),
        },
    };
    Ok((path, name_range.unwrap(), args))
}

pub(super) fn item_range_with_attrs(item: &SyntaxNode, attrs: &[ast::Attribute]) -> Range<usize> {
    let item = range(item.text_range());
    attrs
        .first()
        .map(|attr| range(attr.syntax().text_range()).start..item.end)
        .unwrap_or(item)
}

pub(super) fn append_macro_diagnostics(
    diagnostics: &mut Vec<Diagnostic>,
    incoming: Vec<ProcMacroDiagnostic>,
    call_site: &Range<usize>,
    source_len: usize,
) -> bool {
    let has_error = incoming
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error);
    diagnostics.extend(incoming.into_iter().map(|incoming| {
        let span = checked_output_span(&incoming.span, call_site, source_len);
        diagnostic(span, incoming.message, incoming.severity)
    }));
    has_error
}

pub(super) fn replace_if_parses(
    document: &mut TokenDocument,
    parsed: &ParsedDocument,
    range: Range<usize>,
    replacement: Vec<DocumentToken>,
    call_site: &Range<usize>,
    message: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let mut candidate = document.clone();
    candidate.replace(parsed, range, replacement);
    let parse = candidate.parse();
    if parse.parse.errors.is_empty() {
        *document = candidate;
        true
    } else {
        let detail = parse
            .parse
            .errors
            .first()
            .map(|error| format!(": {}", error.message))
            .unwrap_or_default();
        diagnostics.push(diagnostic(
            call_site.clone(),
            format!("{message}{detail}"),
            Severity::Error,
        ));
        false
    }
}

pub(super) fn validate_item_output(tokens: &[DocumentToken], message: &str) -> Result<(), String> {
    let parsed = TokenDocument {
        tokens: tokens.to_vec(),
    }
    .parse();
    if let Some(error) = parsed.parse.errors.first() {
        return Err(format!("{message}: {}", error.message));
    }
    let root = ast::Root::cast(parsed.parse.syntax()).expect("parsed root should be a root node");
    if root.stmts().all(|statement| {
        matches!(
            statement,
            ast::Stmt::FuncDecl(_)
                | ast::Stmt::StructDecl(_)
                | ast::Stmt::EnumDecl(_)
                | ast::Stmt::TraitDecl(_)
                | ast::Stmt::ImplDecl(_)
                | ast::Stmt::ConstDecl(_)
                | ast::Stmt::TypeAliasDecl(_)
                | ast::Stmt::ModDecl(_)
                | ast::Stmt::UseDecl(_)
                | ast::Stmt::ExternBlock(_)
                | ast::Stmt::ExternFnDecl(_)
        ) || matches!(statement, ast::Stmt::ExprStmt(ref expression)
            if expression.syntax().descendants().any(|node| node.kind() == SyntaxKind::MacroCall))
    }) {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn validate_derive_helper_attributes(
    document: &TokenDocument,
    parsed: &ParsedDocument,
    item: &SyntaxNode,
    item_attrs: &[ast::Attribute],
    helpers: &HashSet<String>,
    macros: &MacroScope,
    provider: &dyn ProcMacroProvider,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let mut valid = true;
    for attribute in item_attrs.iter().cloned().chain(
        item.descendants()
            .filter(|node| node.kind() == SyntaxKind::Attribute)
            .filter_map(ast::Attribute::cast),
    ) {
        let Some(name) = attribute.name().map(|name| name.text().to_string()) else {
            continue;
        };
        if matches!(
            name.as_str(),
            "derive"
                | "lang"
                | "fundamental"
                | "builtin"
                | "c_export"
                | "proc_macro"
                | "proc_macro_attribute"
                | "proc_macro_derive"
        ) || helpers.contains(&name)
        {
            continue;
        }
        if parse_attribute_invocation(&attribute.raw_text())
            .ok()
            .and_then(|(path, _)| {
                resolve_macro(&path, ProcMacroKind::Attribute, macros, provider).ok()
            })
            .is_some()
        {
            continue;
        }
        let attribute_range = range(attribute.syntax().text_range());
        diagnostics.push(diagnostic(
            document.origin_in(parsed, attribute_range),
            format!(
                "cannot find attribute `{name}` in this scope; derive helper attributes must be declared with `attributes({name})`"
            ),
            Severity::Error,
        ));
        valid = false;
    }
    valid
}

pub(super) fn space_token(original: Range<usize>, depth: usize) -> DocumentToken {
    DocumentToken {
        kind: SyntaxKind::Whitespace,
        text: "\n".into(),
        original,
        depth,
        generated: true,
    }
}

pub(super) fn erased_token(
    document: &TokenDocument,
    parsed: &ParsedDocument,
    range: Range<usize>,
    depth: usize,
) -> DocumentToken {
    let text = parsed.source[range.clone()]
        .bytes()
        .map(|byte| match byte {
            b'\n' | b'\r' | b'\t' => byte,
            _ => b' ',
        })
        .collect::<Vec<_>>();
    DocumentToken {
        kind: SyntaxKind::Whitespace,
        text: String::from_utf8(text).expect("erased macro syntax is ASCII whitespace"),
        original: document.origin_in(parsed, range),
        depth,
        generated: true,
    }
}

pub(super) fn erase_range(
    document: &mut TokenDocument,
    parsed: &ParsedDocument,
    range: Range<usize>,
    depth: usize,
) {
    let erased = erased_token(document, parsed, range.clone(), depth);
    document.replace(parsed, range, vec![erased]);
}

fn consume_failed_action(
    document: &mut TokenDocument,
    parsed: &ParsedDocument,
    action: &ExpansionAction,
) -> bool {
    match action {
        ExpansionAction::Derive { attribute, .. }
        | ExpansionAction::Attribute { attribute, .. } => {
            let range = range(attribute.syntax().text_range());
            let erased = erased_token(document, parsed, range.clone(), 0);
            document.replace(parsed, range, vec![erased]);
            true
        }
        ExpansionAction::FunctionLike { .. } => false,
    }
}

pub(super) fn collect_macro_import_diagnostics(
    parsed: &ParsedDocument,
    provider: &dyn ProcMacroProvider,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(root) = ast::Root::cast(parsed.parse.syntax()) else {
        return;
    };
    let reexports = build_macro_reexports(&root, provider);
    collect_scoped_statements(
        root.stmts().collect(),
        &MacroScope::default(),
        provider,
        Some(&reexports),
        &[],
        &mut Vec::new(),
        &mut Vec::new(),
        diagnostics,
    );
}

pub(super) fn erase_macro_imports(
    document: &mut TokenDocument,
    parsed: &ParsedDocument,
    provider: &dyn ProcMacroProvider,
) {
    let syntax = parsed.parse.syntax();
    let Some(root) = ast::Root::cast(syntax.clone()) else {
        return;
    };
    let reexports = build_macro_reexports(&root, provider);
    let mut edits = syntax
        .descendants()
        .filter_map(ast::UseDecl::cast)
        .filter_map(|use_decl| {
            let module_path = module_path_for_node(use_decl.syntax());
            let replacement = match parse_proc_macro_use_resolved(
                &use_decl,
                provider,
                &reexports,
                &module_path,
            ) {
                ProcMacroUse::Ordinary => return None,
                ProcMacroUse::Imports { replacement, .. }
                | ProcMacroUse::Invalid { replacement, .. } => replacement,
            };
            Some((range(use_decl.syntax().text_range()), replacement))
        })
        .collect::<Vec<_>>();
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    for (range, replacement) in edits {
        let tokens = replacement.map_or_else(
            || vec![erased_token(document, parsed, range.clone(), 0)],
            |replacement| replacement_tokens(document, parsed, range.clone(), &replacement),
        );
        document.replace(parsed, range, tokens);
    }
}

pub(super) fn module_path_for_node(node: &SyntaxNode) -> Vec<String> {
    let mut path = node
        .ancestors()
        .filter_map(ast::ModDecl::cast)
        .filter_map(|module| module.name().map(|name| name.text().to_string()))
        .collect::<Vec<_>>();
    path.reverse();
    path
}

pub(super) fn replacement_tokens(
    document: &TokenDocument,
    parsed: &ParsedDocument,
    range: Range<usize>,
    replacement: &str,
) -> Vec<DocumentToken> {
    let original = document.origin_in(parsed, range);
    lexer::lex(replacement)
        .into_iter()
        .map(|token| DocumentToken {
            kind: token.kind,
            text: token.text(replacement).into(),
            original: original.clone(),
            depth: 0,
            generated: true,
        })
        .collect()
}

fn expand_derive_action(
    mut context: ExpansionActionContext<'_>,
    item: &SyntaxNode,
    attribute: &ast::Attribute,
    macros: &MacroScope,
) -> bool {
    let call_range = range(attribute.syntax().text_range());
    let call_site = context
        .document
        .origin_in(context.parsed, call_range.clone());
    let Some(prepared) = prepare_derive_action(
        &mut context,
        item,
        attribute,
        macros,
        &call_range,
        &call_site,
    ) else {
        return true;
    };
    let PreparedDeriveAction {
        full_range,
        input,
        imported,
    } = prepared;
    let ExpansionActionContext {
        document,
        parsed,
        provider,
        original_source: _,
        source_len,
        output_depth,
        diagnostics,
    } = context;

    let mut generated = Vec::new();
    for imported in imported {
        let expansion = if imported.package == STANDARD_MACRO_PACKAGE {
            match expand_standard_derive_macro(&imported.macro_name, item, &call_site) {
                Ok(output) => Ok(ProcMacroExpansion {
                    output,
                    diagnostics: Vec::new(),
                }),
                Err(message) => Err(message),
            }
        } else {
            provider.expand(
                &imported.package,
                &imported.macro_name,
                ProcMacroKind::Derive,
                &input,
                None,
                call_site.clone(),
            )
        };
        append_derive_expansion(
            expansion,
            &imported,
            &call_site,
            source_len,
            output_depth,
            diagnostics,
            &mut generated,
        );
    }

    let mut replacement = document.tokens_in_replacing(
        parsed,
        full_range.clone(),
        call_range.clone(),
        erased_token(document, parsed, call_range, output_depth),
    );
    if !generated.is_empty() {
        replacement.push(space_token(call_site.clone(), output_depth));
        replacement.extend(generated);
    }
    replace_if_parses(
        document,
        parsed,
        full_range,
        replacement,
        &call_site,
        "derive macro output is not valid Riddle",
        diagnostics,
    )
}

fn append_derive_expansion(
    expansion: Result<ProcMacroExpansion, String>,
    imported: &ImportedMacro,
    call_site: &Range<usize>,
    source_len: usize,
    output_depth: usize,
    diagnostics: &mut Vec<Diagnostic>,
    generated: &mut Vec<DocumentToken>,
) {
    let expansion = match expansion {
        Ok(expansion) => expansion,
        Err(message) => {
            diagnostics.push(diagnostic(
                call_site.clone(),
                format!(
                    "failed to expand {}::{}: {message}",
                    imported.package, imported.macro_name
                ),
                Severity::Error,
            ));
            return;
        }
    };
    if append_macro_diagnostics(diagnostics, expansion.diagnostics, call_site, source_len) {
        return;
    }
    let mut output =
        match document_tokens_from_output(&expansion.output, call_site, source_len, output_depth) {
            Ok(output) => output,
            Err(message) => {
                diagnostics.push(diagnostic(call_site.clone(), message, Severity::Error));
                return;
            }
        };
    if let Err(message) = validate_item_output(
        &output,
        "derive macro output must contain only top-level items",
    ) {
        diagnostics.push(diagnostic(call_site.clone(), message, Severity::Error));
        return;
    }
    if !generated.is_empty() && !output.is_empty() {
        generated.push(space_token(call_site.clone(), output_depth));
    }
    generated.append(&mut output);
}

fn prepare_derive_action(
    context: &mut ExpansionActionContext<'_>,
    item: &SyntaxNode,
    attribute: &ast::Attribute,
    macros: &MacroScope,
    call_range: &Range<usize>,
    call_site: &Range<usize>,
) -> Option<PreparedDeriveAction> {
    if !matches!(item.kind(), SyntaxKind::StructDecl | SyntaxKind::EnumDecl) {
        context.diagnostics.push(diagnostic(
            call_site.clone(),
            "derive macros may only be applied to structs or enums".into(),
            Severity::Error,
        ));
        erase_range(
            context.document,
            context.parsed,
            call_range.clone(),
            context.output_depth,
        );
        return None;
    }

    let attrs = ast::attrs_for_node(item);
    let full_range = item_range_with_attrs(item, &attrs);
    let derive_ranges = attrs
        .iter()
        .filter(|attr| attr.name().is_some_and(|name| name.text() == "derive"))
        .map(|attr| range(attr.syntax().text_range()))
        .collect::<Vec<_>>();
    let input_tokens =
        context
            .document
            .tokens_in_without(context.parsed, full_range.clone(), &derive_ranges);
    let input = match token_stream_from_document(&input_tokens) {
        Ok(input) => input,
        Err(message) => {
            context
                .diagnostics
                .push(diagnostic(call_site.clone(), message, Severity::Error));
            erase_range(
                context.document,
                context.parsed,
                call_range.clone(),
                context.output_depth,
            );
            return None;
        }
    };
    let paths = match parse_derive_paths(&attribute.raw_text()) {
        Ok(paths) => paths,
        Err(message) => {
            context
                .diagnostics
                .push(diagnostic(call_site.clone(), message, Severity::Error));
            erase_range(
                context.document,
                context.parsed,
                call_range.clone(),
                context.output_depth,
            );
            return None;
        }
    };

    let mut imported = Vec::new();
    for path in paths {
        let path = match path {
            DeriveMacroPath::Imported(name) => vec![name],
            DeriveMacroPath::Qualified {
                package,
                macro_name,
            } => vec![package, macro_name],
        };
        match resolve_macro(&path, ProcMacroKind::Derive, macros, context.provider) {
            Ok(resolved) => imported.push(resolved),
            Err(message) => {
                context
                    .diagnostics
                    .push(diagnostic(call_site.clone(), message, Severity::Error));
            }
        }
    }
    let helpers = imported
        .iter()
        .flat_map(|imported| imported.helper_attributes.iter().cloned())
        .collect::<HashSet<_>>();
    if !validate_derive_helper_attributes(
        context.document,
        context.parsed,
        item,
        &attrs,
        &helpers,
        macros,
        context.provider,
        context.diagnostics,
    ) {
        erase_range(
            context.document,
            context.parsed,
            call_range.clone(),
            context.output_depth,
        );
        return None;
    }
    Some(PreparedDeriveAction {
        full_range,
        input,
        imported,
    })
}

fn expand_attribute_action(
    context: ExpansionActionContext<'_>,
    item: &SyntaxNode,
    attribute: &ast::Attribute,
    macros: &MacroScope,
) -> bool {
    let ExpansionActionContext {
        document,
        parsed,
        provider,
        original_source: _,
        source_len,
        output_depth,
        diagnostics,
    } = context;
    let attribute_range = range(attribute.syntax().text_range());
    let call_site = document.origin_in(parsed, attribute_range.clone());
    let (path, args) = match parse_attribute_invocation(&attribute.raw_text()) {
        Ok(parsed) => parsed,
        Err(message) => {
            diagnostics.push(diagnostic(call_site, message, Severity::Error));
            erase_range(document, parsed, attribute_range, output_depth);
            return true;
        }
    };
    let imported = match resolve_macro(&path, ProcMacroKind::Attribute, macros, provider) {
        Ok(imported) => imported,
        Err(message) => {
            diagnostics.push(diagnostic(call_site, message, Severity::Error));
            erase_range(document, parsed, attribute_range, output_depth);
            return true;
        }
    };
    let attrs = ast::attrs_for_node(item);
    let full_range = item_range_with_attrs(item, &attrs);
    let item_tokens = document.tokens_in_without(
        parsed,
        full_range.clone(),
        std::slice::from_ref(&attribute_range),
    );
    let item_input = match token_stream_from_document(&item_tokens) {
        Ok(input) => input,
        Err(message) => {
            diagnostics.push(diagnostic(call_site, message, Severity::Error));
            return false;
        }
    };
    let expansion = match provider.expand(
        &imported.package,
        &imported.macro_name,
        ProcMacroKind::Attribute,
        &args,
        Some(&item_input),
        call_site.clone(),
    ) {
        Ok(expansion) => expansion,
        Err(message) => {
            diagnostics.push(diagnostic(
                call_site,
                format!(
                    "failed to expand {}::{}: {message}",
                    imported.package, imported.macro_name
                ),
                Severity::Error,
            ));
            return false;
        }
    };
    let has_error =
        append_macro_diagnostics(diagnostics, expansion.diagnostics, &call_site, source_len);
    let mut replacement = if has_error {
        item_tokens
    } else {
        match document_tokens_from_output(&expansion.output, &call_site, source_len, output_depth) {
            Ok(output) => {
                if let Err(message) = validate_item_output(
                    &output,
                    "attribute macro output must contain only top-level items",
                ) {
                    diagnostics.push(diagnostic(call_site, message, Severity::Error));
                    return false;
                }
                output
            }
            Err(message) => {
                diagnostics.push(diagnostic(call_site, message, Severity::Error));
                return false;
            }
        }
    };
    replacement.insert(
        0,
        erased_token(document, parsed, attribute_range, output_depth),
    );
    replace_if_parses(
        document,
        parsed,
        full_range,
        replacement,
        &call_site,
        "attribute macro output is not valid Riddle",
        diagnostics,
    )
}

fn expand_function_action(
    context: ExpansionActionContext<'_>,
    call: &ast::MacroCall,
    macros: &MacroScope,
) -> bool {
    let ExpansionActionContext {
        document,
        parsed,
        provider,
        original_source,
        source_len,
        output_depth,
        diagnostics,
    } = context;
    let call_range = range(call.syntax().text_range());
    let call_site = document.origin_in(parsed, call_range.clone());
    let path = call
        .path()
        .map(|path| {
            path.segments()
                .filter_map(|segment| segment.name_token().map(|token| token.text().to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let imported = match resolve_macro(&path, ProcMacroKind::FunctionLike, macros, provider) {
        Ok(imported) => imported,
        Err(message) => {
            diagnostics.push(diagnostic(call_site, message, Severity::Error));
            return false;
        }
    };
    let call_tokens = document.tokens_in(parsed, call_range.clone());
    let input = match macro_call_input(&call_tokens) {
        Ok(input) => input,
        Err(message) => {
            diagnostics.push(diagnostic(call_site, message, Severity::Error));
            return false;
        }
    };
    let output = if imported.package == STANDARD_MACRO_PACKAGE {
        let expanded = match imported.macro_name.as_str() {
            "quote" => expand_standard_quote_macro(&input, &call_site),
            "format" => expand_standard_format_macro(&input, &call_site),
            "panic" => expand_standard_panic_macro(&input, &call_site, original_source),
            "print" | "println" => {
                expand_standard_print_macro(&imported.macro_name, &input, &call_site)
            }
            "assert" | "debug_assert" => {
                // ponytail: debug assertions stay enabled until Riddle gains build-profile cfg.
                expand_standard_assert_macro(&imported.macro_name, &input, &call_site)
            }
            "assert_eq" | "assert_ne" | "debug_assert_eq" | "debug_assert_ne" => {
                expand_standard_assert_comparison_macro(&imported.macro_name, &input, &call_site)
            }
            "todo" | "unimplemented" | "unreachable" => {
                expand_standard_panic_shorthand_macro(&imported.macro_name, &input, &call_site)
            }
            _ => unreachable!("registered standard macro must have an expander"),
        };
        match expanded {
            Ok(output) => output,
            Err((span, message)) => {
                diagnostics.push(diagnostic(span, message, Severity::Error));
                return false;
            }
        }
    } else {
        let expansion = match provider.expand(
            &imported.package,
            &imported.macro_name,
            ProcMacroKind::FunctionLike,
            &input,
            None,
            call_site.clone(),
        ) {
            Ok(expansion) => expansion,
            Err(message) => {
                diagnostics.push(diagnostic(
                    call_site,
                    format!(
                        "failed to expand {}::{}: {message}",
                        imported.package, imported.macro_name
                    ),
                    Severity::Error,
                ));
                return false;
            }
        };
        if append_macro_diagnostics(diagnostics, expansion.diagnostics, &call_site, source_len) {
            return false;
        }
        expansion.output
    };
    let mut replacement =
        match document_tokens_from_output(&output, &call_site, source_len, output_depth) {
            Ok(output) => output,
            Err(message) => {
                diagnostics.push(diagnostic(call_site, message, Severity::Error));
                return false;
            }
        };
    replacement.insert(
        0,
        erased_token(document, parsed, call_range.clone(), output_depth),
    );

    replace_function_output(
        document,
        parsed,
        call,
        call_range,
        replacement,
        call_site,
        diagnostics,
    )
}

fn replace_function_output(
    document: &mut TokenDocument,
    parsed: &ParsedDocument,
    call: &ast::MacroCall,
    call_range: Range<usize>,
    replacement: Vec<DocumentToken>,
    call_site: Range<usize>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let mut candidate = document.clone();
    candidate.replace(parsed, call_range, replacement.clone());
    if candidate.parse().parse.errors.is_empty() {
        *document = candidate;
        return true;
    }
    if let Some(statement) = call
        .syntax()
        .ancestors()
        .find(|node| node.kind() == SyntaxKind::ExprStmt)
    {
        let mut candidate = document.clone();
        candidate.replace(parsed, range(statement.text_range()), replacement);
        if candidate.parse().parse.errors.is_empty() {
            *document = candidate;
            return true;
        }
    }
    diagnostics.push(diagnostic(
        call_site,
        "function-like macro output is not valid in this position".into(),
        Severity::Error,
    ));
    false
}
