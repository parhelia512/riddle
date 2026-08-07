use anyhow::{Context, bail};
use ast::{self, support::AstNode};
use frontend::incremental::IncrementalParser;
use libloading::Library;
use riddlec::proc_macro::{
    ProcMacroDefinition, ProcMacroDiagnostic, ProcMacroExpansion, ProcMacroExport, ProcMacroKind,
    ProcMacroProvider, ProcMacroTokenStream, ProcMacroTokenTree,
};
use std::collections::{HashMap, HashSet};
use std::ffi::{CString, c_char};
use std::fmt::Write as _;
use std::mem::size_of;
use std::path::Path;
use std::slice;
use std::sync::Mutex;

use crate::project::ProcMacroPackage;

const MAX_PROC_MACRO_BYTES: usize = 16 * 1024 * 1024;
const PROC_MACRO_API: &str = include_str!("../../../std/std/proc_macro.rid");
const PROC_MACRO_ENTRY: &[u8] = b"riddle_proc_expand\0";
const PROC_MACRO_ENTRY_X86: &[u8] = b"_riddle_proc_expand\0";
// ponytail: process macro runtimes have mutable globals; use per-library locks if parallel
// expansion becomes necessary.
static PROC_MACRO_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone)]
pub struct HostMacroExport {
    pub macro_name: String,
    pub function_name: String,
    pub function_name_range: std::ops::Range<usize>,
    pub wrapper_name: String,
    pub kind: ProcMacroKind,
    pub helper_attributes: Vec<String>,
}

pub fn discover_exports(source: &str) -> anyhow::Result<Vec<HostMacroExport>> {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(source);
    if let Some(error) = parse.errors.first() {
        bail!("cannot parse proc-macro package: {}", error.message);
    }

    let root = ast::Root::cast(parse.syntax()).context("proc-macro source has no root")?;
    let mut exports = Vec::new();
    let mut macro_names = HashSet::new();
    for stmt in root.stmts() {
        let ast::Stmt::FuncDecl(function) = stmt else {
            continue;
        };
        let Some(function_name) = function.name().map(|name| name.text().to_string()) else {
            continue;
        };
        let function_name_range = function
            .name()
            .map(|name| {
                usize::from(name.text_range().start())..usize::from(name.text_range().end())
            })
            .unwrap();
        let declarations = ast::attrs_for_node(function.syntax())
            .into_iter()
            .filter_map(|attr| parse_export_attribute(&attr.raw_text()))
            .collect::<anyhow::Result<Vec<_>>>()?;
        if declarations.len() > 1 {
            bail!("proc-macro function `{function_name}` has more than one export attribute");
        }
        for declaration in declarations {
            validate_export_signature(&function, &function_name, declaration.kind)?;
            let macro_name = declaration.name.unwrap_or_else(|| function_name.clone());
            if !macro_names.insert(macro_name.clone()) {
                bail!("proc macro `{macro_name}` is exported more than once");
            }
            exports.push(HostMacroExport {
                wrapper_name: wrapper_name(&macro_name),
                macro_name,
                function_name: function_name.clone(),
                function_name_range: function_name_range.clone(),
                kind: declaration.kind,
                helper_attributes: declaration.helper_attributes,
            });
        }
    }
    if exports.is_empty() {
        bail!("proc-macro package exports no process macro function");
    }
    Ok(exports)
}

pub fn host_source(source: &str, exports: &[HostMacroExport]) -> String {
    let suffix = host_suffix(exports);
    let mut output = String::with_capacity(PROC_MACRO_API.len() + source.len() + suffix.len() + 1);
    output.push_str(host_prefix());
    output.push('\n');
    output.push_str(source);
    output.push_str(&suffix);
    output
}

pub const fn host_prefix() -> &'static str {
    PROC_MACRO_API
}

pub fn host_suffix(exports: &[HostMacroExport]) -> String {
    let mut output = String::with_capacity(exports.len() * 180 + 120);
    for export in exports {
        output.push_str("\n#[c_export]\nfun ");
        output.push_str(&export.wrapper_name);
        output.push_str("(input: &str, second_input: &str) {\n");
        output.push_str("    let output = ");
        output.push_str(&export.function_name);
        output.push_str("(riddle_proc_decode_token_stream(input)");
        if export.kind == ProcMacroKind::Attribute {
            output.push_str(", riddle_proc_decode_token_stream(second_input)");
        }
        output.push_str(");\n");
        output.push_str("    emit_output(&output);\n}\n");
    }
    output
}

