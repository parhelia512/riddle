//! Session-based REPL engine, independent of any line-editor.
//!
//! Feed lines to [`Session::eval`]; each call recompiles the session source
//! against the bundled standard library and interprets it with the MIR
//! interpreter. Output is replayed on every evaluation (the whole program
//! reruns), so only the bytes past the previous run's output are reported
//! as new — this requires deterministic programs, which is also how the
//! C backend behaves.

use interpreter::{Session as RunSession, Trap, Val};
use riddlec::pipeline::{self, CompileResult};

/// Result of feeding one logical input to the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A definition (or `use`) was accepted into the session.
    Defined,
    /// A `let` binding was accepted; nothing to print.
    Bound,
    /// An expression was evaluated; render this text for the user.
    Evaluated(String),
    /// A REPL command produced output (help text, MIR listing, notice).
    Notice(String),
    /// The user asked to quit.
    Quit,
}

/// Why an input was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    /// Parse/type errors; carries the rendered diagnostics.
    Diagnostics(String),
    /// The program trapped at runtime (panic, abort, …); carries the
    /// rendered trap output.
    Runtime(String),
    /// Interpreter-level failure.
    Internal(String),
}

pub struct Session {
    /// Top-level definitions accumulated so far (`fun`, `struct`, `use`, …).
    definitions: String,
    /// Statement lines inside the generated `main`.
    statements: Vec<String>,
    last_mir: Option<String>,
}

const HELP: &str = "commands: :help  :reset  :mir  :quit | \
expressions print their value and bind `__` | definitions (fun/struct/…) join the session";

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// Prefix marking the one-off value echo inside program output: the
/// expression evaluator prints `println!("\0r{:?}", expr)` and carves the
/// value out of stdout after this marker. `\0` survives Riddle's escape
/// decoding as a NUL byte and effectively never collides with user output.
const VALUE_MARKER: &str = "\u{0}r";

/// Path shown in panic locations for REPL-evaluated code.
const SOURCE_NAME: &str = "repl";

impl Session {
    #[must_use]
    pub fn new() -> Self {
        Self {
            definitions: String::new(),
            statements: Vec::new(),
            last_mir: None,
        }
    }

    /// Whether `text` ends with unbalanced delimiters, so the caller should
    /// keep reading continuation lines before evaluating. Delimiters inside
    /// string/char literals are counted too — close them on their own line
    /// if a literal contains unbalanced brackets.
    #[must_use]
    pub fn needs_continuation(text: &str) -> bool {
        let mut balance: i64 = 0;
        for ch in text.chars() {
            match ch {
                '{' | '(' | '[' => balance += 1,
                '}' | ')' | ']' => balance -= 1,
                _ => {}
            }
        }
        balance > 0
    }

    /// Appends a continuation line to the pending input (no evaluation).
    /// Returns the combined text so the caller can keep checking balance.
    #[must_use]
    pub fn combine(pending: &str, line: &str) -> String {
        let mut combined = String::from(pending);
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(line);
        combined
    }

