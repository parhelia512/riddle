use ast::{Root, Stmt, UseTree, attrs_for_node, support::AstNode};
use lsp_types::TextEdit;
use rowan::{TextRange, TextSize};

use crate::text::LineIndex;

pub fn import_edit(source: &str, path: &str) -> Option<TextEdit> {
    let root = parse_root(source)?;
    let target = path.split("::").collect::<Vec<_>>();
    if root.stmts().any(|stmt| {
        let Stmt::UseDecl(use_decl) = stmt else {
            return false;
        };
        use_decl
            .use_tree()
            .is_some_and(|tree| use_tree_contains(&tree, &[], &target))
    }) {
        return None;
    }

    let stmts = root.stmts().collect::<Vec<_>>();
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let (offset, new_text) = match stmts.first() {
        Some(Stmt::UseDecl(_)) => {
            let end = stmts
                .iter()
                .take_while(|stmt| matches!(stmt, Stmt::UseDecl(_)))
                .last()
                .map(|stmt| stmt.syntax().text_range().end())?;
            (end, format!("{newline}use {path};"))
        }
        Some(first) => {
            let start = attrs_for_node(first.syntax())
                .first()
                .and_then(|attribute| {
                    attribute
                        .syntax()
                        .descendants_with_tokens()
                        .filter_map(rowan::NodeOrToken::into_token)
                        .find(|token| !token.kind().is_trivia())
                        .map(|token| token.text_range().start())
                })
                .unwrap_or_else(|| first.syntax().text_range().start());
            let start = usize::from(start);
            let line_start = source[..start].rfind('\n').map_or(0, |line| line + 1);
            let offset = if source[line_start..start].trim().is_empty() {
                line_start
            } else {
                start
            };
            (text_size(offset), format!("use {path};{newline}"))
        }
        None => (text_size(source.len()), format!("use {path};{newline}")),
    };
    Some(TextEdit {
        range: LineIndex::new(source).range(source, TextRange::empty(offset))?,
        new_text,
    })
}

pub(crate) fn parse_root(source: &str) -> Option<Root> {
    let tokens = frontend::lexer::lex(source);
    let (events, tokens, errors, parsed_source) =
        frontend::parser::Parser::new(source, tokens).parse();
    let parse = frontend::tree_builder::build_tree(&events, &tokens, parsed_source, errors);
    Root::cast(parse.syntax())
}

fn use_tree_contains(tree: &UseTree, prefix: &[String], target: &[&str]) -> bool {
    let mut path = prefix.to_vec();
    if let Some(tree_path) = tree.path() {
        path.extend(
            tree_path
                .segments()
                .filter_map(|segment| segment.name_token().map(|token| token.text().to_string())),
        );
    }
    if tree.is_glob() {
        return target.starts_with(&path.iter().map(String::as_str).collect::<Vec<_>>());
    }
    if let Some(list) = tree.subtree_list() {
        return list
            .trees()
            .any(|child| use_tree_contains(&child, &path, target));
    }
    tree.alias().is_none() && path.iter().map(String::as_str).eq(target.iter().copied())
}

fn text_size(offset: usize) -> TextSize {
    TextSize::from(u32::try_from(offset).expect("document offset must fit in u32"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_edit_preserves_crlf_and_does_not_split_attributes() {
        let source = "// header\r\n#[test]\r\nfun main() {}\r\n";
        let edit = import_edit(source, "util::Helper").unwrap();
        assert_eq!(edit.range.start, lsp_types::Position::new(1, 0));
        assert_eq!(edit.new_text, "use util::Helper;\r\n");
    }

    #[test]
    fn import_edit_detects_grouped_and_glob_imports() {
        assert!(import_edit("use util::{Helper, Other};\n", "util::Helper").is_none());
        assert!(import_edit("use util::*;\n", "util::Helper").is_none());
    }
}