const HOST_RUNTIME_PREAMBLE: &str = r#"#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>

#if defined(_WIN32)
#define RIDDLE_PROC_EXPORT __declspec(dllexport)
#else
#define RIDDLE_PROC_EXPORT __attribute__((visibility("default")))
#endif

void rgc_init(void *stack_bottom);
void riddle_proc_begin(size_t call_site_start, size_t call_site_end);
void riddle_proc_emit_diagnostic(
    uint8_t level,
    size_t start,
    size_t end,
    const uint8_t *message,
    size_t len
);
const char *riddle_proc_output_value(void);
size_t riddle_proc_output_length(void);
size_t riddle_proc_diagnostic_count(void);
const void *riddle_proc_diagnostics_value(void);

int riddle_proc_putchar(int value) {
    unsigned char byte = (unsigned char)value;
    return fwrite(&byte, 1u, 1u, stderr) == 1u ? (int)byte : EOF;
}

typedef struct {
    uint8_t level;
    size_t start;
    size_t end;
    const char *message;
    size_t message_len;
} RiddleProcDiagnostic;

typedef struct {
    const char *output;
    size_t output_len;
    const RiddleProcDiagnostic *diagnostics;
    size_t diagnostic_count;
} RiddleProcResult;

"#;
const HOST_RUNTIME_DISPATCH_PREFIX: &str = r"

static void dispatch_macro(
    const char *name,
    const char *input,
    const char *second_input,
    size_t call_site_start,
    size_t call_site_end
) {
";
const HOST_RUNTIME_ENTRY: &str = r#"    else {
        static const char message[] = "unknown process macro";
        riddle_proc_emit_diagnostic(
            0u,
            call_site_start,
            call_site_end,
            (const uint8_t *)message,
            sizeof(message) - 1u
        );
    }
}

RIDDLE_PROC_EXPORT int riddle_proc_expand(
    const char *name,
    const char *input,
    const char *second_input,
    size_t call_site_start,
    size_t call_site_end,
    RiddleProcResult *result
) {
    if (!name || !input || !second_input || !result || call_site_start > call_site_end) {
        return 1;
    }

    int stack_anchor = 0;
    rgc_init(&stack_anchor);
    riddle_proc_begin(call_site_start, call_site_end);
    dispatch_macro(name, input, second_input, call_site_start, call_site_end);
    result->output = riddle_proc_output_value();
    result->output_len = riddle_proc_output_length();
    result->diagnostics = (const RiddleProcDiagnostic *)riddle_proc_diagnostics_value();
    result->diagnostic_count = riddle_proc_diagnostic_count();
    return 0;
}
"#;

pub fn host_runtime_c(exports: &[HostMacroExport]) -> String {
    let mut output = String::from(HOST_RUNTIME_PREAMBLE);
    append_host_declarations(&mut output, exports);
    output.push_str(HOST_RUNTIME_DISPATCH_PREFIX);
    append_host_dispatch(&mut output, exports);
    output.push_str(HOST_RUNTIME_ENTRY);
    output
}

fn append_host_declarations(output: &mut String, exports: &[HostMacroExport]) {
    for export in exports {
        output.push_str("void ");
        output.push_str(&export.wrapper_name);
        output.push_str("(const char *input, const char *second_input);\n");
    }
}

fn append_host_dispatch(output: &mut String, exports: &[HostMacroExport]) {
    for (index, export) in exports.iter().enumerate() {
        if index == 0 {
            output.push_str("    if ");
        } else {
            output.push_str("    else if ");
        }
        output.push_str("(strcmp(name, \"");
        output.push_str(&c_escape(&export.macro_name));
        output.push_str("\") == 0) { ");
        output.push_str(&export.wrapper_name);
        output.push_str("(input, second_input); }\n");
    }
}

struct ExportAttribute {
    kind: ProcMacroKind,
    name: Option<String>,
    helper_attributes: Vec<String>,
}