    /// Evaluates one logical input line (already continuation-combined).
    pub fn eval(&mut self, line: &str) -> Result<Outcome, Reject> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(Outcome::Notice(String::new()));
        }
        if let Some(command) = trimmed.strip_prefix(':') {
            return self.command(command);
        }
        match classify(trimmed) {
            Kind::Definition => self.eval_definition(line),
            Kind::Let => self.eval_let(line),
            Kind::Expression => self.eval_expression(trimmed),
        }
    }

    fn command(&mut self, command: &str) -> Result<Outcome, Reject> {
        match command.trim() {
            "help" | "?" => Ok(Outcome::Notice(HELP.to_string())),
            "quit" | "exit" | "q" => Ok(Outcome::Quit),
            "reset" => {
                self.definitions.clear();
                self.statements.clear();
                self.last_mir = None;
                Ok(Outcome::Notice("session reset".to_string()))
            }
            "mir" => Ok(Outcome::Notice(self.last_mir.clone().unwrap_or_else(
                || "no compiled program yet; evaluate something first".to_string(),
            ))),
            other => Ok(Outcome::Notice(format!(
                "unknown command `:{other}` — {HELP}"
            ))),
        }
    }

    /// Full source of the current session with `body` as `main`'s body.
    fn source_with_body(&self, body: &str) -> String {
        let mut source = self.definitions.clone();
        source.push_str("\n\nfun main() -> i32 {\n");
        source.push_str(body);
        source.push_str("\n    0\n}\n");
        source
    }

    /// Session source whose `main` has no return type and no trailing
    /// literal, so a never-typed tail expression type-checks.
    fn source_with_unit_main(&self, body: &str) -> String {
        let mut source = self.definitions.clone();
        source.push_str("\n\nfun main() {\n");
        source.push_str(body);
        source.push_str("\n}\n");
        source
    }

    fn compile(&self, body: &str) -> Result<CompileResult, Reject> {
        let source = self.source_with_body(body);
        let result = pipeline::compile_for_interpretation(&source, SOURCE_NAME);
        if result.success() {
            Ok(result)
        } else {
            Err(Reject::Diagnostics(render_diagnostics(&source, &result)))
        }
    }

    fn eval_definition(&mut self, line: &str) -> Result<Outcome, Reject> {
        let saved = self.definitions.clone();
        if !self.definitions.is_empty() {
            self.definitions.push('\n');
        }
        self.definitions.push_str(line);
        if let Err(error) = self.compile("") {
            self.definitions = saved;
            return Err(error);
        }
        Ok(Outcome::Defined)
    }

    fn eval_let(&mut self, line: &str) -> Result<Outcome, Reject> {
        let mut statements = self.statements.clone();
        // Let lines may omit the trailing semicolon conversationally; a
        // trailing line comment must not swallow the inserted `;`.
        let (code, comment) = split_trailing_comment(line.trim_end());
        let code = code.trim_end();
        let terminated = if code.ends_with(';') {
            code.to_string()
        } else {
            format!("{code};")
        };
        let statement = match comment {
            Some(comment) => format!("{terminated} {comment}"),
            None => terminated,
        };
        statements.push(format!("    {statement}"));
        let body = statements.join("\n");
        self.compile(&body)?;
        self.statements = statements;
        Ok(Outcome::Bound)
    }

    fn eval_expression(&mut self, expression: &str) -> Result<Outcome, Reject> {
        // Evaluate as `println!("\0r{:?}", expr)` so the value renders
        // through the same Debug machinery the C backend uses; the marker
        // lets us separate the echoed value from replayed program output.
        // The println is a one-off suffix — it must NOT join the journal,
        // or every later evaluation would re-print it (only
        // `let __ = expr` persists, including its side effects).
        let mut statements = self.statements.clone();
        statements.push(format!("    println!(\"\\0r{{:?}}\", {expression});"));
        let body = statements.join("\n");
        match self.compile(&body) {
            Ok(result) => self.run_expression(result, expression),
            // Expressions of type `!` (panic, exit, …) cannot take the
            // Debug-echo form; evaluate them as the tail of a Unit `main`
            // so the trap surfaces at runtime instead.
            Err(diagnostics) => self.run_terminating_expression(expression, diagnostics),
        }
    }

    fn run_terminating_expression(
        &mut self,
        expression: &str,
        first_error: Reject,
    ) -> Result<Outcome, Reject> {
        let mut statements = self.statements.clone();
        statements.push(format!("    {expression}"));
        let body = statements.join("\n");
        let source = self.source_with_unit_main(&body);
        let result = pipeline::compile_for_interpretation(&source, SOURCE_NAME);
        if !result.success() {
            // Not a terminating expression after all; report the original
            // diagnostics.
            return Err(first_error);
        }
        let module = result.mir_module.expect("successful compile produced MIR");
        let source_files = result.source_files;
        let mut run = RunSession::new(&module, source_files, &interpreter::Config::default());
        match run.call("main", Vec::new()) {
            Ok(Val::Unit) => Ok(Outcome::Evaluated(String::new())),
            Ok(_) => Err(Reject::Internal(
                "terminating expression returned a value".to_string(),
            )),
            Err(trap) => {
                let mut rendered = run.take_stderr();
                trap.render(&mut rendered);
                Err(Reject::Runtime(
                    String::from_utf8_lossy(&rendered).into_owned(),
                ))
            }
        }
    }

    fn run_expression(
        &mut self,
        result: CompileResult,
        expression: &str,
    ) -> Result<Outcome, Reject> {
        let module = result.mir_module.expect("successful compile produced MIR");

        let source_files = result.source_files;
        let mut run = RunSession::new(&module, source_files, &interpreter::Config::default());
        let value = run.call("main", Vec::new());
        let stdout = run.take_stdout();
        let stderr = run.take_stderr();
        match value {
            Ok(Val::Int(_)) => {
                let text = String::from_utf8_lossy(&stdout).into_owned();
                let (replayed, echoed) = match text.find(VALUE_MARKER) {
                    Some(index) => (
                        text[..index].to_string(),
                        text[index + VALUE_MARKER.len()..].to_string(),
                    ),
                    None => (text, String::new()),
                };
                self.statements
                    .push(format!("    let __ = ({expression});"));
                self.last_mir = Some(format!("{module}"));
                let mut output = replayed;
                // Suppress the echo when it is just `()` (no Debug output
                // worth repeating, e.g. `println!` calls as expressions).
                if echoed != "()\n" {
                    output.push_str(&echoed);
                }
                Ok(Outcome::Evaluated(output))
            }
            Ok(_) => Err(Reject::Internal(
                "main returned a non-integer value".to_string(),
            )),
            Err(trap) => {
                let mut rendered = stderr;
                trap.render(&mut rendered);
                Err(Reject::Runtime(
                    String::from_utf8_lossy(&rendered).into_owned(),
                ))
            }
        }
    }

    /// The rendered MIR of the last accepted program, if any.
    #[must_use]
    pub fn last_mir(&self) -> Option<&str> {
        self.last_mir.as_deref()
    }
}

