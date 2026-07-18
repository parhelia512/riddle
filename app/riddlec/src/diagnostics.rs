use std::io::{self, IsTerminal, Write};

use crate::pipeline::{CompileResult, DiagnosticExt, IntoDiagnosticExt, LoadedSource};

/// Print diagnostics to stderr in rustc-inspired format.
/// Returns the error count.
pub fn report(result: &CompileResult, source: Option<&str>, source_name: &str) -> usize {
    let mut stderr = io::stderr();
    let color = stderr.is_terminal();
    let errors = report_with(result, |diagnostic| {
        print_rust_style(&mut stderr, source, source_name, diagnostic, color)
    });
    print_summary(&mut stderr, errors, color);
    errors
}

pub fn report_mapped(result: &CompileResult, source: &LoadedSource, source_name: &str) -> usize {
    let mut stderr = io::stderr();
    let color = stderr.is_terminal();
    let errors = report_with(result, |diagnostic| {
        print_mapped(&mut stderr, source, source_name, diagnostic, color)
    });
    print_summary(&mut stderr, errors, color);
    errors
}

fn report_with(result: &CompileResult, mut emit: impl FnMut(&DiagnosticExt)) -> usize {
    let mut count = 0;

    for e in &result.parse_errors {
        emit(&e.to_ext());
        count += 1;
    }

    for d in &result.hir_diagnostics {
        emit(&d.to_ext());
        if d.severity == type_checker::Severity::Error {
            count += 1;
        }
    }

    for d in &result.type_result.diagnostics {
        emit(&d.to_ext());
        if d.severity == type_checker::Severity::Error {
            count += 1;
        }
    }

    for d in &result.analysis_diagnostics {
        emit(&d.to_ext());
        if d.severity == type_checker::Severity::Error {
            count += 1;
        }
    }

    count
}

fn print_mapped(
    out: &mut impl Write,
    source: &LoadedSource,
    source_name: &str,
    diagnostic: &DiagnosticExt,
    color: bool,
) {
    let Some(primary) = diagnostic
        .labels
        .iter()
        .find(|label| label.style == type_checker::LabelStyle::Primary)
        .and_then(|label| source.source_map.map_range(label.range))
    else {
        print_rust_style(out, Some(&source.source), source_name, diagnostic, color);
        return;
    };

    let mut mapped = diagnostic.clone();
    mapped.labels = diagnostic
        .labels
        .iter()
        .filter_map(|label| {
            let location = source.source_map.map_range(label.range)?;
            (location.path == primary.path).then(|| type_checker::SourceLabel {
                range: location.range,
                message: label.message.clone(),
                style: label.style,
            })
        })
        .collect();
    for label in &diagnostic.labels {
        let Some(location) = source.source_map.map_range(label.range) else {
            continue;
        };
        if location.path != primary.path && !label.message.is_empty() {
            let line_col = offset_to_line_col(location.source, location.range.start());
            mapped.notes.push(format!(
                "{}:{}:{}: {}",
                display_path(location.path),
                line_col.line,
                line_col.col,
                label.message
            ));
        }
    }
    let mapped_source_name =
        if std::fs::canonicalize(source_name).is_ok_and(|path| path == primary.path) {
            display_path(std::path::Path::new(source_name))
        } else {
            display_path(primary.path)
        };
    print_rust_style(
        out,
        Some(primary.source),
        &mapped_source_name,
        &mapped,
        color,
    );
}

