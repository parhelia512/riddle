use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use lsp_types::{
    CodeDescription, Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location,
    NumberOrString, Position, Range, Url,
};
use riddlec::pipeline::{
    CheckSession, CompileOptions, CompileResult, DiagnosticExt, IntoDiagnosticExt, SourceMap,
};
use rowan::{TextRange, TextSize};
use type_checker::{LabelStyle, SourceLabel};

use super::Document;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PublishedDiagnostics {
    pub(crate) uri: Url,
    pub(crate) version: Option<i32>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

struct ResolvedLabel {
    uri: Url,
    range: Range,
}

struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(offset, byte)| (byte == b'\n').then_some(offset + 1)),
        );
        Self { starts }
    }

    fn position(&self, source: &str, offset: usize) -> Option<Position> {
        if offset > source.len() || !source.is_char_boundary(offset) {
            return None;
        }
        let line = self.starts.partition_point(|start| *start <= offset) - 1;
        let character = source[self.starts[line]..offset]
            .chars()
            .map(char::len_utf16)
            .sum::<usize>();
        Some(Position::new(line as u32, character as u32))
    }

    fn range(&self, source: &str, range: TextRange) -> Option<Range> {
        Some(Range::new(
            self.position(source, usize::from(range.start()))?,
            self.position(source, usize::from(range.end()))?,
        ))
    }
}

#[derive(Default)]
pub(crate) struct DiagnosticSessions {
    standalone: HashMap<Url, StandaloneDiagnosticSession>,
    projects: HashMap<PathBuf, ProjectDiagnosticSession>,
}

#[derive(Default)]
struct StandaloneDiagnosticSession {
    checker: CheckSession,
    cached: Option<CachedStandaloneDiagnostics>,
    #[cfg(test)]
    checks: usize,
}

struct CachedStandaloneDiagnostics {
    options: CompileOptions,
    source: String,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Default)]
struct ProjectDiagnosticSession {
    checker: CheckSession,
    cached: Option<CachedProjectDiagnostics>,
    #[cfg(test)]
    checks: usize,
}

struct CachedProjectDiagnostics {
    options: CompileOptions,
    inputs: ProjectInputs,
    files: HashSet<PathBuf>,
    diagnostics: BTreeMap<Url, Vec<Diagnostic>>,
}

struct ProjectDiagnostics {
    by_uri: BTreeMap<Url, Vec<Diagnostic>>,
    files: HashSet<PathBuf>,
}

#[derive(PartialEq, Eq)]
struct ProjectInputs {
    overlays: BTreeMap<PathBuf, String>,
    disk: BTreeMap<PathBuf, Option<(u64, Option<SystemTime>)>>,
}

impl DiagnosticSessions {
    #[cfg(test)]
    pub(crate) fn check_counts(&self) -> (usize, usize) {
        (
            self.standalone.values().map(|session| session.checks).sum(),
            self.projects.values().map(|session| session.checks).sum(),
        )
    }
}

#[cfg(test)]
pub(crate) fn collect_workspace_diagnostics(
    docs: &HashMap<Url, Document>,
    options: CompileOptions,
) -> Vec<PublishedDiagnostics> {
    collect_workspace_diagnostics_with_sessions(docs, options, &mut DiagnosticSessions::default())
}

#[cfg(test)]
pub(crate) fn collect_workspace_diagnostics_with_sessions(
    docs: &HashMap<Url, Document>,
    options: CompileOptions,
    sessions: &mut DiagnosticSessions,
) -> Vec<PublishedDiagnostics> {
    collect_workspace_diagnostics_cancellable(docs, options, sessions, || false)
        .expect("non-cancellable analysis cannot be cancelled")
}