/// Drives a whole session from input lines, writing results to `out`.
/// Returns `false` if the session ended via `:quit`.
pub fn run_session(lines: &mut dyn Iterator<Item = String>, out: &mut dyn std::io::Write) -> bool {
    let mut session = Session::new();
    let mut pending: Option<String> = None;
    writeln!(out, "Riddle REPL — type :help for commands").ok();
    for line in &mut *lines {
        let input = match pending.take() {
            Some(text) => Session::combine(&text, &line),
            None => line,
        };
        if Session::needs_continuation(&input) {
            pending = Some(input);
            continue;
        }
        match session.eval(&input) {
            Ok(Outcome::Quit) => {
                return false;
            }
            Ok(Outcome::Notice(notice)) => {
                if !notice.is_empty() {
                    writeln!(out, "{notice}").ok();
                }
            }
            Ok(Outcome::Evaluated(value)) => {
                if !value.is_empty() {
                    if value.ends_with('\n') {
                        write!(out, "{value}").ok();
                    } else {
                        writeln!(out, "{value}").ok();
                    }
                }
            }
            Ok(Outcome::Defined) | Ok(Outcome::Bound) => {}
            Err(Reject::Diagnostics(text)) => {
                write!(out, "{text}").ok();
            }
            Err(Reject::Runtime(text)) | Err(Reject::Internal(text)) => {
                write!(out, "{text}").ok();
            }
        }
    }
    true
}

fn render_diagnostics(source: &str, result: &CompileResult) -> String {
    let mut text = String::new();
    for diagnostic in result
        .type_result
        .diagnostics
        .iter()
        .chain(result.hir_diagnostics.iter())
        .chain(result.analysis_diagnostics.iter())
    {
        text.push_str(&render_diagnostic(source, diagnostic));
        text.push('\n');
    }
    for error in &result.parse_errors {
        let offset = usize::from(error.span.start()).min(source.len());
        let prefix = &source[..offset];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        let column = source[line_start..offset].chars().count() + 1;
        text.push_str(&format!("error: {} at {line}:{column}\n", error.message));
    }
    if text.is_empty() {
        text.push_str("error: compilation failed\n");
    }
    text
}

