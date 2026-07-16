use rowan::TextRange;

#[allow(dead_code)]
pub fn assert_type_diagnostics(source: &str, diagnostics: &[type_checker::Diagnostic]) {
    for diagnostic in diagnostics {
        assert_labels(
            source,
            diagnostic.code,
            &diagnostic.message,
            diagnostic.labels.iter().map(|label| {
                (
                    label.range,
                    matches!(label.style, type_checker::LabelStyle::Primary),
                )
            }),
        );
    }
}

pub fn assert_hir_diagnostics<'a>(
    source: &str,
    diagnostics: impl IntoIterator<Item = &'a hir::body::Diagnostic>,
) {
    for diagnostic in diagnostics {
        assert_labels(
            source,
            diagnostic.code,
            &diagnostic.message,
            diagnostic.labels.iter().map(|label| {
                (
                    label.range,
                    matches!(label.style, hir::body::LabelStyle::Primary),
                )
            }),
        );
    }
}

fn assert_labels(
    source: &str,
    code: &str,
    message: &str,
    labels: impl IntoIterator<Item = (TextRange, bool)>,
) {
    let labels = labels.into_iter().collect::<Vec<_>>();
    let primary_count = labels.iter().filter(|(_, primary)| *primary).count();
    assert_eq!(
        primary_count, 1,
        "diagnostic {code} ({message}) must have exactly one primary label: {labels:?}"
    );

    for (range, _) in labels {
        let start = usize::from(range.start());
        let end = usize::from(range.end());
        assert!(
            start <= end && end <= source.len(),
            "diagnostic {code} ({message}) has out-of-bounds range {range:?} for {} bytes",
            source.len()
        );
        assert!(
            source.is_char_boundary(start) && source.is_char_boundary(end),
            "diagnostic {code} ({message}) range {range:?} splits a UTF-8 character"
        );
        assert!(
            start == end || !source[start..end].trim().is_empty(),
            "diagnostic {code} ({message}) range {range:?} covers only whitespace"
        );
    }
}
