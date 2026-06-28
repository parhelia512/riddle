use std::io::{self, Write};

use crate::pipeline::{CompileResult, DiagnosticExt, IntoDiagnosticExt};

/// Print diagnostics to stderr in rustc-inspired format.
/// Returns the error count.
pub fn report(result: &CompileResult, source: Option<&str>, source_name: &str) -> usize {
    let mut stderr = io::stderr();
    let mut count = 0;

    for e in &result.parse_errors {
        print_rust_style(&mut stderr, source, source_name, &e.to_ext(), "parse error");
        count += 1;
    }

    for d in &result.hir_diagnostics {
        print_rust_style(&mut stderr, source, source_name, &d.to_ext(), "hir error");
        if d.severity == type_checker::Severity::Error {
            count += 1;
        }
    }

    for d in &result.type_result.diagnostics {
        print_rust_style(&mut stderr, source, source_name, &d.to_ext(), "type error");
        if d.severity == type_checker::Severity::Error {
            count += 1;
        }
    }

    for d in &result.analysis_diagnostics {
        let stage = match d.code {
            "E0100" => "move error",
            "E0200" => "escape",
            _ => "analysis",
        };
        print_rust_style(&mut stderr, source, source_name, &d.to_ext(), stage);
        if d.severity == type_checker::Severity::Error {
            count += 1;
        }
    }

    count
}

pub fn report_verbose(result: &CompileResult, source: Option<&str>, source_name: &str) {
    if result.parse_errors.is_empty() {
        println!("parse: ok");
    }

    if result.hir_diagnostics.is_empty() {
        println!("hir lower: ok");
    }

    if result.type_result.diagnostics.is_empty() {
        println!("type check: ok");
    }

    if result.analysis_diagnostics.is_empty() {
        println!("move + escape analysis: ok");
    }

    if result.success() && result.mir_module.is_some() {
        println!("MIR lowering: ok");
    }

    if !result.success() {
        println!();
        report(result, source, source_name);
    }
}

// ── rustc-style formatting ──────────────────────────────────────────────

const RED: &str = "\x1b[1;31m";
const YELLOW: &str = "\x1b[1;33m";
const BLUE: &str = "\x1b[1;34m";
const CYAN: &str = "\x1b[1;36m";
const RESET: &str = "\x1b[0m";

fn print_rust_style(
    out: &mut impl Write,
    source: Option<&str>,
    source_name: &str,
    d: &DiagnosticExt,
    stage: &str,
) {
    let severity_str = match d.severity {
        type_checker::Severity::Error => format!("{RED}error{RESET}"),
        type_checker::Severity::Warning => format!("{YELLOW}warning{RESET}"),
        type_checker::Severity::Note => format!("{CYAN}note{RESET}"),
        type_checker::Severity::Help => format!("{BLUE}help{RESET}"),
    };

    let code = if d.code.is_empty() { stage.to_string() } else { d.code.to_string() };

    // Header
    let _ = write!(out, "{severity_str}[{code}]: {}\n", d.message);

    // Primary label — show source context
    if let (Some(source), Some(primary)) = (source, d.labels.first()) {
        let line_col = offset_to_line_col(source, primary.range.start());
        let _ = writeln!(out, " {BLUE}-->{RESET} {source_name}:{}:{}", line_col.line, line_col.col);

        let annotated = annotate_source(source, &d.labels);
        let _ = write!(out, "{}", annotated);
    } else {
        let _ = writeln!(out, " {BLUE}-->{RESET} {source_name}");
    }

    // Help
    if let Some(ref help) = d.help {
        let _ = writeln!(out, "{BLUE}help{RESET}: {help}");
    }

    // Notes
    for note in &d.notes {
        let _ = writeln!(out, "{CYAN}note{RESET}: {note}");
    }

    let _ = writeln!(out);
}

/// Build a gutter prefix like ` 10 | ` or `    | ` with consistent width.
fn gutter(line_no: Option<usize>, width: usize) -> String {
    match line_no {
        Some(n) => format!(" {BLUE}{:>width$} |{RESET} ", n, width = width),
        None => format!(" {:>width$} | ", "", width = width),
    }
}

/// Annotate source text with underline markers for diagnostic labels.
fn annotate_source(source: &str, labels: &[type_checker::SourceLabel]) -> String {
    // Group labels by line
    let mut line_labels: std::collections::BTreeMap<usize, Vec<(usize, usize, &str, bool)>> =
        std::collections::BTreeMap::new();

    for label in labels {
        let start_lc = offset_to_line_col(source, label.range.start());
        let end_lc = offset_to_line_col(source, label.range.end());
        let is_primary = matches!(label.style, type_checker::LabelStyle::Primary);

        if start_lc.line == end_lc.line {
            line_labels
                .entry(start_lc.line)
                .or_default()
                .push((start_lc.col - 1, end_lc.col - 1, label.message.as_str(), is_primary));
        } else {
            line_labels
                .entry(start_lc.line)
                .or_default()
                .push((start_lc.col - 1, 0, label.message.as_str(), is_primary));
        }
    }

    if line_labels.is_empty() {
        return String::new();
    }

    // Collect lines and strip \r (Windows CRLF)
    let raw_lines: Vec<String> = source.lines().map(|s| s.trim_end_matches('\r').to_string()).collect();
    let max_line = line_labels.keys().last().copied().unwrap_or(1);
    let min_line = line_labels.keys().next().copied().unwrap_or(1);
    let gutter_w = max_line.to_string().len().max(1);

    let context_start = (min_line - 1).max(1);
    let context_end = (max_line + 1).min(raw_lines.len());

    let mut out = String::new();
    // Opening margin line (rustc style)
    out.push_str(&format!(" {:>width$} |\n", "", width = gutter_w));

    for line_no in context_start..=context_end {
        let source_line = raw_lines.get(line_no - 1).map(|s| s.as_str()).unwrap_or("");

        // Source line with gutter
        out.push_str(&format!("{}{}\n", gutter(Some(line_no), gutter_w), source_line));

        // Marker line
        if let Some(labels_for_line) = line_labels.get(&line_no) {
            let line_len = source_line.len().max(1);
            let mut markers: Vec<u8> = vec![b' '; line_len];

            for &(start_byte, end_byte, _msg, is_primary) in labels_for_line {
                let end = if end_byte == 0 { line_len } else { end_byte };
                for i in start_byte..end.min(line_len) {
                    markers[i] = if is_primary { b'^' } else { b'-' };
                }
            }

            // Trim trailing spaces
            while markers.last() == Some(&b' ') {
                markers.pop();
            }
            let underline: String = markers.iter().map(|&b| b as char).collect();

            if !underline.is_empty() {
                out.push_str(&format!("{}{RED}{}{RESET}", gutter(None, gutter_w), underline));

                // Show label message for primary labels
                for &(_, _, msg, is_primary) in labels_for_line {
                    if is_primary && !msg.is_empty() {
                        out.push_str(&format!(" {msg}"));
                    }
                }
            }
            out.push('\n');
        }
    }

    out
}

#[derive(Debug, Clone, Copy)]
struct LineCol {
    line: usize,  // 1-based
    col: usize,   // 1-based
}

fn offset_to_line_col(source: &str, offset: rowan::TextSize) -> LineCol {
    let offset: usize = offset.into();
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    LineCol { line, col }
}