fn render_diagnostic(source: &str, diagnostic: &type_checker::Diagnostic) -> String {
    use std::fmt::Write as _;
    let mut text = String::new();
    let severity = match diagnostic.severity {
        type_checker::Severity::Error => "error",
        type_checker::Severity::Warning => "warning",
        type_checker::Severity::Note => "note",
        type_checker::Severity::Help => "help",
    };
    let _ = writeln!(
        text,
        "{severity}[{}]: {}",
        diagnostic.code, diagnostic.message
    );
    for label in &diagnostic.labels {
        let offset = usize::from(label.range.start()).min(source.len());
        let prefix = &source[..offset];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        let column = source[line_start..offset].chars().count() + 1;
        let style = match label.style {
            type_checker::LabelStyle::Primary => "",
            type_checker::LabelStyle::Secondary => " (secondary)",
        };
        let _ = writeln!(text, "  {line}:{column}{style}: {}", label.message);
    }
    if let Some(help) = &diagnostic.help {
        let _ = writeln!(text, "  help: {help}");
    }
    for note in &diagnostic.notes {
        let _ = writeln!(text, "  note: {note}");
    }
    text
}

enum Kind {
    Definition,
    Let,
    Expression,
}

/// Splits a trailing `// …` line comment off `text`, ignoring `//` inside
/// string and char literals and comments on inner lines, so a statement
/// terminator can be inserted before the comment instead of inside it.
fn split_trailing_comment(text: &str) -> (&str, Option<&str>) {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b'"' {
                    index += if bytes[index] == b'\\' { 2 } else { 1 };
                }
                index += 1;
            }
            b'\'' if is_char_literal(bytes, index) => {
                index += if bytes[index + 1] == b'\\' { 4 } else { 3 };
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                match text[index..].find('\n') {
                    // The comment reaches end-of-line: it is the trailing
                    // one.
                    None => return (&text[..index], Some(&text[index..])),
                    // A comment on an inner line only covers its own line;
                    // keep scanning after it.
                    Some(line_end) => index += line_end + 1,
                }
            }
            _ => index += 1,
        }
    }
    (text, None)
}

/// Whether the `'` at `index` opens a char literal rather than an
/// apostrophe or lifetime marker: the quote must close after one
/// (possibly escaped) character.
fn is_char_literal(bytes: &[u8], index: usize) -> bool {
    match bytes.get(index + 1) {
        Some(b'\\') => bytes.get(index + 3) == Some(&b'\''),
        Some(_) => bytes.get(index + 2) == Some(&b'\''),
        None => false,
    }
}

fn classify(line: &str) -> Kind {
    let first = line.split_whitespace().next().unwrap_or_default();
    match first {
        "fun" | "struct" | "enum" | "trait" | "impl" | "mod" | "use" | "const" | "type"
        | "unsafe" | "extern" | "pub" => Kind::Definition,
        _ if line.starts_with("#[") => Kind::Definition,
        _ if first == "let" || line.starts_with("let(") => Kind::Let,
        _ => Kind::Expression,
    }
}

