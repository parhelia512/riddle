use std::collections::HashSet;

use frontend::{
    incremental::IncrementalParser, lexer::lex, parser::Parser, tree_builder::build_tree,
};
use syntax::{SyntaxKind, SyntaxToken};

/// Formatting options shared by the CLI and language server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatOptions {
    /// Number of spaces in one indentation level when `insert_spaces` is set.
    pub tab_size: u32,
    /// Use spaces instead of hard tabs for indentation.
    pub insert_spaces: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            tab_size: 4,
            insert_spaces: true,
        }
    }
}

/// Return lexical and parser errors for a standalone source document.
#[must_use]
pub fn parse_errors(source: &str) -> Vec<frontend::ParseError> {
    let mut parser = IncrementalParser::new();
    parser.set_source(source).errors.clone()
}

/// Format one Riddle source document without changing its tokens.
#[must_use]
pub fn format_source(source: &str, options: FormatOptions) -> String {
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let indent_unit = if options.insert_spaces {
        " ".repeat(options.tab_size.max(1) as usize)
    } else {
        "\t".into()
    };
    let significant = lex(source)
        .into_iter()
        .filter(|token| token.kind != SyntaxKind::Whitespace)
        .collect::<Vec<_>>();
    let generic_angles = generic_angle_offsets(source);
    let mut formatter = SourceFormatter::new(indent_unit, newline, generic_angles);
    for (index, token) in significant.iter().enumerate() {
        let previous = index
            .checked_sub(1)
            .and_then(|index| significant.get(index));
        let next = significant.get(index + 1);
        let line_breaks_before = previous.map_or(0, |previous| {
            source[previous.span.end..token.span.start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
        });
        let before_newline = line_breaks_before > 0;
        let blank_line_before = line_breaks_before > 1;
        let after_newline =
            next.is_some_and(|next| source[token.span.end..next.span.start].contains('\n'));
        formatter.push(
            token.kind,
            &source[token.span.clone()],
            token.span.start,
            next.map(|token| token.kind),
            (before_newline, after_newline, blank_line_before),
        );
    }
    formatter.finish()
}

struct SourceFormatter {
    output: String,
    indent: usize,
    delimiters: Vec<SyntaxKind>,
    previous: Option<SyntaxKind>,
    indent_unit: String,
    newline: &'static str,
    generic_angles: HashSet<usize>,
    generic_angle_depth: usize,
    tight_after_generic_open: bool,
    tight_after_generic_close: bool,
    attribute_bracket_depth: usize,
    newline_after_attribute: bool,
}

impl SourceFormatter {
    fn new(indent_unit: String, newline: &'static str, generic_angles: HashSet<usize>) -> Self {
        Self {
            output: String::new(),
            indent: 0,
            delimiters: Vec::new(),
            previous: None,
            indent_unit,
            newline,
            generic_angles,
            generic_angle_depth: 0,
            tight_after_generic_open: false,
            tight_after_generic_close: false,
            attribute_bracket_depth: 0,
            newline_after_attribute: false,
        }
    }

    fn push(
        &mut self,
        kind: SyntaxKind,
        text: &str,
        start: usize,
        next: Option<SyntaxKind>,
        spacing: (bool, bool, bool),
    ) {
        let (before_newline, after_newline, blank_line_before) = spacing;
        if self.newline_after_attribute {
            self.newline();
            self.newline_after_attribute = false;
        }
        if blank_line_before && !self.output.is_empty() {
            self.blank_line();
        }
        let generic_angle = self.generic_angles.contains(&start);
        if kind == SyntaxKind::Less && generic_angle {
            self.generic_angle_depth += 1;
        }
        let tight_after_generic = self.tight_after_generic_open
            || (self.tight_after_generic_close
                && matches!(
                    kind,
                    SyntaxKind::LParen | SyntaxKind::LBracket | SyntaxKind::Bang
                ));
        self.tight_after_generic_open = false;
        self.tight_after_generic_close = false;
        match kind {
            SyntaxKind::LineComment
            | SyntaxKind::DocComment
            | SyntaxKind::DocBlockComment
            | SyntaxKind::BlockComment => {
                self.push_comment(kind, text, before_newline, after_newline);
            }
            SyntaxKind::LBrace => {
                self.write_indent();
                if self.previous != Some(SyntaxKind::ColonColon) {
                    ensure_space(&mut self.output);
                }
                self.output.push('{');
                self.delimiters.push(kind);
                if next != Some(SyntaxKind::RBrace) {
                    self.indent += 1;
                    self.newline();
                }
            }
            SyntaxKind::RBrace => {
                if self.previous == Some(SyntaxKind::LBrace) {
                    self.output.push('}');
                    self.delimiters.pop();
                    if !matches!(
                        next,
                        Some(
                            SyntaxKind::Else
                                | SyntaxKind::Semi
                                | SyntaxKind::Comma
                                | SyntaxKind::RParen
                                | SyntaxKind::RBracket
                        )
                    ) {
                        self.newline();
                    }
                } else {
                    self.close_brace(next);
                }
            }
            SyntaxKind::LParen | SyntaxKind::LBracket => {
                self.write_indent();
                if !tight_after_generic && needs_space(self.previous, kind, &self.output) {
                    ensure_space(&mut self.output);
                }
                self.output.push_str(text);
                self.delimiters.push(kind);
                if kind == SyntaxKind::LBracket
                    && (self.attribute_bracket_depth > 0 || self.previous == Some(SyntaxKind::Hash))
                {
                    self.attribute_bracket_depth += 1;
                }
            }
            SyntaxKind::RParen | SyntaxKind::RBracket => {
                trim_spaces(&mut self.output);
                self.output.push_str(text);
                self.delimiters.pop();
                if kind == SyntaxKind::RBracket && self.attribute_bracket_depth > 0 {
                    self.attribute_bracket_depth -= 1;
                    if self.attribute_bracket_depth == 0 {
                        self.newline_after_attribute = true;
                    }
                }
            }
            SyntaxKind::Semi => {
                trim_spaces(&mut self.output);
                self.output.push(';');
                if self.delimiters.last() == Some(&SyntaxKind::LBracket) {
                    self.output.push(' ');
                } else {
                    self.newline();
                }
            }
            SyntaxKind::Comma => {
                trim_spaces(&mut self.output);
                self.output.push(',');
                if self.delimiters.last() == Some(&SyntaxKind::LBrace)
                    && self.generic_angle_depth == 0
                {
                    self.newline();
                } else {
                    self.output.push(' ');
                }
            }
            SyntaxKind::Colon => {
                trim_spaces(&mut self.output);
                self.output.push_str(": ");
            }
            SyntaxKind::Dot | SyntaxKind::ColonColon | SyntaxKind::Question => {
                trim_spaces(&mut self.output);
                self.output.push_str(text);
            }
            SyntaxKind::Less | SyntaxKind::Greater | SyntaxKind::Shr
                if self.generic_angles.contains(&start) =>
            {
                trim_spaces(&mut self.output);
                self.output.push_str(text);
            }
            SyntaxKind::Star if self.previous == Some(SyntaxKind::ColonColon) => {
                trim_spaces(&mut self.output);
                self.output.push_str(text);
            }
            kind if is_prefix_operator(kind, self.previous) => {
                self.write_indent();
                if !tight_after_generic && needs_space(self.previous, kind, &self.output) {
                    ensure_space(&mut self.output);
                }
                self.output.push_str(text);
            }
            kind if is_spaced_operator(kind) => {
                self.write_indent();
                ensure_space(&mut self.output);
                self.output.push_str(text);
                self.output.push(' ');
            }
            _ => {
                self.write_indent();
                if !tight_after_generic && needs_space(self.previous, kind, &self.output) {
                    ensure_space(&mut self.output);
                }
                self.output.push_str(text);
            }
        }
        if kind == SyntaxKind::Less && generic_angle {
            self.tight_after_generic_open = true;
        }
        if matches!(kind, SyntaxKind::Greater | SyntaxKind::Shr) && generic_angle {
            self.tight_after_generic_close = true;
            self.generic_angle_depth = self
                .generic_angle_depth
                .saturating_sub(if kind == SyntaxKind::Shr { 2 } else { 1 });
        }
        self.previous = Some(kind);
    }

    fn push_comment(
        &mut self,
        kind: SyntaxKind,
        text: &str,
        before_newline: bool,
        after_newline: bool,
    ) {
        if before_newline && !self.output.is_empty() && !self.ends_with_newline() {
            self.newline();
        }
        self.write_indent();
        ensure_space(&mut self.output);
        self.output.push_str(&normalize_line_endings(
            text.trim_end_matches('\r'),
            self.newline,
        ));
        if matches!(kind, SyntaxKind::LineComment | SyntaxKind::DocComment) || after_newline {
            self.newline();
        }
    }

    fn close_brace(&mut self, next: Option<SyntaxKind>) {
        self.indent = self.indent.saturating_sub(1);
        if !self.ends_with_newline() {
            self.newline();
        }
        self.write_indent();
        self.output.push('}');
        if self.delimiters.last() == Some(&SyntaxKind::LBrace) {
            self.delimiters.pop();
        }
        if !matches!(
            next,
            Some(
                SyntaxKind::Else
                    | SyntaxKind::Semi
                    | SyntaxKind::Comma
                    | SyntaxKind::RParen
                    | SyntaxKind::RBracket
            )
        ) {
            self.newline();
        }
    }

    fn write_indent(&mut self) {
        write_indent(
            &mut self.output,
            self.indent,
            &self.indent_unit,
            self.newline,
        );
    }

    fn ends_with_newline(&self) -> bool {
        self.output.ends_with(self.newline)
    }

    fn newline(&mut self) {
        trim_spaces(&mut self.output);
        if !self.ends_with_newline() {
            self.output.push_str(self.newline);
        }
    }

    fn blank_line(&mut self) {
        self.newline();
        if !self
            .output
            .ends_with(&format!("{}{}", self.newline, self.newline))
        {
            self.output.push_str(self.newline);
        }
    }

    fn finish(mut self) -> String {
        trim_spaces(&mut self.output);
        let double_newline = format!("{}{}", self.newline, self.newline);
        while self.output.ends_with(&double_newline) {
            self.output.truncate(self.output.len() - self.newline.len());
        }
        if !self.output.is_empty() && !self.ends_with_newline() {
            self.output.push_str(self.newline);
        }
        self.output
    }
}

fn normalize_line_endings(text: &str, newline: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', newline)
}

// The token stream cannot distinguish `<` used for comparison from generic
// delimiters; reuse the parser's existing tree to keep those operators spaced.
fn generic_angle_offsets(source: &str) -> HashSet<usize> {
    let tokens = lex(source);
    let (events, tokens, errors, parsed_source) = Parser::new(source, tokens).parse();
    let parse = build_tree(&events, &tokens, parsed_source, errors);
    let mut offsets = HashSet::new();
    for token in parse
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        if !matches!(
            token.kind(),
            SyntaxKind::Less | SyntaxKind::Greater | SyntaxKind::Shr
        ) {
            continue;
        }
        let generic = token.parent_ancestors().any(|node| {
            matches!(
                node.kind(),
                SyntaxKind::GenericParams
                    | SyntaxKind::TypeArgList
                    | SyntaxKind::CallableTraitArgs
                    | SyntaxKind::DynTraitType
                    | SyntaxKind::ImplTraitType
                    | SyntaxKind::WhereClause
            )
        }) || generic_header_angle(&token);
        if !generic {
            continue;
        }
        let start: usize = token.text_range().start().into();
        let end: usize = token.text_range().end().into();
        offsets.extend(start..end);
    }
    offsets
}

