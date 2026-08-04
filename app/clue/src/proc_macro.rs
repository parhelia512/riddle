use anyhow::{Context, bail};
use ast::{self, support::AstNode};
use frontend::incremental::IncrementalParser;
use riddlec::proc_macro::{
    ProcMacroDefinition, ProcMacroDiagnostic, ProcMacroExpansion, ProcMacroExport, ProcMacroKind,
    ProcMacroProvider, ProcMacroTokenStream, ProcMacroTokenTree,
};
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;

use crate::project::ProcMacroPackage;

const PROTOCOL_VERSION: u32 = 3;
const MAX_FRAME: usize = 16 * 1024 * 1024;
const PROC_MACRO_API: &str = include_str!("../../../std/std/proc_macro.rid");

#[derive(Debug, Clone)]
pub(crate) struct HostMacroExport {
    pub macro_name: String,
    pub function_name: String,
    pub function_name_range: std::ops::Range<usize>,
    pub wrapper_name: String,
    pub kind: ProcMacroKind,
    pub helper_attributes: Vec<String>,
}

pub(crate) fn discover_exports(source: &str) -> anyhow::Result<Vec<HostMacroExport>> {
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

pub(crate) fn host_source(source: &str, exports: &[HostMacroExport]) -> String {
    let suffix = host_suffix(exports);
    let mut output = String::with_capacity(PROC_MACRO_API.len() + source.len() + suffix.len() + 1);
    output.push_str(host_prefix());
    output.push('\n');
    output.push_str(source);
    output.push_str(&suffix);
    output
}

pub(crate) fn host_prefix() -> &'static str {
    PROC_MACRO_API
}

pub(crate) fn host_suffix(exports: &[HostMacroExport]) -> String {
    let mut output = String::with_capacity(exports.len() * 180 + 120);
    output.push_str("\nunsafe extern \"C\" { safe fun riddle_proc_run() -> i32; }\n");
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
    output.push_str("\nfun main() { riddle_proc_run(); }\n");
    output
}