pub(crate) fn collect_workspace_diagnostics_cancellable(
    docs: &HashMap<Url, Document>,
    options: CompileOptions,
    sessions: &mut DiagnosticSessions,
    cancelled: impl Fn() -> bool,
) -> Option<Vec<PublishedDiagnostics>> {
    let overlays = docs
        .iter()
        .filter_map(|(uri, document)| {
            uri.to_file_path()
                .ok()
                .map(|path| (path, document.text.clone()))
        })
        .collect::<HashMap<PathBuf, String>>();
    let mut projects = BTreeMap::<PathBuf, Vec<Url>>::new();
    let mut standalone = Vec::new();

    for uri in docs.keys() {
        let Ok(path) = uri.to_file_path() else {
            standalone.push(uri.clone());
            continue;
        };
        if let Some(root) = clue::find_project_root(&path) {
            projects.entry(root).or_default().push(uri.clone());
        } else {
            standalone.push(uri.clone());
        }
    }
    standalone.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    for project_docs in projects.values_mut() {
        project_docs.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    }

    let mut by_uri = BTreeMap::<Url, Vec<Diagnostic>>::new();
    let mut live_standalone = HashSet::new();
    let mut live_projects = HashSet::new();
    for uri in standalone {
        if cancelled() {
            return None;
        }
        let Some(document) = docs.get(&uri) else {
            continue;
        };
        live_standalone.insert(uri.clone());
        let diagnostics = standalone_diagnostics(
            &uri,
            document,
            options,
            sessions.standalone.entry(uri.clone()).or_default(),
        );
        by_uri.entry(uri.clone()).or_default().extend(diagnostics);
    }

    for (root, project_docs) in projects {
        if cancelled() {
            return None;
        }
        live_projects.insert(root.clone());
        for uri in &project_docs {
            by_uri.entry(uri.clone()).or_default();
        }

        let project_diagnostics = match project_diagnostics(
            &root,
            &overlays,
            options,
            sessions.projects.entry(root.clone()).or_default(),
        ) {
            Ok(cached) => cached,
            Err(error) => {
                let manifest = root.join("Clue.toml");
                let uri = Url::from_file_path(&manifest)
                    .ok()
                    .map(|uri| published_uri(&uri, docs))
                    .or_else(|| project_docs.first().cloned());
                if let Some(uri) = uri {
                    let source = fs::read_to_string(&manifest).unwrap_or_default();
                    by_uri
                        .entry(uri.clone())
                        .or_default()
                        .push(project_error(&source, error));
                }
                continue;
            }
        };
        if cancelled() {
            return None;
        }

        for (uri, mut diagnostics) in project_diagnostics.by_uri {
            let uri = published_uri(&uri, docs);
            for related in diagnostics
                .iter_mut()
                .flat_map(|diagnostic| diagnostic.related_information.iter_mut().flatten())
            {
                related.location.uri = published_uri(&related.location.uri, docs);
            }
            by_uri.entry(uri).or_default().extend(diagnostics);
        }

        for uri in project_docs {
            let Some(document) = docs.get(&uri) else {
                continue;
            };
            let Ok(path) = uri.to_file_path() else {
                continue;
            };
            let Ok(path) = fs::canonicalize(path) else {
                continue;
            };
            if project_diagnostics.files.contains(&path) {
                continue;
            }
            live_standalone.insert(uri.clone());
            let diagnostics = standalone_diagnostics(
                &uri,
                document,
                options,
                sessions.standalone.entry(uri.clone()).or_default(),
            );
            by_uri.entry(uri.clone()).or_default().extend(diagnostics);
        }
    }

    sessions
        .standalone
        .retain(|uri, _| live_standalone.contains(uri));
    sessions
        .projects
        .retain(|root, _| live_projects.contains(root));

    Some(
        by_uri
            .into_iter()
            .map(|(uri, mut diagnostics)| {
                sort_and_dedup(&mut diagnostics);
                PublishedDiagnostics {
                    version: document_version(&uri, docs),
                    uri,
                    diagnostics,
                }
            })
            .collect(),
    )
}

fn standalone_diagnostics(
    uri: &Url,
    document: &Document,
    options: CompileOptions,
    session: &mut StandaloneDiagnosticSession,
) -> Vec<Diagnostic> {
    if let Some(cached) = &session.cached
        && cached.options == options
        && cached.source == document.text
    {
        return cached.diagnostics.clone();
    }

    let result = session.checker.check_with_options(&document.text, options);
    let diagnostics = collect_diagnostics(uri, &document.text, &result);
    session.cached = Some(CachedStandaloneDiagnostics {
        options,
        source: document.text.clone(),
        diagnostics: diagnostics.clone(),
    });
    #[cfg(test)]
    {
        session.checks += 1;
    }
    diagnostics
}

fn project_diagnostics(
    root: &Path,
    overlays: &HashMap<PathBuf, String>,
    options: CompileOptions,
    session: &mut ProjectDiagnosticSession,
) -> Result<ProjectDiagnostics, String> {
    if let Some(cached) = &session.cached {
        let inputs = project_inputs(root, overlays, &cached.files);
        if cached.options == options && inputs == cached.inputs {
            return Ok(ProjectDiagnostics {
                by_uri: cached.diagnostics.clone(),
                files: cached.files.clone(),
            });
        }
    }

    let analysis = clue::check_project_with_session(root, overlays, options, &mut session.checker)
        .map_err(|error| error.to_string())?;
    let files = analysis
        .source
        .files
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let diagnostics = collect_mapped_diagnostics(&analysis.source.source_map, &analysis.result);
    let inputs = project_inputs(root, overlays, &files);
    session.cached = Some(CachedProjectDiagnostics {
        options,
        inputs,
        files: files.clone(),
        diagnostics: diagnostics.clone(),
    });
    #[cfg(test)]
    {
        session.checks += 1;
    }
    Ok(ProjectDiagnostics {
        by_uri: diagnostics,
        files,
    })
}