pub fn report_verbose(result: &CompileResult, _source: Option<&str>, _source_name: &str) {
    if result.parse_errors.is_empty() {
        println!("parse: ok");
    } else {
        println!("parse: failed");
        println!("hir lower: skipped");
        println!("type check: skipped");
        println!("move + escape analysis: skipped");
        println!("MIR lowering: skipped");
        return;
    }

    if result.hir_diagnostics.is_empty() {
        println!("hir lower: ok");
    } else {
        println!("hir lower: failed");
    }

    if result.type_result.diagnostics.is_empty() {
        println!("type check: ok");
    } else {
        println!("type check: failed");
    }

    if result.analysis_diagnostics.is_empty() {
        println!("move + escape analysis: ok");
    } else {
        println!("move + escape analysis: failed");
    }

    if result.success() && result.mir_module.is_some() {
        println!("MIR lowering: ok");
    } else if result.success() {
        println!("MIR lowering: skipped");
    } else {
        println!("MIR lowering: failed");
    }
}

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
    color: bool,
) {
    let severity = match d.severity {
        type_checker::Severity::Error => "error",
        type_checker::Severity::Warning => "warning",
        type_checker::Severity::Note => "note",
        type_checker::Severity::Help => "help",
    };
    let primary_color = enabled_color(
        color,
        match d.severity {
            type_checker::Severity::Error => RED,
            type_checker::Severity::Warning => YELLOW,
            type_checker::Severity::Note => CYAN,
            type_checker::Severity::Help => BLUE,
        },
    );
    let blue = enabled_color(color, BLUE);
    let reset = enabled_color(color, RESET);

    if d.code.is_empty() {
        let _ = writeln!(out, "{primary_color}{severity}{reset}: {}", d.message);
    } else {
        let _ = writeln!(
            out,
            "{primary_color}{severity}[{}]{reset}: {}",
            d.code, d.message
        );
    }

    let primary = d
        .labels
        .iter()
        .find(|label| label.style == type_checker::LabelStyle::Primary);
    let gutter_width = if let (Some(source), Some(primary)) = (source, primary) {
        let trim_start = trim_leading_trivia(source, primary.range.start(), primary.range.end());
        let line_col = offset_to_line_col(source, trim_start);
        let _ = writeln!(
            out,
            " {blue}-->{reset} {source_name}:{}:{}",
            line_col.line, line_col.col
        );

        let (annotated, width) = annotate_source(source, &d.labels, primary_color, blue, reset);
        let _ = write!(out, "{annotated}");
        width
    } else {
        let _ = writeln!(out, " {blue}-->{reset} {source_name}");
        1
    };

    if let Some(help) = &d.help {
        let _ = writeln!(
            out,
            " {blue}{:>gutter_width$}={reset} {blue}help{reset}: {help}",
            ""
        );
    }

    for note in &d.notes {
        let cyan = enabled_color(color, CYAN);
        let _ = writeln!(
            out,
            " {blue}{:>gutter_width$}={reset} {cyan}note{reset}: {note}",
            ""
        );
    }

    let _ = writeln!(out);
}

fn print_summary(out: &mut impl Write, errors: usize, color: bool) {
    if errors == 0 {
        return;
    }
    let red = enabled_color(color, RED);
    let reset = enabled_color(color, RESET);
    let suffix = if errors == 1 { "" } else { "s" };
    let _ = writeln!(
        out,
        "{red}error{reset}: aborting due to {errors} previous error{suffix}"
    );
}

fn enabled_color(enabled: bool, code: &'static str) -> &'static str {
    if enabled { code } else { "" }
}

fn display_path(path: &std::path::Path) -> String {
    let path = path.display().to_string();
    if let Some(path) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{path}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
    }
}

fn gutter(line_no: Option<usize>, width: usize, color: bool) -> String {
    let blue = enabled_color(color, BLUE);
    let reset = enabled_color(color, RESET);
    match line_no {
        Some(n) => format!(" {blue}{n:>width$} |{reset} "),
        None => format!(" {blue}{:>width$} |{reset} ", ""),
    }
}