pub(crate) fn host_runtime_c(exports: &[HostMacroExport]) -> String {
    let mut output = String::from(
        r#"#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef _WIN32
#include <fcntl.h>
#include <io.h>
#endif

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
uint8_t riddle_proc_diagnostic_level(size_t index);
size_t riddle_proc_diagnostic_start(size_t index);
size_t riddle_proc_diagnostic_end(size_t index);
const char *riddle_proc_diagnostic_message(size_t index);
size_t riddle_proc_diagnostic_message_length(size_t index);

#if defined(_MSC_VER)
int riddle_proc_putchar(int value) {
#else
int putchar(int value) {
#endif
    unsigned char byte = (unsigned char)value;
    return fwrite(&byte, 1u, 1u, stderr) == 1u ? (int)byte : EOF;
}

"#,
    );
    for export in exports {
        output.push_str("void ");
        output.push_str(&export.wrapper_name);
        output.push_str("(const char *input, const char *second_input);\n");
    }
    output.push_str(
        r#"
static int read_exact(void *buffer, size_t len) {
    return len == 0u || fread(buffer, 1u, len, stdin) == len;
}

static int read_u32(uint32_t *value) {
    unsigned char bytes[4];
    size_t read = fread(bytes, 1u, sizeof(bytes), stdin);
    if (read == 0u && feof(stdin)) {
        return 0;
    }
    if (read != sizeof(bytes)) {
        return -1;
    }
    *value = (uint32_t)bytes[0]
        | ((uint32_t)bytes[1] << 8u)
        | ((uint32_t)bytes[2] << 16u)
        | ((uint32_t)bytes[3] << 24u);
    return 1;
}

static int write_u32(uint32_t value) {
    unsigned char bytes[4] = {
        (unsigned char)(value & 0xffu),
        (unsigned char)((value >> 8u) & 0xffu),
        (unsigned char)((value >> 16u) & 0xffu),
        (unsigned char)((value >> 24u) & 0xffu),
    };
    return fwrite(bytes, 1u, sizeof(bytes), stdout) == sizeof(bytes);
}

static int write_bytes(const void *value, size_t len) {
    return len == 0u || fwrite(value, 1u, len, stdout) == len;
}

static void dispatch_macro(
    const char *name,
    const char *input,
    const char *second_input,
    size_t call_site_start,
    size_t call_site_end
) {
"#,
    );
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
    output.push_str(
        r#"    else {
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

int riddle_proc_run(void) {
#ifdef _WIN32
    if (_setmode(_fileno(stdin), _O_BINARY) == -1
        || _setmode(_fileno(stdout), _O_BINARY) == -1) {
        return 1;
    }
#endif
    for (;;) {
        uint32_t frame_len = 0u;
        int frame_status = read_u32(&frame_len);
        if (frame_status == 0) {
            return 0;
        }
        if (frame_status < 0 || frame_len < 24u || frame_len > 16777216u) {
            return 1;
        }

        unsigned char *frame = (unsigned char *)malloc(frame_len);
        if (!frame || !read_exact(frame, frame_len)) {
            free(frame);
            return 1;
        }
        uint32_t version = (uint32_t)frame[0]
            | ((uint32_t)frame[1] << 8u)
            | ((uint32_t)frame[2] << 16u)
            | ((uint32_t)frame[3] << 24u);
        uint32_t name_len = (uint32_t)frame[4]
            | ((uint32_t)frame[5] << 8u)
            | ((uint32_t)frame[6] << 16u)
            | ((uint32_t)frame[7] << 24u);
        uint32_t input_len = (uint32_t)frame[8]
            | ((uint32_t)frame[9] << 8u)
            | ((uint32_t)frame[10] << 16u)
            | ((uint32_t)frame[11] << 24u);
        uint32_t second_input_len = (uint32_t)frame[12]
            | ((uint32_t)frame[13] << 8u)
            | ((uint32_t)frame[14] << 16u)
            | ((uint32_t)frame[15] << 24u);
        uint32_t call_site_start = (uint32_t)frame[16]
            | ((uint32_t)frame[17] << 8u)
            | ((uint32_t)frame[18] << 16u)
            | ((uint32_t)frame[19] << 24u);
        uint32_t call_site_end = (uint32_t)frame[20]
            | ((uint32_t)frame[21] << 8u)
            | ((uint32_t)frame[22] << 16u)
            | ((uint32_t)frame[23] << 24u);
        if (version != 3u
            || call_site_start > call_site_end
            || name_len > frame_len - 24u
            || input_len > frame_len - 24u - name_len
            || second_input_len != frame_len - 24u - name_len - input_len) {
            free(frame);
            return 1;
        }

        char *name = (char *)malloc((size_t)name_len + 1u);
        char *input = (char *)malloc((size_t)input_len + 1u);
        char *second_input = (char *)malloc((size_t)second_input_len + 1u);
        if (!name || !input || !second_input) {
            free(name);
            free(input);
            free(second_input);
            free(frame);
            return 1;
        }
        memcpy(name, frame + 24u, name_len);
        name[name_len] = '\0';
        memcpy(input, frame + 24u + name_len, input_len);
        input[input_len] = '\0';
        memcpy(second_input, frame + 24u + name_len + input_len, second_input_len);
        second_input[second_input_len] = '\0';
        free(frame);

        riddle_proc_begin(call_site_start, call_site_end);
        dispatch_macro(name, input, second_input, call_site_start, call_site_end);
        free(name);
        free(input);
        free(second_input);

        size_t output_len = riddle_proc_output_length();
        size_t diagnostic_count = riddle_proc_diagnostic_count();
        uint32_t status = 0u;
        if (output_len > 16777216u - 16u) {
            return 1;
        }
        size_t response_len = 16u + output_len;
        for (size_t index = 0; index < diagnostic_count; ++index) {
            status |= riddle_proc_diagnostic_level(index) == 0u;
            size_t diagnostic_start = riddle_proc_diagnostic_start(index);
            size_t diagnostic_end = riddle_proc_diagnostic_end(index);
            size_t message_len = riddle_proc_diagnostic_message_length(index);
            if (diagnostic_start > diagnostic_end
                || diagnostic_end > UINT32_MAX
                || message_len > 16777216u - response_len
                || 16u > 16777216u - response_len - message_len) {
                return 1;
            }
            response_len += 16u + message_len;
        }
        if (response_len > 16777216u || diagnostic_count > UINT32_MAX) {
            return 1;
        }
        if (!write_u32((uint32_t)response_len)
            || !write_u32(3u)
            || !write_u32(status)
            || !write_u32((uint32_t)output_len)
            || !write_u32((uint32_t)diagnostic_count)
            || !write_bytes(riddle_proc_output_value(), output_len)) {
            return 1;
        }
        for (size_t index = 0; index < diagnostic_count; ++index) {
            const char *message = riddle_proc_diagnostic_message(index);
            size_t message_len = riddle_proc_diagnostic_message_length(index);
            if (!write_u32((uint32_t)riddle_proc_diagnostic_level(index))
                || !write_u32((uint32_t)riddle_proc_diagnostic_start(index))
                || !write_u32((uint32_t)riddle_proc_diagnostic_end(index))
                || !write_u32((uint32_t)message_len)
                || !write_bytes(message, message_len)) {
                return 1;
            }
        }
        fflush(stdout);
    }
}
"#,
    );
    output
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
    let encoded = macro_name
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
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

pub(crate) struct ProcMacroHost {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    stderr_thread: Option<JoinHandle<()>>,
}

impl ProcMacroHost {
    pub(crate) fn spawn(path: &Path) -> anyhow::Result<Self> {
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to start proc-macro host `{}`", path.display()))?;
        let stdin = child.stdin.take().context("proc-macro host has no stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("proc-macro host has no stdout")?;
        let mut stderr = child
            .stderr
            .take()
            .context("proc-macro host has no stderr")?;
        let stderr_thread = std::thread::spawn(move || {
            let _ = io::copy(&mut stderr, &mut io::stderr());
        });
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout,
            stderr_thread: Some(stderr_thread),
        })
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
        let call_site_start = u32::try_from(call_site.start)
            .context("proc-macro call-site start exceeds the protocol range")?;
        let call_site_end = u32::try_from(call_site.end)
            .context("proc-macro call-site end exceeds the protocol range")?;
        let payload_len = 24usize
            .checked_add(macro_name.len())
            .and_then(|len| len.checked_add(input.len()))
            .and_then(|len| len.checked_add(second_input.len()))
            .context("proc-macro request is too large")?;
        if payload_len > MAX_FRAME {
            bail!("proc-macro request exceeds the frame limit");
        }
        let stdin = self
            .stdin
            .as_mut()
            .context("proc-macro host stdin is closed")?;
        write_u32(stdin, payload_len as u32)?;
        write_u32(stdin, PROTOCOL_VERSION)?;
        write_u32(stdin, macro_name.len() as u32)?;
        write_u32(stdin, input.len() as u32)?;
        write_u32(stdin, second_input.len() as u32)?;
        write_u32(stdin, call_site_start)?;
        write_u32(stdin, call_site_end)?;
        stdin.write_all(macro_name.as_bytes())?;
        stdin.write_all(input.as_bytes())?;
        stdin.write_all(second_input.as_bytes())?;
        stdin.flush()?;

        let response_len = read_u32(&mut self.stdout)? as usize;
        if !(16..=MAX_FRAME).contains(&response_len) {
            bail!("invalid proc-macro response frame length {response_len}");
        }
        let mut response = vec![0u8; response_len];
        self.stdout.read_exact(&mut response)?;
        let mut cursor = 0usize;
        let version = take_u32(&response, &mut cursor)?;
        let status = take_u32(&response, &mut cursor)?;
        let output_len = take_u32(&response, &mut cursor)? as usize;
        let diagnostic_count = take_u32(&response, &mut cursor)? as usize;
        let encoded_output = take_bytes(&response, &mut cursor, output_len)?;
        let output = if encoded_output.is_empty() {
            ProcMacroTokenStream::default()
        } else {
            ProcMacroTokenStream::decode(&encoded_output)
                .map_err(anyhow::Error::msg)
                .context("invalid structured proc-macro output")?
        };
        let mut diagnostics = Vec::with_capacity(diagnostic_count);
        for _ in 0..diagnostic_count {
            let level = take_u32(&response, &mut cursor)?;
            let start = take_u32(&response, &mut cursor)? as usize;
            let end = take_u32(&response, &mut cursor)? as usize;
            if start > end {
                bail!("invalid proc-macro diagnostic span");
            }
            let message_len = take_u32(&response, &mut cursor)? as usize;
            let message = take_bytes(&response, &mut cursor, message_len)?;
            diagnostics.push(ProcMacroDiagnostic {
                severity: match level {
                    1 => type_checker::Severity::Warning,
                    2 => type_checker::Severity::Note,
                    3 => type_checker::Severity::Help,
                    _ => type_checker::Severity::Error,
                },
                message,
                span: start..end,
            });
        }
        if version != PROTOCOL_VERSION || cursor != response.len() {
            bail!("invalid proc-macro response protocol");
        }
        if status != 0 && diagnostics.is_empty() {
            bail!("proc-macro host reported an unspecified failure");
        }
        Ok(ProcMacroExpansion {
            output,
            diagnostics,
        })
    }
}

impl Drop for ProcMacroHost {
    fn drop(&mut self) {
        self.stdin.take();
        if self.child.wait().is_err() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(thread) = self.stderr_thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) struct ClueProcMacroProvider {
    hosts: HashMap<String, ProcMacroHost>,
    exports: HashMap<String, Vec<ProcMacroExport>>,
    definitions: HashMap<(String, String), ProcMacroDefinition>,
}

impl ClueProcMacroProvider {
    pub(crate) fn build(packages: &[ProcMacroPackage]) -> anyhow::Result<Self> {
        let mut provider = Self {
            hosts: HashMap::new(),
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
            let executable =
                crate::build::build_proc_macro_host(package, &exports, &expansion.source)?;
            for export in &exports {
                let range = rowan::TextRange::new(
                    (export.function_name_range.start as u32).into(),
                    (export.function_name_range.end as u32).into(),
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
                .hosts
                .insert(package.alias.clone(), ProcMacroHost::spawn(&executable)?);
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
        let host = self
            .hosts
            .get_mut(package)
            .ok_or_else(|| format!("unknown proc-macro package `{package}`"))?;
        host.expand(macro_name, input, second_input, call_site)
            .map_err(|error| error.to_string())
    }
}

fn write_u32(writer: &mut impl Write, value: u32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> anyhow::Result<u32> {
    let value = bytes
        .get(*cursor..*cursor + 4)
        .context("truncated proc-macro response")?;
    *cursor += 4;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

fn take_bytes(bytes: &[u8], cursor: &mut usize, len: usize) -> anyhow::Result<String> {
    let value = bytes
        .get(*cursor..*cursor + len)
        .context("truncated proc-macro response payload")?;
    *cursor += len;
    String::from_utf8(value.to_vec()).context("proc-macro response is not UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_proc_macro_exports() {
        let valid = r#"
            #[proc_macro_derive(Answer, attributes(answer))]
            pub fun derive_answer(input: TokenStream) -> TokenStream { input }
        "#;
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

        let duplicate = r#"
            #[proc_macro_derive(Answer)]
            pub fun first(input: TokenStream) -> TokenStream { input }
            #[proc_macro_derive(Answer)]
            pub fun second(input: TokenStream) -> TokenStream { input }
        "#;
        assert!(
            discover_exports(duplicate)
                .unwrap_err()
                .to_string()
                .contains("exported more than once")
        );
    }

    #[test]
    fn proc_macro_host_uses_binary_stdio_on_windows() {
        let runtime = host_runtime_c(&[]);
        assert!(runtime.contains("_setmode(_fileno(stdin), _O_BINARY)"));
        assert!(runtime.contains("_setmode(_fileno(stdout), _O_BINARY)"));
    }
}