/// Exit code for an abnormal trap, matching the C backend's process end.
#[must_use]
pub fn trap_exit_code(trap: &Trap) -> u8 {
    match trap {
        // abort() on Unix is SIGABRT (134); on Windows it is exit code 3.
        Trap::Panic { .. }
        | Trap::Abort { .. }
        | Trap::Unreachable { .. }
        | Trap::UnsupportedExtern { .. } => {
            if cfg!(windows) {
                3
            } else {
                134
            }
        }
        Trap::ProcessExit(code) => (*code).max(0) as u8,
        Trap::StackOverflow { .. } | Trap::Internal(_) => 101,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evaluate(lines: &[&str]) -> Vec<Outcome> {
        let mut session = Session::new();
        let mut pending: Option<String> = None;
        let mut outcomes = Vec::new();
        for line in lines {
            let input = match pending.take() {
                Some(text) => Session::combine(&text, line),
                None => (*line).to_string(),
            };
            if Session::needs_continuation(&input) {
                pending = Some(input);
                continue;
            }
            outcomes.push(session.eval(&input).expect("input should evaluate"));
        }
        outcomes
    }

    #[test]
    fn expressions_evaluate_and_bind() {
        let outcomes = evaluate(&["1 + 2", "__ * 10"]);
        assert_eq!(
            outcomes,
            vec![
                Outcome::Evaluated("3\n".to_string()),
                Outcome::Evaluated("30\n".to_string()),
            ]
        );
    }

    #[test]
    fn let_lines_bind_names_without_echo() {
        let outcomes = evaluate(&["let x = 21", "x + x"]);
        assert_eq!(outcomes[0], Outcome::Bound);
        assert_eq!(outcomes[1], Outcome::Evaluated("42\n".to_string()));
    }

    #[test]
    fn let_lines_keep_trailing_comments_working() {
        // The inserted `;` must land before the comment, not inside it.
        let outcomes = evaluate(&["let x = 1 // answer", "x"]);
        assert_eq!(outcomes[0], Outcome::Bound);
        assert_eq!(outcomes[1], Outcome::Evaluated("1\n".to_string()));

        let outcomes = evaluate(&["let x = 1; // answer", "x + 1"]);
        assert_eq!(outcomes[0], Outcome::Bound);
        assert_eq!(outcomes[1], Outcome::Evaluated("2\n".to_string()));

        // `//` inside string and char literals is not a comment.
        let outcomes = evaluate(&["let s = \"a//b\"", "s == \"a//b\""]);
        assert_eq!(outcomes[0], Outcome::Bound);
        assert_eq!(outcomes[1], Outcome::Evaluated("true\n".to_string()));
    }

    #[test]
    fn definitions_join_the_session() {
        let outcomes = evaluate(&["fun triple(n: i32) -> i32 { n * 3 }", "triple(5)"]);
        assert_eq!(outcomes[0], Outcome::Defined);
        assert_eq!(outcomes[1], Outcome::Evaluated("15\n".to_string()));
    }

    #[test]
    fn multi_line_definitions_are_accepted() {
        let mut session = Session::new();
        let first = "struct Point {";
        assert!(Session::needs_continuation(first));
        let combined = Session::combine(first, "    x: i32,");
        let combined = Session::combine(&combined, "    y: i32,");
        let combined = Session::combine(&combined, "}");
        assert!(!Session::needs_continuation(&combined));
        assert!(matches!(session.eval(&combined), Ok(Outcome::Defined)));
        assert!(matches!(
            session.eval("Point { x: 1, y: 2 }.x"),
            Ok(Outcome::Evaluated(_))
        ));
    }

    #[test]
    fn rejected_input_does_not_mutate_the_session() {
        let mut session = Session::new();
        assert!(session.eval("let good = 1;").is_ok());
        assert!(session.eval("let bad = ;").is_err());
        assert!(matches!(
            session.eval("good + 1"),
            Ok(Outcome::Evaluated(_))
        ));
    }

    #[test]
    fn panics_report_and_keep_the_session_usable() {
        let mut session = Session::new();
        let outcome = session.eval("crate::std::panic::panic(\"boom\")");
        assert!(matches!(outcome, Err(Reject::Runtime(_))));
        assert!(matches!(session.eval("2 + 2"), Ok(Outcome::Evaluated(_))));
    }

    #[test]
    fn commands_work() {
        let mut session = Session::new();
        assert!(matches!(session.eval(":help"), Ok(Outcome::Notice(_))));
        assert!(matches!(session.eval(":mir"), Ok(Outcome::Notice(_))));
        session.eval("1 + 1").unwrap();
        assert!(session.last_mir().is_some());
        assert!(matches!(session.eval(":quit"), Ok(Outcome::Quit)));
    }

    #[test]
    fn reset_clears_everything() {
        let mut session = Session::new();
        session.eval("let x = 1;").unwrap();
        assert!(matches!(session.eval(":reset"), Ok(Outcome::Notice(_))));
        assert!(session.eval("x").is_err());
    }

    #[test]
    fn run_session_writes_output_and_stops_on_quit() {
        let input = ["1 + 1", ":quit", "3 + 3"].map(str::to_string);
        let mut out: Vec<u8> = Vec::new();
        let completed = run_session(&mut input.into_iter(), &mut out);
        assert!(!completed, "quit should end the session");
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("2\n"), "output: {text}");
        assert!(!text.contains("6\n"), "nothing after :quit: {text}");
    }
}
