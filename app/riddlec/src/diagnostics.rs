use crate::pipeline::CompileResult;

pub fn report(result: &CompileResult, source_name: &str) -> usize {
    let mut count = 0;

    for e in &result.parse_errors {
        eprintln!("{source_name}: parse error: {e}");
        count += 1;
    }

    for d in &result.type_result.diagnostics {
        eprintln!("{source_name}: type error: {}", d.message);
        count += 1;
    }

    for d in &result.move_diagnostics {
        eprintln!("{source_name}: move error: {}", d.message);
        count += 1;
    }

    count
}

pub fn report_verbose(result: &CompileResult) {
    if result.parse_errors.is_empty() {
        println!("parse: ok");
    }

    if result.type_result.diagnostics.is_empty() {
        println!("type check: ok");
    }

    if result.move_diagnostics.is_empty() {
        println!("move check: ok");
    }

    if !result.success() {
        println!();
        report(result, "<source>");
    }
}