fn generic_header_angle(token: &SyntaxToken) -> bool {
    token.parent_ancestors().any(|node| {
        if !matches!(node.kind(), SyntaxKind::TraitDecl | SyntaxKind::ImplDecl) {
            return false;
        }
        let body_start = node
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|candidate| candidate.kind() == SyntaxKind::LBrace)
            .map(|candidate| candidate.text_range().start());
        body_start.is_none_or(|start| token.text_range().start() < start)
    })
}

fn is_prefix_operator(kind: SyntaxKind, previous: Option<SyntaxKind>) -> bool {
    matches!(
        kind,
        SyntaxKind::Amp
            | SyntaxKind::Star
            | SyntaxKind::Plus
            | SyntaxKind::Minus
            | SyntaxKind::Bang
    ) && previous.is_none_or(is_prefix_boundary)
}

const fn is_prefix_boundary(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::LParen
            | SyntaxKind::LBracket
            | SyntaxKind::LBrace
            | SyntaxKind::Comma
            | SyntaxKind::Colon
            | SyntaxKind::Semi
            | SyntaxKind::Eq
            | SyntaxKind::Arrow
            | SyntaxKind::FatArrow
            | SyntaxKind::EqEq
            | SyntaxKind::BangEq
            | SyntaxKind::Less
            | SyntaxKind::LessEq
            | SyntaxKind::Greater
            | SyntaxKind::GreaterEq
            | SyntaxKind::AmpAmp
            | SyntaxKind::PipePipe
            | SyntaxKind::Plus
            | SyntaxKind::Minus
            | SyntaxKind::Star
            | SyntaxKind::Slash
            | SyntaxKind::Percent
            | SyntaxKind::Amp
            | SyntaxKind::Pipe
            | SyntaxKind::Caret
            | SyntaxKind::PlusEq
            | SyntaxKind::MinusEq
            | SyntaxKind::StarEq
            | SyntaxKind::SlashEq
            | SyntaxKind::PercentEq
            | SyntaxKind::AmpEq
            | SyntaxKind::PipeEq
            | SyntaxKind::CaretEq
            | SyntaxKind::Shl
            | SyntaxKind::Shr
            | SyntaxKind::ShlEq
            | SyntaxKind::ShrEq
            | SyntaxKind::Return
            | SyntaxKind::Break
            | SyntaxKind::Continue
            | SyntaxKind::Else
            | SyntaxKind::If
            | SyntaxKind::While
            | SyntaxKind::Loop
            | SyntaxKind::For
            | SyntaxKind::In
            | SyntaxKind::Match
            | SyntaxKind::Let
            | SyntaxKind::Mut
            | SyntaxKind::As
    )
}