fn parse_export_attribute(raw: &str) -> Option<anyhow::Result<ExportAttribute>> {
    match parse_export_attribute_inner(raw) {
        Ok(Some(attribute)) => Some(Ok(attribute)),
        Ok(None) => None,
        Err(error) => Some(Err(error)),
    }
}

fn parse_export_attribute_inner(raw: &str) -> anyhow::Result<Option<ExportAttribute>> {
    let tokens = ProcMacroTokenStream::from_source(raw, 0)
        .map_err(anyhow::Error::msg)
        .context("invalid process macro export attribute")?;
    let [
        ProcMacroTokenTree::Punct { value: '#', .. },
        ProcMacroTokenTree::Group {
            delimiter: riddlec::proc_macro::ProcMacroDelimiter::Bracket,
            stream,
            ..
        },
    ] = tokens.trees.as_slice()
    else {
        return Ok(None);
    };
    let Some(ProcMacroTokenTree::Ident {
        text: attribute, ..
    }) = stream.trees.first()
    else {
        return Ok(None);
    };
    match attribute.as_str() {
        "proc_macro" | "proc_macro_attribute" => {
            if stream.trees.len() != 1 {
                bail!("`#[{attribute}]` does not accept arguments");
            }
            Ok(Some(ExportAttribute {
                kind: if attribute == "proc_macro" {
                    ProcMacroKind::FunctionLike
                } else {
                    ProcMacroKind::Attribute
                },
                name: None,
                helper_attributes: Vec::new(),
            }))
        }
        "proc_macro_derive" => parse_derive_export(stream).map(Some),
        _ => Ok(None),
    }
}

fn parse_derive_export(stream: &ProcMacroTokenStream) -> anyhow::Result<ExportAttribute> {
    let [
        ProcMacroTokenTree::Ident { .. },
        ProcMacroTokenTree::Group {
            delimiter: riddlec::proc_macro::ProcMacroDelimiter::Parenthesis,
            stream: args,
            ..
        },
    ] = stream.trees.as_slice()
    else {
        bail!("proc_macro_derive expects (Name, attributes(...))");
    };
    let Some(ProcMacroTokenTree::Ident { text: name, .. }) = args.trees.first() else {
        bail!("proc_macro_derive expects a macro name");
    };

    let mut index = 1usize;
    let mut helper_attributes = Vec::new();
    if index < args.trees.len() {
        expect_punct(&args.trees, &mut index, ',')?;
        if index < args.trees.len() {
            expect_ident(&args.trees, &mut index, "attributes")?;
            let Some(ProcMacroTokenTree::Group {
                delimiter: riddlec::proc_macro::ProcMacroDelimiter::Parenthesis,
                stream: helpers,
                ..
            }) = args.trees.get(index)
            else {
                bail!("attributes expects a parenthesized identifier list");
            };
            index += 1;
            helper_attributes = parse_ident_list(&helpers.trees)?;
            if index < args.trees.len() {
                expect_punct(&args.trees, &mut index, ',')?;
            }
        }
    }
    if index != args.trees.len() {
        bail!("unexpected tokens in proc_macro_derive");
    }
    Ok(ExportAttribute {
        kind: ProcMacroKind::Derive,
        name: Some(name.clone()),
        helper_attributes,
    })
}

fn parse_ident_list(trees: &[ProcMacroTokenTree]) -> anyhow::Result<Vec<String>> {
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    let mut index = 0usize;
    while index < trees.len() {
        let name = match trees.get(index) {
            Some(ProcMacroTokenTree::Ident { text, .. }) => text.clone(),
            _ => bail!("expected an identifier in attributes(...)"),
        };
        if !seen.insert(name.clone()) {
            bail!("derive helper attribute `{name}` is declared more than once");
        }
        output.push(name);
        index += 1;
        if index < trees.len() {
            expect_punct(trees, &mut index, ',')?;
        }
    }
    Ok(output)
}

fn expect_ident(
    trees: &[ProcMacroTokenTree],
    index: &mut usize,
    expected: &str,
) -> anyhow::Result<()> {
    match trees.get(*index) {
        Some(ProcMacroTokenTree::Ident { text, .. }) if text == expected => {
            *index += 1;
            Ok(())
        }
        _ => bail!("expected `{expected}` in process macro attribute"),
    }
}