fn annotate_source<'a>(
    source: &'a str,
    labels: &'a [type_checker::SourceLabel],
    primary_color: &str,
    secondary_color: &str,
    reset: &str,
) -> (String, usize) {
    let mut line_labels: std::collections::BTreeMap<
        usize,
        Vec<(usize, Option<usize>, &'a str, bool)>,
    > = std::collections::BTreeMap::new();

    for label in labels {
        let (trim_start, trim_end) = trim_range(source, label.range.start(), label.range.end());
        let start_lc = offset_to_line_col(source, trim_start);
        let end_lc = offset_to_line_col(source, trim_end);
        let is_primary = matches!(label.style, type_checker::LabelStyle::Primary);

        if start_lc.line == end_lc.line {
            let start = start_lc.col - 1;
            let end = (end_lc.col - 1).max(start + 1);
            line_labels.entry(start_lc.line).or_default().push((
                start,
                Some(end),
                label.message.as_str(),
                is_primary,
            ));
        } else {
            line_labels.entry(start_lc.line).or_default().push((
                start_lc.col - 1,
                None,
                label.message.as_str(),
                is_primary,
            ));
            for line in (start_lc.line + 1)..end_lc.line {
                line_labels
                    .entry(line)
                    .or_default()
                    .push((0, None, "", is_primary));
            }
            if end_lc.col > 1 {
                line_labels.entry(end_lc.line).or_default().push((
                    0,
                    Some(end_lc.col - 1),
                    "",
                    is_primary,
                ));
            }
        }
    }

    if line_labels.is_empty() {
        return (String::new(), 1);
    }

    let raw_lines: Vec<String> = source
        .split('\n')
        .map(|s| s.trim_end_matches('\r').to_string())
        .collect();
    let max_line = line_labels.keys().last().copied().unwrap_or(1);
    let gutter_w = max_line.to_string().len().max(1);
    let mut out = String::new();
    out.push_str(gutter(None, gutter_w, !secondary_color.is_empty()).trim_end());
    out.push('\n');

    let mut previous_line = None;
    for (&line_no, labels_for_line) in &line_labels {
        if previous_line.is_some_and(|previous| line_no > previous + 1) {
            out.push_str(" ...\n");
        }
        previous_line = Some(line_no);
        let source_line = raw_lines.get(line_no - 1).map(|s| s.as_str()).unwrap_or("");
        out.push_str(&format!(
            "{}{}\n",
            gutter(Some(line_no), gutter_w, !secondary_color.is_empty()),
            source_line
        ));

        let source_chars: Vec<char> = source_line.chars().collect();
        let line_len = labels_for_line
            .iter()
            .map(|(start, end, _, _)| end.unwrap_or(source_chars.len()).max(start + 1))
            .max()
            .unwrap_or(1)
            .max(source_chars.len())
            .max(1);
        let mut markers = vec![0_u8; line_len];

        for &(start, end, _, is_primary) in labels_for_line {
            let marker = if is_primary { 2 } else { 1 };
            for current in markers
                .iter_mut()
                .take(end.unwrap_or(line_len).min(line_len))
                .skip(start.min(line_len))
            {
                if marker == 2 || *current == 0 {
                    *current = marker;
                }
            }
        }

        while markers.last() == Some(&0) {
            markers.pop();
        }
        if markers.is_empty() {
            continue;
        }

        out.push_str(&gutter(None, gutter_w, !secondary_color.is_empty()));
        for (index, marker) in markers.iter().enumerate() {
            match marker {
                2 => out.push_str(&format!("{primary_color}^{reset}")),
                1 => out.push_str(&format!("{secondary_color}-{reset}")),
                _ if source_chars.get(index) == Some(&'\t') => out.push('\t'),
                _ => out.push(' '),
            }
        }

        let messages = labels_for_line
            .iter()
            .filter_map(|(_, _, message, _)| (!message.is_empty()).then_some(*message))
            .collect::<Vec<_>>();
        if !messages.is_empty() {
            let message_color = labels_for_line
                .iter()
                .find(|(_, _, message, _)| !message.is_empty())
                .map_or(secondary_color, |(_, _, _, primary)| {
                    if *primary {
                        primary_color
                    } else {
                        secondary_color
                    }
                });
            out.push_str(&format!(" {message_color}{}{reset}", messages.join("; ")));
        }
        out.push('\n');
    }

    out.push_str(gutter(None, gutter_w, !secondary_color.is_empty()).trim_end());
    out.push('\n');
    (out, gutter_w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_style_output_preserves_diagnostic_hierarchy() {
        let source =
            "struct Foo{}\n\nfun main(){\n    let a = Foo{};\n    let b = a;\n    let c = a;\n}\n";
        let moved = source.find("a;\n    let c").unwrap();
        let used = source.rfind("a;").unwrap();
        let mut diagnostic = DiagnosticExt {
            code: "E0100",
            severity: type_checker::Severity::Error,
            message: "use of moved value: `a`".into(),
            labels: vec![
                type_checker::SourceLabel {
                    range: rowan::TextRange::new((used as u32).into(), ((used + 1) as u32).into()),
                    message: "value used here after move".into(),
                    style: type_checker::LabelStyle::Primary,
                },
                type_checker::SourceLabel {
                    range: rowan::TextRange::new(
                        (moved as u32).into(),
                        ((moved + 1) as u32).into(),
                    ),
                    message: "value moved here".into(),
                    style: type_checker::LabelStyle::Secondary,
                },
            ],
            help: Some("borrow with `&` to keep using the value".into()),
            notes: vec!["non-Copy values move on assignment".into()],
        };
        let mut output = Vec::new();
        print_rust_style(&mut output, Some(source), "main.rid", &diagnostic, false);
        print_summary(&mut output, 1, false);
        let output = String::from_utf8(output).unwrap();

        assert!(output.starts_with("error[E0100]: use of moved value: `a`\n"));
        assert!(output.contains(" --> main.rid:6:13\n"));
        assert!(output.contains(" 5 |     let b = a;\n   |             - value moved here\n"));
        assert!(
            output.contains(" 6 |     let c = a;\n   |             ^ value used here after move\n")
        );
        assert!(output.contains("  = help: borrow with `&` to keep using the value\n"));
        assert!(output.contains("  = note: non-Copy values move on assignment\n"));
        assert!(output.ends_with("error: aborting due to 1 previous error\n"));
        assert!(!output.contains("\x1b["));

        diagnostic.code = "";
        let mut no_code_output = Vec::new();
        print_rust_style(
            &mut no_code_output,
            Some(source),
            "main.rid",
            &diagnostic,
            false,
        );
        assert!(
            String::from_utf8(no_code_output)
                .unwrap()
                .starts_with("error: ")
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct LineCol {
    line: usize, // 1-based
    col: usize,  // 1-based
}

/// Trim leading and trailing whitespace from source range — CST ranges
/// include leading trivia (newlines, indentation) that inflate the span.
fn trim_range(
    source: &str,
    start: rowan::TextSize,
    end: rowan::TextSize,
) -> (rowan::TextSize, rowan::TextSize) {
    let s: usize = start.into();
    let e: usize = end.into();
    if s >= e || e > source.len() {
        return (start, end);
    }
    let slice = &source[s..e];
    let lead = slice.len() - slice.trim_start_matches(|c: char| c.is_whitespace()).len();
    let trimmed_tail = slice.trim_end_matches(|c: char| c.is_whitespace()).len();
    let tail = slice.len() - trimmed_tail;
    let new_s = s + lead;
    let content_len = slice.len().saturating_sub(lead).saturating_sub(tail);
    let new_e = (new_s + content_len).min(e);
    if new_s >= new_e {
        return (start, end);
    }
    (
        rowan::TextSize::from(new_s as u32),
        rowan::TextSize::from(new_e as u32),
    )
}

/// Skip leading whitespace/newlines in source range — CST ranges include
/// leading trivia that shouldn't be used for the `--> file:line:col` header.
fn trim_leading_trivia(
    source: &str,
    start: rowan::TextSize,
    end: rowan::TextSize,
) -> rowan::TextSize {
    let s: usize = start.into();
    let e: usize = end.into();
    if s >= e {
        return start;
    }
    let slice = &source[s..e];
    let trimmed = slice.len() - slice.trim_start_matches(|c: char| c.is_whitespace()).len();
    rowan::TextSize::from((s + trimmed) as u32)
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