const fn is_spaced_operator(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Arrow
            | SyntaxKind::FatArrow
            | SyntaxKind::Eq
            | SyntaxKind::EqEq
            | SyntaxKind::BangEq
            | SyntaxKind::Less
            | SyntaxKind::LessEq
            | SyntaxKind::Greater
            | SyntaxKind::GreaterEq
            | SyntaxKind::Plus
            | SyntaxKind::Minus
            | SyntaxKind::Slash
            | SyntaxKind::Percent
            | SyntaxKind::Pipe
            | SyntaxKind::Caret
            | SyntaxKind::Star
            | SyntaxKind::Amp
            | SyntaxKind::AmpAmp
            | SyntaxKind::PipePipe
            | SyntaxKind::PlusEq
            | SyntaxKind::MinusEq
            | SyntaxKind::StarEq
            | SyntaxKind::SlashEq
            | SyntaxKind::PercentEq
            | SyntaxKind::AmpEq
            | SyntaxKind::PipeEq
            | SyntaxKind::CaretEq
            | SyntaxKind::Shl
            | SyntaxKind::Shr
            | SyntaxKind::ShlEq
            | SyntaxKind::ShrEq
    )
}

fn needs_space(previous: Option<SyntaxKind>, current: SyntaxKind, output: &str) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    if output.ends_with([' ', '\n', '\t']) {
        return false;
    }
    if matches!(current, SyntaxKind::LParen | SyntaxKind::LBracket) {
        return !matches!(
            previous,
            SyntaxKind::Ident
                | SyntaxKind::SelfKw
                | SyntaxKind::SuperKw
                | SyntaxKind::CrateKw
                | SyntaxKind::Fun
                | SyntaxKind::LParen
                | SyntaxKind::LBracket
                | SyntaxKind::Number
                | SyntaxKind::Float
                | SyntaxKind::String
                | SyntaxKind::Char
                | SyntaxKind::True
                | SyntaxKind::False
                | SyntaxKind::RParen
                | SyntaxKind::RBracket
                | SyntaxKind::Dot
                | SyntaxKind::ColonColon
                | SyntaxKind::Hash
                | SyntaxKind::Bang
                | SyntaxKind::Amp
                | SyntaxKind::Star
                | SyntaxKind::Plus
                | SyntaxKind::Minus
        );
    }
    if current == SyntaxKind::Bang {
        return is_prefix_boundary(previous)
            && !matches!(previous, SyntaxKind::LParen | SyntaxKind::LBracket);
    }
    !matches!(
        (previous, current),
        (
            SyntaxKind::LParen
                | SyntaxKind::LBracket
                | SyntaxKind::Dot
                | SyntaxKind::ColonColon
                | SyntaxKind::Hash
                | SyntaxKind::Bang
                | SyntaxKind::Amp
                | SyntaxKind::Star
                | SyntaxKind::Plus
                | SyntaxKind::Minus,
            _
        ) | (
            _,
            SyntaxKind::LParen | SyntaxKind::LBracket | SyntaxKind::Bang
        )
    )
}