fn expect_punct(
    trees: &[ProcMacroTokenTree],
    index: &mut usize,
    expected: char,
) -> anyhow::Result<()> {
    match trees.get(*index) {
        Some(ProcMacroTokenTree::Punct { value, .. }) if *value == expected => {
            *index += 1;
            Ok(())
        }
        _ => bail!("expected `{expected}` in process macro attribute"),
    }
}

fn validate_export_signature(
    function: &ast::FuncDecl,
    name: &str,
    kind: ProcMacroKind,
) -> anyhow::Result<()> {
    if !function.is_pub() {
        bail!("proc-macro function `{name}` must be public");
    }
    if function.generic_params().is_some() {
        bail!("proc-macro function `{name}` cannot be generic");
    }
    let params = function
        .param_list()
        .map(|params| params.params().collect::<Vec<_>>())
        .unwrap_or_default();
    let expected = if kind == ProcMacroKind::Attribute {
        2
    } else {
        1
    };
    if params.len() != expected
        || !params
            .iter()
            .all(|param| param.ty().as_ref().is_some_and(is_token_stream_type))
    {
        bail!(
            "proc-macro function `{name}` must take exactly {expected} `TokenStream` parameter(s)"
        );
    }
    if !function
        .return_type()
        .as_ref()
        .is_some_and(is_token_stream_type)
    {
        bail!("proc-macro function `{name}` must return `TokenStream`");
    }
    Ok(())
}

fn is_token_stream_type(ty: &ast::Type) -> bool {
    let text = ty
        .syntax()
        .text()
        .to_string()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    text == "TokenStream" || text.ends_with("::TokenStream")
}

fn wrapper_name(macro_name: &str) -> String {
    let mut encoded = String::with_capacity(macro_name.len() * 2);
    for byte in macro_name.bytes() {
        write!(encoded, "{byte:02x}").expect("writing to a String should not fail");
    }
    format!("__riddle_proc_macro_{encoded}")
}

fn c_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[repr(C)]
struct ProcMacroFfiDiagnostic {
    level: u8,
    start: usize,
    end: usize,
    message: *const u8,
    message_len: usize,
}

#[repr(C)]
struct ProcMacroFfiResult {
    output: *const u8,
    output_len: usize,
    diagnostics: *const ProcMacroFfiDiagnostic,
    diagnostic_count: usize,
}

type ProcMacroExpandFn = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const c_char,
    usize,
    usize,
    *mut ProcMacroFfiResult,
) -> i32;

pub struct ProcMacroLibrary {
    library: Library,
}

impl ProcMacroLibrary {
    pub(crate) fn load(path: &Path) -> anyhow::Result<Self> {
        // SAFETY: this loads a compiler-generated library whose only initializer is the C runtime.
        let library = unsafe { Library::new(path) }
            .with_context(|| format!("failed to load proc-macro library `{}`", path.display()))?;
        Ok(Self { library })
    }

    pub(crate) fn expand(
        &mut self,
        macro_name: &str,
        input: &ProcMacroTokenStream,
        second_input: Option<&ProcMacroTokenStream>,
        call_site: std::ops::Range<usize>,
    ) -> anyhow::Result<ProcMacroExpansion> {
        let input = input.encode();
        let second_input = second_input.cloned().unwrap_or_default().encode();
        let payload_len = macro_name
            .len()
            .checked_add(input.len())
            .and_then(|len| len.checked_add(second_input.len()))
            .context("proc-macro request is too large")?;
        if payload_len > MAX_PROC_MACRO_BYTES {
            bail!("proc-macro request exceeds the size limit");
        }
        let macro_name = CString::new(macro_name).context("proc-macro name contains NUL")?;
        let input = CString::new(input).context("proc-macro input contains NUL")?;
        let second_input =
            CString::new(second_input).context("second proc-macro input contains NUL")?;
        let mut result = ProcMacroFfiResult {
            output: std::ptr::null(),
            output_len: 0,
            diagnostics: std::ptr::null(),
            diagnostic_count: 0,
        };
        let _guard = PROC_MACRO_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("proc-macro library lock is poisoned"))?;
        // SAFETY: both accepted names denote the generated C entry with this exact signature.
        let expand = unsafe {
            self.library
                .get::<ProcMacroExpandFn>(PROC_MACRO_ENTRY)
                .or_else(|_| self.library.get::<ProcMacroExpandFn>(PROC_MACRO_ENTRY_X86))
        }
        .context("proc-macro library does not export `riddle_proc_expand`")?;
        // SAFETY: CString pointers live through the call and result points to writable storage.
        let status = unsafe {
            expand(
                macro_name.as_ptr(),
                input.as_ptr(),
                second_input.as_ptr(),
                call_site.start,
                call_site.end,
                &raw mut result,
            )
        };
        if status != 0 {
            bail!("proc-macro library rejected the expansion request");
        }

