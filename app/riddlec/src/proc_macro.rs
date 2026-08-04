use ast::{self, support::AstNode};
use frontend::{
    incremental::{IncrementalParser, parse_tokens},
    lexer,
    tree_builder::Parse,
};
use rowan::TextRange;
use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    fmt::Write as _,
    ops::Range,
    path::PathBuf,
};
use syntax::{SyntaxKind, SyntaxNode};
use type_checker::{Diagnostic, LabelStyle, Severity, SourceLabel};

const PROC_MACRO_ERROR: &str = "E0400";
const TOKEN_WIRE_HEADER: &str = "RMT1;";
const MAX_DERIVE_EXPANSION_DEPTH: usize = 32;
const STANDARD_MACRO_PACKAGE: &str = "std";

pub const STANDARD_FUNCTION_MACROS: [&str; 2] = ["print", "println"];
pub const STANDARD_DERIVE_MACROS: [&str; 1] = ["Debug"];

mod document;
mod expand;
mod scope;
mod standard;
mod token_stream;

pub use expand::{expand_source, expand_standard_macros};
pub use token_stream::{
    ProcMacroDelimiter, ProcMacroSpacing, ProcMacroTokenStream, ProcMacroTokenTree,
};

#[derive(Debug, Clone)]
pub struct ProcMacroDiagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Range<usize>,
}

#[derive(Debug, Clone)]
pub struct ProcMacroExpansion {
    pub output: ProcMacroTokenStream,
    pub diagnostics: Vec<ProcMacroDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcMacroKind {
    Derive,
    Attribute,
    FunctionLike,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcMacroExport {
    pub name: String,
    pub kind: ProcMacroKind,
    pub helper_attributes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcMacroDefinition {
    pub path: PathBuf,
    pub source: String,
    pub range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcMacroOccurrence {
    pub name: String,
    pub package: String,
    pub macro_name: String,
    pub kind: ProcMacroKind,
    pub range: Range<usize>,
    pub binding: Option<Range<usize>>,
    pub definition: Option<ProcMacroDefinition>,
    pub is_declaration: bool,
}

pub trait ProcMacroProvider {
    fn exports(&self, _package: &str) -> Option<Vec<ProcMacroExport>> {
        None
    }

    fn definition(&self, _package: &str, _macro_name: &str) -> Option<ProcMacroDefinition> {
        None
    }

    fn expand(
        &mut self,
        package: &str,
        macro_name: &str,
        kind: ProcMacroKind,
        input: &ProcMacroTokenStream,
        second_input: Option<&ProcMacroTokenStream>,
        call_site: Range<usize>,
    ) -> Result<ProcMacroExpansion, String>;
}

struct StandardMacroProvider;

impl ProcMacroProvider for StandardMacroProvider {
    fn expand(
        &mut self,
        _package: &str,
        _macro_name: &str,
        _kind: ProcMacroKind,
        _input: &ProcMacroTokenStream,
        _second_input: Option<&ProcMacroTokenStream>,
        _call_site: Range<usize>,
    ) -> Result<ProcMacroExpansion, String> {
        Err("external process macros require a package provider".into())
    }
}

#[derive(Debug, Clone)]
pub struct GeneratedInsertion {
    pub at: usize,
    pub text: String,
    pub call_site: Range<usize>,
    pub(crate) spans: Vec<GeneratedSpanMapping>,
}

#[derive(Debug, Clone)]
pub(crate) struct GeneratedSpanMapping {
    pub generated: Range<usize>,
    pub original: Range<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct ExpandedSource {
    pub source: String,
    pub parse: Option<Parse>,
    pub mappings: Vec<ExpandedTokenMapping>,
    pub insertions: Vec<GeneratedInsertion>,
    pub macro_occurrences: Vec<ProcMacroOccurrence>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct ExpandedTokenMapping {
    pub generated: Range<usize>,
    pub original: Range<usize>,
    pub synthetic: bool,
}