fn project_inputs(
    root: &Path,
    overlays: &HashMap<PathBuf, String>,
    files: &HashSet<PathBuf>,
) -> ProjectInputs {
    let overlays = overlays
        .iter()
        .filter_map(|(path, source)| {
            let path = normalized_path(path.clone());
            files.contains(&path).then(|| (path, source.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut watched_files = files.clone();
    watched_files.insert(normalized_path(root.join("Clue.toml")));
    for file in files {
        if let Some(project_root) = clue::find_project_root(file) {
            watched_files.insert(normalized_path(project_root.join("Clue.toml")));
        }
    }
    let disk = watched_files
        .into_iter()
        .filter(|path| !overlays.contains_key(path))
        .map(|path| {
            let stamp = fs::metadata(&path)
                .ok()
                .map(|metadata| (metadata.len(), metadata.modified().ok()));
            (path, stamp)
        })
        .collect();
    ProjectInputs { overlays, disk }
}

fn normalized_path(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

#[cfg(test)]
pub(crate) fn collect_document_diagnostics(
    uri: &Url,
    _source: &str,
    docs: &HashMap<Url, Document>,
    options: CompileOptions,
) -> Vec<Diagnostic> {
    let target = published_uri(uri, docs);
    collect_workspace_diagnostics(docs, options)
        .into_iter()
        .find(|published| published.uri == target)
        .map(|published| published.diagnostics)
        .unwrap_or_default()
}

pub(crate) fn collect_diagnostics(
    uri: &Url,
    source: &str,
    result: &CompileResult,
) -> Vec<Diagnostic> {
    let line_index = LineIndex::new(source);
    let mut diagnostics = diagnostic_exts(result)
        .filter_map(|diagnostic| to_lsp_with_index(uri, source, &line_index, diagnostic))
        .collect::<Vec<_>>();
    sort_and_dedup(&mut diagnostics);
    diagnostics
}

fn collect_mapped_diagnostics(
    source_map: &SourceMap,
    result: &CompileResult,
) -> BTreeMap<Url, Vec<Diagnostic>> {
    let mut by_uri = BTreeMap::<Url, Vec<Diagnostic>>::new();
    let mut line_indexes = HashMap::new();
    for diagnostic in diagnostic_exts(result) {
        let Some((uri, diagnostic)) =
            to_lsp_mapped_with_indexes(source_map, &mut line_indexes, diagnostic)
        else {
            continue;
        };
        by_uri.entry(uri).or_default().push(diagnostic);
    }
    for diagnostics in by_uri.values_mut() {
        sort_and_dedup(diagnostics);
    }
    by_uri
}

fn diagnostic_exts(result: &CompileResult) -> impl Iterator<Item = DiagnosticExt> + '_ {
    result
        .parse_errors
        .iter()
        .map(IntoDiagnosticExt::to_ext)
        .chain(result.hir_diagnostics.iter().map(IntoDiagnosticExt::to_ext))
        .chain(
            result
                .type_result
                .diagnostics
                .iter()
                .map(IntoDiagnosticExt::to_ext),
        )
        .chain(
            result
                .analysis_diagnostics
                .iter()
                .map(IntoDiagnosticExt::to_ext),
        )
}

#[cfg(test)]
pub(crate) fn to_lsp(uri: &Url, source: &str, diagnostic: DiagnosticExt) -> Option<Diagnostic> {
    to_lsp_with_index(uri, source, &LineIndex::new(source), diagnostic)
}

fn to_lsp_with_index(
    uri: &Url,
    source: &str,
    line_index: &LineIndex,
    diagnostic: DiagnosticExt,
) -> Option<Diagnostic> {
    convert_diagnostic(diagnostic, |label| {
        Some(ResolvedLabel {
            uri: uri.clone(),
            range: line_index.range(source, normalize_range(source, label.range)?)?,
        })
    })
    .map(|(_, diagnostic)| diagnostic)
}

#[cfg(test)]
pub(crate) fn to_lsp_mapped(
    source_map: &SourceMap,
    diagnostic: DiagnosticExt,
) -> Option<(Url, Diagnostic)> {
    to_lsp_mapped_with_indexes(source_map, &mut HashMap::new(), diagnostic)
}

fn to_lsp_mapped_with_indexes(
    source_map: &SourceMap,
    line_indexes: &mut HashMap<PathBuf, LineIndex>,
    diagnostic: DiagnosticExt,
) -> Option<(Url, Diagnostic)> {
    convert_diagnostic(diagnostic, |label| {
        let mapped = source_map.map_range(label.range)?;
        let path = mapped.path.to_path_buf();
        let line_index = line_indexes
            .entry(path.clone())
            .or_insert_with(|| LineIndex::new(mapped.source));
        Some(ResolvedLabel {
            uri: Url::from_file_path(path).ok()?,
            range: line_index
                .range(mapped.source, normalize_range(mapped.source, mapped.range)?)?,
        })
    })
}

fn convert_diagnostic(
    diagnostic: DiagnosticExt,
    mut resolve: impl FnMut(&SourceLabel) -> Option<ResolvedLabel>,
) -> Option<(Url, Diagnostic)> {
    let primary_index = diagnostic
        .labels
        .iter()
        .position(|label| label.style == LabelStyle::Primary)?;
    let mut resolved = diagnostic
        .labels
        .iter()
        .map(&mut resolve)
        .collect::<Vec<_>>();
    let primary = resolved.get_mut(primary_index)?.take()?;

    let mut message = diagnostic.message;
    let primary_message = diagnostic.labels[primary_index].message.trim();
    if !primary_message.is_empty() {
        message.push('\n');
        message.push_str(primary_message);
    }
    if let Some(help) = diagnostic.help {
        message.push_str("\nhelp: ");
        message.push_str(&help);
    }
    for note in diagnostic.notes {
        message.push_str("\nnote: ");
        message.push_str(&note);
    }

    let mut related_information = resolved
        .into_iter()
        .enumerate()
        .filter(|(index, _)| *index != primary_index)
        .filter_map(|(index, location)| {
            let location = location?;
            let label = &diagnostic.labels[index];
            Some(DiagnosticRelatedInformation {
                location: Location::new(location.uri, location.range),
                message: if label.message.trim().is_empty() {
                    "related location".into()
                } else {
                    label.message.clone()
                },
            })
        })
        .collect::<Vec<_>>();
    related_information.sort_by(compare_related);
    related_information.dedup();

    let uri = primary.uri;
    let code =
        (!diagnostic.code.is_empty()).then(|| NumberOrString::String(diagnostic.code.into()));
    Some((
        uri,
        Diagnostic {
            range: primary.range,
            severity: Some(severity(diagnostic.severity)),
            code,
            code_description: code_description(diagnostic.code),
            source: Some("riddle".into()),
            message,
            related_information: (!related_information.is_empty()).then_some(related_information),
            ..Diagnostic::default()
        },
    ))
}

fn code_description(code: &str) -> Option<CodeDescription> {
    (!code.is_empty()).then(|| CodeDescription {
        href: Url::parse(&format!(
            "https://riddle-lang.github.io/docs/errorcode.html#{}",
            code.to_ascii_lowercase()
        ))
        .expect("diagnostic code must form a valid documentation URL"),
    })
}

fn project_error(source: &str, error: impl std::fmt::Display) -> Diagnostic {
    Diagnostic {
        range: anchor_range(source),
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String("CLUE0001".into())),
        source: Some("clue".into()),
        message: error.to_string(),
        ..Diagnostic::default()
    }
}

fn anchor_range(source: &str) -> Range {
    let start = source
        .char_indices()
        .find_map(|(offset, ch)| (!ch.is_whitespace()).then_some(offset))
        .unwrap_or(0);
    let end = source[start..]
        .find(['\r', '\n'])
        .map(|offset| start + offset)
        .unwrap_or(source.len());
    let range = TextRange::new(TextSize::from(start as u32), TextSize::from(end as u32));
    try_range(source, normalize_range(source, range).unwrap_or(range)).unwrap_or_default()
}

fn normalize_range(source: &str, range: TextRange) -> Option<TextRange> {
    let start = usize::from(range.start());
    let end = usize::from(range.end());
    if start > end
        || end > source.len()
        || !source.is_char_boundary(start)
        || !source.is_char_boundary(end)
    {
        return None;
    }
    if start == end {
        return Some(range);
    }

    let text = source.get(start..end)?;
    let trimmed_start = start + text.len() - text.trim_start().len();
    let trimmed_end = end - (text.len() - text.trim_end().len());
    if trimmed_start >= trimmed_end {
        return Some(TextRange::empty(TextSize::from(trimmed_start as u32)));
    }
    Some(TextRange::new(
        TextSize::from(trimmed_start as u32),
        TextSize::from(trimmed_end as u32),
    ))
}

fn try_range(source: &str, range: TextRange) -> Option<Range> {
    LineIndex::new(source).range(source, range)
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn range(source: &str, range: TextRange) -> Range {
    try_range(source, normalize_range(source, range).unwrap_or(range)).unwrap_or_default()
}

fn try_position(source: &str, offset: usize) -> Option<Position> {
    LineIndex::new(source).position(source, offset)
}

pub(crate) fn position(source: &str, offset: usize) -> Position {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    try_position(source, offset).unwrap_or_default()
}

fn severity(severity: type_checker::Severity) -> DiagnosticSeverity {
    match severity {
        type_checker::Severity::Error => DiagnosticSeverity::ERROR,
        type_checker::Severity::Warning => DiagnosticSeverity::WARNING,
        type_checker::Severity::Note => DiagnosticSeverity::INFORMATION,
        type_checker::Severity::Help => DiagnosticSeverity::HINT,
    }
}

fn normalized_uri(uri: &Url) -> Url {
    uri.to_file_path()
        .ok()
        .and_then(|path| fs::canonicalize(path).ok())
        .and_then(|path| Url::from_file_path(path).ok())
        .unwrap_or_else(|| uri.clone())
}

fn published_uri(uri: &Url, docs: &HashMap<Url, Document>) -> Url {
    if docs.contains_key(uri) {
        return uri.clone();
    }

    let normalized = normalized_uri(uri);
    docs.keys()
        .filter(|candidate| normalized_uri(candidate) == normalized)
        .min_by(|left, right| left.as_str().cmp(right.as_str()))
        .cloned()
        .unwrap_or(normalized)
}

fn document_version(uri: &Url, docs: &HashMap<Url, Document>) -> Option<i32> {
    docs.get(&published_uri(uri, docs))
        .and_then(|document| document.version)
}

fn sort_and_dedup(diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.sort_by(compare_diagnostics);
    diagnostics.dedup();
}

fn compare_diagnostics(left: &Diagnostic, right: &Diagnostic) -> Ordering {
    diagnostic_range_key(left)
        .cmp(&diagnostic_range_key(right))
        .then_with(|| severity_key(left.severity).cmp(&severity_key(right.severity)))
        .then_with(|| code_key(left.code.as_ref()).cmp(&code_key(right.code.as_ref())))
        .then_with(|| left.source.cmp(&right.source))
        .then_with(|| left.message.cmp(&right.message))
        .then_with(|| related_key(left).cmp(&related_key(right)))
}

fn compare_related(
    left: &DiagnosticRelatedInformation,
    right: &DiagnosticRelatedInformation,
) -> Ordering {
    related_location_key(left)
        .cmp(&related_location_key(right))
        .then_with(|| left.message.cmp(&right.message))
}

fn diagnostic_range_key(diagnostic: &Diagnostic) -> (u32, u32, u32, u32) {
    let range = diagnostic.range;
    (
        range.start.line,
        range.start.character,
        range.end.line,
        range.end.character,
    )
}

fn related_location_key(related: &DiagnosticRelatedInformation) -> (String, u32, u32, u32, u32) {
    let range = related.location.range;
    (
        related.location.uri.as_str().to_owned(),
        range.start.line,
        range.start.character,
        range.end.line,
        range.end.character,
    )
}

fn related_key(diagnostic: &Diagnostic) -> Vec<(String, u32, u32, u32, u32, String)> {
    diagnostic
        .related_information
        .iter()
        .flatten()
        .map(|related| {
            let (uri, start_line, start_character, end_line, end_character) =
                related_location_key(related);
            (
                uri,
                start_line,
                start_character,
                end_line,
                end_character,
                related.message.clone(),
            )
        })
        .collect()
}

fn severity_key(severity: Option<DiagnosticSeverity>) -> u8 {
    match severity {
        Some(DiagnosticSeverity::ERROR) => 0,
        Some(DiagnosticSeverity::WARNING) => 1,
        Some(DiagnosticSeverity::INFORMATION) => 2,
        Some(DiagnosticSeverity::HINT) => 3,
        _ => 4,
    }
}

fn code_key(code: Option<&NumberOrString>) -> (u8, String) {
    match code {
        Some(NumberOrString::Number(number)) => (0, number.to_string()),
        Some(NumberOrString::String(code)) => (1, code.clone()),
        None => (2, String::new()),
    }
}