        let encoded_output = copy_ffi_text(result.output, result.output_len, "output")?;
        let output = if encoded_output.is_empty() {
            ProcMacroTokenStream::default()
        } else {
            ProcMacroTokenStream::decode(&encoded_output)
                .map_err(anyhow::Error::msg)
                .context("invalid structured proc-macro output")?
        };
        if result.diagnostic_count > MAX_PROC_MACRO_BYTES / size_of::<ProcMacroFfiDiagnostic>() {
            bail!("proc-macro library returned too many diagnostics");
        }
        let ffi_diagnostics = if result.diagnostic_count == 0 {
            &[][..]
        } else {
            if result.diagnostics.is_null() {
                bail!("proc-macro library returned a null diagnostic array");
            }
            // SAFETY: the library owns this array until the next locked expansion call.
            unsafe { slice::from_raw_parts(result.diagnostics, result.diagnostic_count) }
        };
        let mut diagnostics = Vec::with_capacity(ffi_diagnostics.len());
        let mut result_bytes = result.output_len;
        for diagnostic in ffi_diagnostics {
            let start = diagnostic.start;
            let end = diagnostic.end;
            if start > end {
                bail!("invalid proc-macro diagnostic span");
            }
            result_bytes = result_bytes
                .checked_add(size_of::<ProcMacroFfiDiagnostic>())
                .and_then(|size| size.checked_add(diagnostic.message_len))
                .context("proc-macro result is too large")?;
            if result_bytes > MAX_PROC_MACRO_BYTES {
                bail!("proc-macro result exceeds the size limit");
            }
            let message = copy_ffi_text(diagnostic.message, diagnostic.message_len, "diagnostic")?;
            diagnostics.push(ProcMacroDiagnostic {
                severity: match diagnostic.level {
                    1 => type_checker::Severity::Warning,
                    2 => type_checker::Severity::Note,
                    3 => type_checker::Severity::Help,
                    _ => type_checker::Severity::Error,
                },
                message,
                span: start..end,
            });
        }
        Ok(ProcMacroExpansion {
            output,
            diagnostics,
        })
    }
}

fn copy_ffi_text(pointer: *const u8, len: usize, kind: &str) -> anyhow::Result<String> {
    if len > MAX_PROC_MACRO_BYTES {
        bail!("proc-macro {kind} exceeds the size limit");
    }
    if pointer.is_null() {
        if len == 0 {
            return Ok(String::new());
        }
        bail!("proc-macro library returned a null {kind} pointer");
    }
    // SAFETY: callers hold PROC_MACRO_LOCK and the library keeps result buffers alive until
    // the next expansion.
    let bytes = unsafe { slice::from_raw_parts(pointer, len) };
    String::from_utf8(bytes.to_vec()).with_context(|| format!("proc-macro {kind} is not UTF-8"))
}

pub struct ClueProcMacroProvider {
    libraries: HashMap<String, ProcMacroLibrary>,
    exports: HashMap<String, Vec<ProcMacroExport>>,
    definitions: HashMap<(String, String), ProcMacroDefinition>,
}

