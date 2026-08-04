use super::{expand::*, *};

pub(super) fn build_macro_reexports(
    root: &ast::Root,
    provider: &dyn ProcMacroProvider,
) -> MacroReexports {
    let mut modules = Vec::new();
    collect_module_uses(root.stmts().collect(), &[], &mut modules);
    let mut reexports = modules
        .iter()
        .map(|(path, _)| (path.clone(), MacroScope::default()))
        .collect::<MacroReexports>();

    loop {
        let snapshot = reexports.clone();
        let mut changed = false;
        for (path, uses) in &modules {
            for use_decl in uses.iter().filter(|use_decl| use_decl.is_pub()) {
                let ProcMacroUse::Imports { imports, .. } =
                    parse_proc_macro_use_resolved(use_decl, provider, &snapshot, path)
                else {
                    continue;
                };
                let exported = reexports.entry(path.clone()).or_default();
                for (name, imported) in imports {
                    if exported.get(&name).is_none_or(|existing| {
                        existing.package != imported.package
                            || existing.macro_name != imported.macro_name
                            || existing.kind != imported.kind
                    }) {
                        exported.insert(name, imported);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    reexports
}

pub(super) fn collect_module_uses(
    statements: Vec<ast::Stmt>,
    path: &[String],
    output: &mut Vec<(Vec<String>, Vec<ast::UseDecl>)>,
) {
    output.push((
        path.to_vec(),
        statements
            .iter()
            .filter_map(|statement| match statement {
                ast::Stmt::UseDecl(use_decl) => Some(use_decl.clone()),
                _ => None,
            })
            .collect(),
    ));
    for statement in statements {
        let ast::Stmt::ModDecl(module) = statement else {
            continue;
        };
        let Some(items) = module.items() else {
            continue;
        };
        let Some(name) = module.name().map(|name| name.text().to_string()) else {
            continue;
        };
        let mut child_path = path.to_vec();
        child_path.push(name);
        collect_module_uses(items.collect(), &child_path, output);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_scoped_statements(
    statements: Vec<ast::Stmt>,
    inherited_macros: &MacroScope,
    provider: &dyn ProcMacroProvider,
    reexports: Option<&MacroReexports>,
    module_path: &[String],
    output: &mut Vec<ScopedStatement>,
    erased_imports: &mut Vec<Range<usize>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut macros = inherited_macros.clone();
    for statement in &statements {
        let ast::Stmt::UseDecl(use_decl) = statement else {
            continue;
        };
        let use_range = range(use_decl.syntax().text_range());
        match reexports.map_or_else(
            || parse_proc_macro_use(use_decl, provider),
            |reexports| parse_proc_macro_use_resolved(use_decl, provider, reexports, module_path),
        ) {
            ProcMacroUse::Ordinary => {}
            ProcMacroUse::Imports {
                imports,
                replacement,
            } => {
                if replacement.is_none() {
                    erased_imports.push(use_range.clone());
                }
                for (local_name, imported) in imports {
                    match macros.entry(local_name) {
                        Entry::Occupied(entry) => diagnostics.push(diagnostic(
                            use_range.clone(),
                            format!("macro `{}` is imported more than once", entry.key()),
                            Severity::Error,
                        )),
                        Entry::Vacant(entry) => {
                            entry.insert(imported);
                        }
                    }
                }
            }
            ProcMacroUse::Invalid {
                message,
                replacement,
            } => {
                if replacement.is_none() {
                    erased_imports.push(use_range.clone());
                }
                diagnostics.push(diagnostic(use_range, message, Severity::Error));
            }
        }
    }

    for statement in statements {
        let (children, child_path) = match &statement {
            ast::Stmt::ModDecl(module) => {
                let children = module
                    .items()
                    .map(|items| items.collect::<Vec<_>>())
                    .unwrap_or_default();
                let mut child_path = module_path.to_vec();
                if let Some(name) = module.name() {
                    child_path.push(name.text().to_string());
                }
                (children, child_path)
            }
            _ => (Vec::new(), module_path.to_vec()),
        };
        output.push(ScopedStatement {
            statement,
            macros: macros.clone(),
        });
        if !children.is_empty() {
            collect_scoped_statements(
                children,
                &MacroScope::default(),
                provider,
                reexports,
                &child_path,
                output,
                erased_imports,
                diagnostics,
            );
        }
    }
}

pub(super) fn parse_proc_macro_use(
    use_decl: &ast::UseDecl,
    provider: &dyn ProcMacroProvider,
) -> ProcMacroUse {
    parse_proc_macro_use_resolved(use_decl, provider, &MacroReexports::default(), &[])
}

pub(super) fn parse_proc_macro_use_resolved(
    use_decl: &ast::UseDecl,
    provider: &dyn ProcMacroProvider,
    reexports: &MacroReexports,
    module_path: &[String],
) -> ProcMacroUse {
    let Some(tree) = use_decl.use_tree() else {
        return ProcMacroUse::Ordinary;
    };
    let mut bindings = Vec::new();
    flatten_use_tree(&tree, &[], &mut bindings);

    let mut imports = Vec::new();
    let mut ordinary = Vec::new();
    let mut errors = Vec::new();
    for binding in bindings {
        let Some(package) = binding.path.first() else {
            ordinary.push(binding);
            continue;
        };

        if let Some(exports) = provider.exports(package) {
            if binding.glob {
                if binding.path.len() != 1 {
                    errors.push(format!(
                        "process macros are exported from the root of proc-macro package `{package}`"
                    ));
                    continue;
                }
                imports.extend(exports.into_iter().map(|export| {
                    (
                        export.name.clone(),
                        ImportedMacro {
                            package: package.clone(),
                            definition: provider.definition(package, &export.name),
                            macro_name: export.name,
                            kind: export.kind,
                            helper_attributes: export.helper_attributes,
                            binding: None,
                        },
                    )
                }));
                continue;
            }
            if binding.path.len() != 2 {
                errors.push(format!(
                    "process macros are exported from the root of proc-macro package `{package}`"
                ));
                continue;
            }
            let macro_name = &binding.path[1];
            let Some(export) = exports.iter().find(|export| export.name == *macro_name) else {
                errors.push(format!(
                    "proc-macro package `{package}` does not export macro `{macro_name}`"
                ));
                continue;
            };
            imports.push((
                binding.alias.clone().unwrap_or_else(|| macro_name.clone()),
                ImportedMacro {
                    package: package.clone(),
                    macro_name: macro_name.clone(),
                    kind: export.kind,
                    helper_attributes: export.helper_attributes.clone(),
                    binding: binding.local_range.clone(),
                    definition: provider.definition(package, macro_name),
                },
            ));
            continue;
        }

        if binding.glob {
            if let Some(exported) = find_reexport_scope(reexports, module_path, &binding.path) {
                imports.extend(exported.iter().map(|(name, imported)| {
                    let mut imported = imported.clone();
                    imported.binding = None;
                    (name.clone(), imported)
                }));
                ordinary.push(binding);
            } else {
                ordinary.push(binding);
            }
            continue;
        }

        let Some((name, export_path)) = binding.path.split_last() else {
            ordinary.push(binding);
            continue;
        };
        let Some(imported) = find_reexport_scope(reexports, module_path, export_path)
            .and_then(|exports| exports.get(name))
            .cloned()
        else {
            ordinary.push(binding);
            continue;
        };
        let mut imported = imported;
        imported.binding = binding.local_range.clone();
        imports.push((
            binding.alias.clone().unwrap_or_else(|| name.clone()),
            imported,
        ));
    }

    if imports.is_empty() && errors.is_empty() {
        return ProcMacroUse::Ordinary;
    }
    let replacement = render_use_bindings(&ordinary, use_decl.is_pub());
    if !errors.is_empty() {
        return ProcMacroUse::Invalid {
            message: errors.join("; "),
            replacement,
        };
    }
    ProcMacroUse::Imports {
        imports,
        replacement,
    }
}

pub(super) fn find_reexport_scope<'a>(
    reexports: &'a MacroReexports,
    module_path: &[String],
    path: &[String],
) -> Option<&'a MacroScope> {
    let mut resolved = module_path.to_vec();
    let mut rest = path;
    if path.first().is_some_and(|segment| segment == "crate") {
        resolved.clear();
        rest = &path[1..];
    } else if path.first().is_some_and(|segment| segment == "self") {
        rest = &path[1..];
    } else {
        while rest.first().is_some_and(|segment| segment == "super") {
            resolved.pop();
            rest = &rest[1..];
        }
        if rest.len() == path.len()
            && let Some(exports) = reexports.get(path)
        {
            return Some(exports);
        }
    }
    resolved.extend_from_slice(rest);
    reexports.get(&resolved)
}

pub(super) fn render_use_bindings(bindings: &[UseBinding], public: bool) -> Option<String> {
    if bindings.is_empty() {
        return None;
    }
    let mut output = String::new();
    for binding in bindings {
        if public {
            output.push_str("pub ");
        }
        output.push_str("use ");
        output.push_str(&binding.path.join("::"));
        if binding.glob {
            if !binding.path.is_empty() {
                output.push_str("::");
            }
            output.push('*');
        }
        if let Some(alias) = &binding.alias {
            output.push_str(" as ");
            output.push_str(alias);
        }
        output.push_str(";\n");
    }
    Some(output)
}

pub(super) fn flatten_use_tree(
    tree: &ast::UseTree,
    prefix: &[String],
    output: &mut Vec<UseBinding>,
) {
    let mut path = prefix.to_vec();
    if let Some(tree_path) = tree.path() {
        path.extend(
            tree_path
                .segments()
                .filter_map(|segment| segment.name_token().map(|token| token.text().to_string())),
        );
    }
    if let Some(list) = tree.subtree_list() {
        for child in list.trees() {
            flatten_use_tree(&child, &path, output);
        }
        return;
    }
    let alias = tree.alias();
    let local_range = alias
        .as_ref()
        .map(|token| range(token.text_range()))
        .or_else(|| {
            (!tree.is_glob())
                .then(|| tree.path()?.segments().last()?.name_token())
                .flatten()
                .map(|token| range(token.text_range()))
        });
    output.push(UseBinding {
        path,
        alias: alias.map(|alias| alias.text().to_string()),
        glob: tree.is_glob(),
        local_range,
    });
}

pub(super) fn erase_imports(source: &str, ranges: &[Range<usize>]) -> String {
    if ranges.is_empty() {
        return source.into();
    }
    let mut bytes = source.as_bytes().to_vec();
    for range in ranges {
        for byte in &mut bytes[range.clone()] {
            if !matches!(*byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(bytes).expect("blanking imports should preserve UTF-8")
}

pub(super) fn parse_derive_paths(raw: &str) -> Result<Vec<DeriveMacroPath>, String> {
    parse_derive_invocations(raw)
        .map(|invocations| invocations.into_iter().map(|(path, _)| path).collect())
}

pub(super) fn parse_derive_invocations(
    raw: &str,
) -> Result<Vec<(DeriveMacroPath, Range<usize>)>, String> {
    let attribute = ProcMacroTokenStream::from_source(raw, 0)
        .map_err(|_| "malformed derive attribute".to_string())?;
    let [
        ProcMacroTokenTree::Punct { value: '#', .. },
        ProcMacroTokenTree::Group {
            delimiter: ProcMacroDelimiter::Bracket,
            stream: body,
            ..
        },
    ] = attribute.trees.as_slice()
    else {
        return Err("malformed derive attribute".into());
    };
    let [
        ProcMacroTokenTree::Ident { text: name, .. },
        ProcMacroTokenTree::Group {
            delimiter: ProcMacroDelimiter::Parenthesis,
            stream: arguments,
            ..
        },
    ] = body.trees.as_slice()
    else {
        return Err("malformed derive attribute".into());
    };
    if name != "derive" {
        return Err("malformed derive attribute".into());
    }

    let mut paths = Vec::new();
    let trees = &arguments.trees;
    let mut index = 0;
    while index < trees.len() {
        let Some(name) = token_ident(trees.get(index)) else {
            return Err("expected a derive macro name".into());
        };
        if token_punct(trees.get(index + 1)) == Some(':')
            && token_punct(trees.get(index + 2)) == Some(':')
            && let Some(macro_name) = token_ident(trees.get(index + 3))
        {
            paths.push((
                DeriveMacroPath::Qualified {
                    package: name.into(),
                    macro_name: macro_name.into(),
                },
                trees[index + 3].span().clone(),
            ));
            index += 4;
        } else {
            paths.push((
                DeriveMacroPath::Imported(name.into()),
                trees[index].span().clone(),
            ));
            index += 1;
        }
        if index < trees.len() {
            if token_punct(trees.get(index)) != Some(',') {
                return Err("expected comma between derive macros".into());
            }
            index += 1;
        }
    }
    if paths.is_empty() {
        return Err("derive attribute must contain at least one macro".into());
    }
    Ok(paths)
}

pub(super) fn token_ident(tree: Option<&ProcMacroTokenTree>) -> Option<&str> {
    match tree? {
        ProcMacroTokenTree::Ident { text, .. } => Some(text),
        _ => None,
    }
}

pub(super) fn token_punct(tree: Option<&ProcMacroTokenTree>) -> Option<char> {
    match tree? {
        ProcMacroTokenTree::Punct { value, .. } => Some(*value),
        _ => None,
    }
}

pub(super) fn validate_output(output: &str) -> Option<String> {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(output);
    if let Some(error) = parse.errors.first() {
        return Some(format!(
            "derive macro output is not valid Riddle: {}",
            error.message
        ));
    }
    let root = ast::Root::cast(parse.syntax()).expect("parsed output should have a root");
    if root.stmts().any(|stmt| !is_item(&stmt)) {
        return Some("derive macro output must contain only top-level items".into());
    }
    None
}

pub(super) fn is_item(stmt: &ast::Stmt) -> bool {
    matches!(
        stmt,
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
    )
}

pub(super) fn diagnostic(range: Range<usize>, message: String, severity: Severity) -> Diagnostic {
    Diagnostic {
        code: PROC_MACRO_ERROR,
        severity,
        message,
        labels: vec![SourceLabel {
            range: text_range(&range),
            message: String::new(),
            style: LabelStyle::Primary,
        }],
        help: None,
        notes: Vec::new(),
    }
}

pub(super) fn text_range(range: &Range<usize>) -> TextRange {
    TextRange::new((range.start as u32).into(), (range.end as u32).into())
}

pub(super) fn valid_span(span: &Range<usize>, source_len: usize) -> bool {
    span.start <= span.end && span.end <= source_len
}

pub(super) fn range(range: TextRange) -> Range<usize> {
    usize::from(range.start())..usize::from(range.end())
}