fn write_indent(output: &mut String, indent: usize, indent_unit: &str, newline: &str) {
    if output.is_empty() || output.ends_with(newline) {
        for _ in 0..indent {
            output.push_str(indent_unit);
        }
    }
}

fn ensure_space(output: &mut String) {
    if !output.is_empty() && !output.ends_with([' ', '\n', '\t']) {
        output.push(' ');
    }
}

fn trim_spaces(output: &mut String) {
    while output.ends_with([' ', '\t']) {
        output.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_parser_errors_for_cli_validation() {
        assert!(!parse_errors("fun main({").is_empty());
        assert!(parse_errors("fun main() {}").is_empty());
    }

    #[test]
    fn formats_doc_and_block_comments_without_losing_content() {
        let source = "/// docs\nfun main(){/* keep\n * detail */let value=1;}// tail\n";
        assert_eq!(
            format_source(source, FormatOptions::default()),
            "/// docs\nfun main() {\n    /* keep\n * detail */ let value = 1;\n}\n// tail\n"
        );
    }

    #[test]
    fn keeps_crlf_sources_in_their_existing_line_ending() {
        let source = "fun main(){\r\nlet value=1;\r\n}\r\n";
        assert_eq!(
            format_source(source, FormatOptions::default()),
            "fun main() {\r\n    let value = 1;\r\n}\r\n"
        );
    }

    #[test]
    fn distinguishes_prefix_references_from_binary_operators() {
        let source = "fun main(value:i32){let a=-1;let b=value*2;let c=value&1;let r=&value;}";
        assert_eq!(
            format_source(source, FormatOptions::default()),
            "fun main(value: i32) {\n    let a = -1;\n    let b = value * 2;\n    let c = value & 1;\n    let r = &value;\n}\n"
        );
    }

    #[test]
    fn formats_generics_paths_and_expression_prefixes() {
        let source = "use std::{io::println,vector::Vector}; fun id<T>(value:Vec<T>)->Vec<T>{value} fun main(){for x in [1,2]{if !false{foo(!false);}} let f=fun(x:i32){x};}";
        assert_eq!(
            format_source(source, FormatOptions::default()),
            "use std::{\n    io::println,\n    vector::Vector\n};\nfun id<T>(value: Vec<T>) -> Vec<T> {\n    value\n}\nfun main() {\n    for x in [1, 2] {\n        if !false {\n            foo(!false);\n        }\n    }\n    let f = fun(x: i32) {\n        x\n    };\n}\n"
        );
    }

    #[test]
    fn keeps_empty_blocks_compact() {
        let source = "fun empty(){} struct Empty{} impl Empty{}";
        let formatted = format_source(source, FormatOptions::default());
        assert_eq!(
            formatted,
            "fun empty() {}\nstruct Empty {}\nimpl Empty {}\n"
        );
        assert_eq!(
            format_source(&formatted, FormatOptions::default()),
            formatted
        );
    }

    #[test]
    fn separates_impl_generics_from_the_trait_name() {
        let source = "trait X {} struct S<T> {} impl<T>X for S<T>{}";
        assert_eq!(
            format_source(source, FormatOptions::default()),
            "trait X {}\nstruct S<T> {}\nimpl<T> X for S<T> {}\n"
        );
    }

    #[test]
    fn keeps_generic_bounds_tight_in_trait_types_and_headers() {
        let source = "trait Base<T> {} trait Child: Base<i32> {} fun f<T>(x:T) where T:Base<i32> {} fun g(x:dyn Base<i32>) {}";
        assert_eq!(
            format_source(source, FormatOptions::default()),
            "trait Base<T> {}\ntrait Child: Base<i32> {}\nfun f<T>(x: T) where T: Base<i32> {}\nfun g(x: dyn Base<i32>) {}\n"
        );
    }

    #[test]
    fn keeps_glob_imports_tight_after_path_separators() {
        assert_eq!(
            format_source("pub use std::prelude::*;", FormatOptions::default()),
            "pub use std::prelude::*;\n"
        );
    }

    #[test]
    fn separates_attributes_and_preserves_blank_lines() {
        let source = "#[derive(Debug)]\nstruct Item{}\n\nimpl Item{}";
        assert_eq!(
            format_source(source, FormatOptions::default()),
            "#[derive(Debug)]\nstruct Item {}\n\nimpl Item {}\n"
        );
    }

    #[test]
    fn keeps_array_const_generics_and_dereferences_compact() {
        let source = "fun take<T,const N:usize>(value:&mut [T;N])->(T,N){&mut *value}";
        assert_eq!(
            format_source(source, FormatOptions::default()),
            "fun take<T, const N: usize>(value: &mut [T; N]) -> (T, N) {\n    &mut *value\n}\n"
        );
    }

    #[test]
    fn keeps_nested_parentheses_and_unit_values_compact() {
        assert_eq!(
            format_source("fun f() { return Ok(()); }", FormatOptions::default()),
            "fun f() {\n    return Ok(());\n}\n"
        );
    }
}