impl ClueProcMacroProvider {
    pub(crate) fn build(packages: &[ProcMacroPackage]) -> anyhow::Result<Self> {
        let mut provider = Self {
            libraries: HashMap::new(),
            exports: HashMap::new(),
            definitions: HashMap::new(),
        };
        for package in packages {
            let exports = discover_exports(&package.source.source)?;
            let expansion =
                riddlec::proc_macro::expand_source(&package.source.source, &mut provider);
            let errors = expansion
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == type_checker::Severity::Error)
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>();
            if !errors.is_empty() {
                bail!(
                    "failed to expand proc-macro package `{}`: {}",
                    package.name,
                    errors.join("; ")
                );
            }
            let library =
                crate::build::build_proc_macro_library(package, &exports, &expansion.source)?;
            for export in &exports {
                let range = rowan::TextRange::new(
                    u32::try_from(export.function_name_range.start)
                        .context("proc-macro definition start exceeds the source range")?
                        .into(),
                    u32::try_from(export.function_name_range.end)
                        .context("proc-macro definition end exceeds the source range")?
                        .into(),
                );
                let mapped = package
                    .source
                    .source_map
                    .map_range(range)
                    .with_context(|| {
                        format!(
                            "cannot map proc-macro definition `{}` to its source file",
                            export.macro_name
                        )
                    })?;
                provider.definitions.insert(
                    (package.alias.clone(), export.macro_name.clone()),
                    ProcMacroDefinition {
                        path: mapped.path.to_owned(),
                        source: mapped.source.into(),
                        range: usize::from(mapped.range.start())..usize::from(mapped.range.end()),
                    },
                );
            }
            provider.exports.insert(
                package.alias.clone(),
                exports
                    .iter()
                    .map(|export| ProcMacroExport {
                        name: export.macro_name.clone(),
                        kind: export.kind,
                        helper_attributes: export.helper_attributes.clone(),
                    })
                    .collect(),
            );
            provider
                .libraries
                .insert(package.alias.clone(), ProcMacroLibrary::load(&library)?);
        }
        Ok(provider)
    }
}

impl ProcMacroProvider for ClueProcMacroProvider {
    fn exports(&self, package: &str) -> Option<Vec<ProcMacroExport>> {
        self.exports.get(package).cloned()
    }

    fn definition(&self, package: &str, macro_name: &str) -> Option<ProcMacroDefinition> {
        self.definitions
            .get(&(package.into(), macro_name.into()))
            .cloned()
    }

    fn expand(
        &mut self,
        package: &str,
        macro_name: &str,
        kind: ProcMacroKind,
        input: &ProcMacroTokenStream,
        second_input: Option<&ProcMacroTokenStream>,
        call_site: std::ops::Range<usize>,
    ) -> Result<ProcMacroExpansion, String> {
        let Some(export) = self
            .exports
            .get(package)
            .and_then(|exports| exports.iter().find(|export| export.name == macro_name))
        else {
            return Err(format!("unknown process macro `{package}::{macro_name}`"));
        };
        if export.kind != kind {
            return Err(format!(
                "`{package}::{macro_name}` is a {:?} macro, not a {:?} macro",
                export.kind, kind
            ));
        }
        let library = self
            .libraries
            .get_mut(package)
            .ok_or_else(|| format!("unknown proc-macro package `{package}`"))?;
        library
            .expand(macro_name, input, second_input, call_site)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_proc_macro_exports() {
        let valid = r"
            #[proc_macro_derive(Answer, attributes(answer))]
            pub fun derive_answer(input: TokenStream) -> TokenStream { input }
        ";
        let exports = discover_exports(valid).unwrap();
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0].macro_name, "Answer");

        let private =
            "#[proc_macro_derive(Answer)] fun derive(input: TokenStream) -> TokenStream { input }";
        assert!(
            discover_exports(private)
                .unwrap_err()
                .to_string()
                .contains("must be public")
        );

        let duplicate = r"
            #[proc_macro_derive(Answer)]
            pub fun first(input: TokenStream) -> TokenStream { input }
            #[proc_macro_derive(Answer)]
            pub fun second(input: TokenStream) -> TokenStream { input }
        ";
        assert!(
            discover_exports(duplicate)
                .unwrap_err()
                .to_string()
                .contains("exported more than once")
        );
    }

    #[test]
    fn proc_macro_host_exports_a_cdylib_entry() {
        let runtime = host_runtime_c(&[]);
        assert!(runtime.contains("RIDDLE_PROC_EXPORT int riddle_proc_expand("));
        assert!(runtime.contains("rgc_init(&stack_anchor)"));
        assert!(!runtime.contains("_setmode"));
        assert!(!runtime.contains("fread"));
        assert!(!runtime.contains("riddle_proc_run"));
        assert!(!host_suffix(&[]).contains("fun main"));
    }
}
