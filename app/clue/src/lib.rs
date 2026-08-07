mod build;
mod manifest;
mod proc_macro;
mod project;
mod target;

use anyhow::bail;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::SystemTime;

pub use manifest::CLUE_PROJECT_FILE_NAME;
pub use project::{ProjectKind, init, new};
pub use riddlec::target::TargetTriple;

pub struct ProjectAnalysis {
    pub entry: PathBuf,
    pub source: riddlec::pipeline::LoadedSource,
    pub result: riddlec::pipeline::CompileResult,
    pub macro_occurrences: Vec<riddlec::proc_macro::ProcMacroOccurrence>,
    pub macro_source_map: riddlec::pipeline::SourceMap,
    pub kind: ProjectKind,
    build_target: Option<String>,
    runtime_source: Option<PathBuf>,
    gc_enabled: bool,
    package_name: String,
    manifest_fingerprint: String,
}

#[derive(Default)]
pub struct ProjectSession {
    checker: riddlec::pipeline::CheckSession,
    cached: Option<CachedProject>,
    revision: u64,
}

struct CachedProject {
    package: project::LoadedPackage,
    overlays: BTreeMap<PathBuf, String>,
    disk: BTreeMap<PathBuf, Option<(u64, Option<SystemTime>)>>,
}

impl ProjectSession {
    fn load<S: BuildHasher>(
        &mut self,
        path: &Path,
        overlays: &HashMap<PathBuf, String, S>,
    ) -> anyhow::Result<project::LoadedPackage> {
        let normalized_overlays = normalized_overlays(overlays);
        let topology_changed = if let Some(cached) = &self.cached {
            let relevant_overlays =
                relevant_overlays(&normalized_overlays, &cached.package.watched_files);
            if relevant_overlays == cached.overlays
                && file_stamps(&cached.package.watched_files, &normalized_overlays) == cached.disk
            {
                return Ok(cached.package.clone());
            }
            relevant_overlays.keys().ne(cached.overlays.keys())
        } else {
            false
        };

        let package = project::load_with_overlays(path, overlays)?;
        if topology_changed {
            self.checker = riddlec::pipeline::CheckSession::default();
        }
        self.revision = self.revision.wrapping_add(1).max(1);
        self.cached = Some(CachedProject {
            overlays: relevant_overlays(&normalized_overlays, &package.watched_files),
            disk: file_stamps(&package.watched_files, &normalized_overlays),
            package: package.clone(),
        });
        Ok(package)
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn inputs_are_current<S: BuildHasher>(
        &self,
        overlays: &HashMap<PathBuf, String, S>,
    ) -> bool {
        let normalized_overlays = normalized_overlays(overlays);
        self.cached.as_ref().is_some_and(|cached| {
            relevant_overlays(&normalized_overlays, &cached.package.watched_files)
                == cached.overlays
                && file_stamps(&cached.package.watched_files, &normalized_overlays) == cached.disk
        })
    }
}

fn normalized_overlays<S: BuildHasher>(
    overlays: &HashMap<PathBuf, String, S>,
) -> HashMap<PathBuf, String> {
    overlays
        .iter()
        .map(|(path, source)| (normalized_path(path), source.clone()))
        .collect()
}

fn relevant_overlays(
    overlays: &HashMap<PathBuf, String>,
    files: &[PathBuf],
) -> BTreeMap<PathBuf, String> {
    files
        .iter()
        .filter_map(|path| {
            overlays
                .get(path)
                .map(|source| (path.clone(), source.clone()))
        })
        .collect()
}

fn file_stamps(
    files: &[PathBuf],
    overlays: &HashMap<PathBuf, String>,
) -> BTreeMap<PathBuf, Option<(u64, Option<SystemTime>)>> {
    files
        .iter()
        .filter(|path| !overlays.contains_key(*path))
        .map(|path| {
            let stamp = std::fs::metadata(path)
                .ok()
                .map(|metadata| (metadata.len(), metadata.modified().ok()));
            (path.clone(), stamp)
        })
        .collect()
}

pub fn find_project_root(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };
    start
        .ancestors()
        .find(|path| path.join(manifest::CLUE_PROJECT_FILE_NAME).is_file())
        .map(normalized_path)
}

fn normalized_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        path.parent()
            .and_then(|parent| std::fs::canonicalize(parent).ok())
            .and_then(|parent| path.file_name().map(|name| parent.join(name)))
            .unwrap_or_else(|| path.to_path_buf())
    })
}

/// Analyzes a project using the default compiler options.
///
/// # Errors
///
/// Returns an error when the project cannot be loaded or process macros cannot be expanded.
pub fn analyze_project<S: BuildHasher>(
    path: &Path,
    overlays: &HashMap<PathBuf, String, S>,
) -> anyhow::Result<ProjectAnalysis> {
    analyze_project_with_options(path, overlays, riddlec::pipeline::CompileOptions::default())
}

/// Analyzes a project using explicit compiler options.
///
/// # Errors
///
/// Returns an error when the project cannot be loaded or process macros cannot be expanded.
pub fn analyze_project_with_options<S: BuildHasher>(
    path: &Path,
    overlays: &HashMap<PathBuf, String, S>,
    options: riddlec::pipeline::CompileOptions,
) -> anyhow::Result<ProjectAnalysis> {
    analyze_project_impl(path, overlays, options, true)
}

/// Checks a project without generating code.
///
/// # Errors
///
/// Returns an error when the project cannot be loaded or process macros cannot be expanded.
pub fn check_project_with_options<S: BuildHasher>(
    path: &Path,
    overlays: &HashMap<PathBuf, String, S>,
    options: riddlec::pipeline::CompileOptions,
) -> anyhow::Result<ProjectAnalysis> {
    analyze_project_impl(path, overlays, options, false)
}

/// Resolves a project using an incremental session.
///
/// # Errors
///
/// Returns an error when project loading, macro expansion, or analysis fails.
pub fn resolve_project_with_session<S: BuildHasher>(
    path: &Path,
    overlays: &HashMap<PathBuf, String, S>,
    options: riddlec::pipeline::CompileOptions,
    session: &mut ProjectSession,
) -> anyhow::Result<ProjectAnalysis> {
    resolve_project_with_session_cancellable(path, overlays, options, session, || false)?
        .ok_or_else(|| anyhow::anyhow!("project analysis cancelled"))
}

/// Resolves a project using an incremental session and cancellation callback.
///
/// # Errors
///
/// Returns an error when project loading, macro expansion, or analysis fails.
pub fn resolve_project_with_session_cancellable<S: BuildHasher>(
    path: &Path,
    overlays: &HashMap<PathBuf, String, S>,
    options: riddlec::pipeline::CompileOptions,
    session: &mut ProjectSession,
    cancelled: impl Fn() -> bool,
) -> anyhow::Result<Option<ProjectAnalysis>> {
    if cancelled() {
        return Ok(None);
    }
    let mut package = session.load(path, overlays)?;
    let macro_analysis = expand_proc_macros(&mut package)?;
    if cancelled() {
        return Ok(None);
    }
    let result = if let Some(parse) = &package.macro_parse {
        session
            .checker
            .resolve_parsed_package_with_options_and_gc_cancellable(
                &package.source.source,
                parse,
                &package.package_ranges,
                options,
                package.gc_enabled,
                &cancelled,
            )
    } else {
        session
            .checker
            .resolve_package_with_options_and_gc_cancellable(
                &package.source.source,
                &package.package_ranges,
                options,
                package.gc_enabled,
                &cancelled,
            )
    };
    let Some(mut result) = result else {
        return Ok(None);
    };
    result.macro_diagnostics = macro_analysis.diagnostics;
    Ok(Some(ProjectAnalysis {
        entry: package.entry,
        source: package.source,
        result,
        macro_occurrences: macro_analysis.occurrences,
        macro_source_map: macro_analysis.source_map,
        kind: package.kind,
        build_target: package.build_target,
        runtime_source: package.runtime_source,
        gc_enabled: package.gc_enabled,
        package_name: package.name,
        manifest_fingerprint: package.manifest_fingerprint,
    }))
}

/// Checks a project using an incremental session.
///
/// # Errors
///
/// Returns an error when project loading, macro expansion, or analysis fails.
pub fn check_project_with_session<S: BuildHasher>(
    path: &Path,
    overlays: &HashMap<PathBuf, String, S>,
    options: riddlec::pipeline::CompileOptions,
    session: &mut ProjectSession,
) -> anyhow::Result<ProjectAnalysis> {
    check_project_with_session_cancellable(path, overlays, options, session, || false)?
        .ok_or_else(|| anyhow::anyhow!("project analysis cancelled"))
}

/// Checks a project using an incremental session and cancellation callback.
///
/// # Errors
///
/// Returns an error when project loading, macro expansion, or analysis fails.
pub fn check_project_with_session_cancellable<S: BuildHasher>(
    path: &Path,
    overlays: &HashMap<PathBuf, String, S>,
    options: riddlec::pipeline::CompileOptions,
    session: &mut ProjectSession,
    cancelled: impl Fn() -> bool,
) -> anyhow::Result<Option<ProjectAnalysis>> {
    if cancelled() {
        return Ok(None);
    }
    let mut package = session.load(path, overlays)?;
    let macro_analysis = expand_proc_macros(&mut package)?;
    if cancelled() {
        return Ok(None);
    }
    let result = if let Some(parse) = &package.macro_parse {
        session
            .checker
            .check_parsed_package_with_options_and_gc_cancellable(
                &package.source.source,
                parse,
                &package.package_ranges,
                options,
                package.gc_enabled,
                &cancelled,
            )
    } else {
        session
            .checker
            .check_package_with_options_and_gc_cancellable(
                &package.source.source,
                &package.package_ranges,
                options,
                package.gc_enabled,
                &cancelled,
            )
    };
    let Some(mut result) = result else {
        return Ok(None);
    };
    result.macro_diagnostics = macro_analysis.diagnostics;
    Ok(Some(ProjectAnalysis {
        entry: package.entry,
        source: package.source,
        result,
        macro_occurrences: macro_analysis.occurrences,
        macro_source_map: macro_analysis.source_map,
        kind: package.kind,
        build_target: package.build_target,
        runtime_source: package.runtime_source,
        gc_enabled: package.gc_enabled,
        package_name: package.name,
        manifest_fingerprint: package.manifest_fingerprint,
    }))
}

fn analyze_project_impl<S: BuildHasher>(
    path: &Path,
    overlays: &HashMap<PathBuf, String, S>,
    options: riddlec::pipeline::CompileOptions,
    build: bool,
) -> anyhow::Result<ProjectAnalysis> {
    let mut package = project::load_with_overlays(path, overlays)?;
    let macro_analysis = expand_proc_macros(&mut package)?;
    let mut result = match (&package.macro_parse, build) {
        (Some(parse), true) => riddlec::pipeline::compile_parsed_package_with_options_and_gc(
            &package.source.source,
            parse,
            &package.package_ranges,
            options,
            package.gc_enabled,
        ),
        (Some(parse), false) => riddlec::pipeline::check_parsed_package_with_options_and_gc(
            &package.source.source,
            parse,
            &package.package_ranges,
            options,
            package.gc_enabled,
        ),
        (None, true) => riddlec::pipeline::compile_package_with_options_and_gc(
            &package.source.source,
            &package.package_ranges,
            options,
            package.gc_enabled,
        ),
        (None, false) => riddlec::pipeline::check_package_with_options_and_gc(
            &package.source.source,
            &package.package_ranges,
            options,
            package.gc_enabled,
        ),
    };
    result.macro_diagnostics = macro_analysis.diagnostics;
    Ok(ProjectAnalysis {
        entry: package.entry,
        source: package.source,
        result,
        macro_occurrences: macro_analysis.occurrences,
        macro_source_map: macro_analysis.source_map,
        kind: package.kind,
        build_target: package.build_target,
        runtime_source: package.runtime_source,
        gc_enabled: package.gc_enabled,
        package_name: package.name,
        manifest_fingerprint: package.manifest_fingerprint,
    })
}

struct MacroAnalysis {
    diagnostics: Vec<type_checker::Diagnostic>,
    occurrences: Vec<riddlec::proc_macro::ProcMacroOccurrence>,
    source_map: riddlec::pipeline::SourceMap,
}

fn expand_proc_macros(package: &mut project::LoadedPackage) -> anyhow::Result<MacroAnalysis> {
    let mut provider = proc_macro::ClueProcMacroProvider::build(&package.proc_macros)?;
    let source_map = package.source.source_map.clone();
    let host_exports = (package.kind == ProjectKind::ProcMacro)
        .then(|| proc_macro::discover_exports(&package.source.source))
        .transpose()?;
    let expansion = riddlec::proc_macro::expand_source(&package.source.source, &mut provider);
    let riddlec::proc_macro::ExpandedSource {
        source,
        parse,
        mappings,
        macro_occurrences,
        mut diagnostics,
        ..
    } = expansion;
    remap_package_ranges(&mut package.package_ranges, &mappings);
    package.source.apply_expansion(source, &mappings);
    package.macro_parse = parse;
    if let Some(exports) = host_exports {
        prepare_proc_macro_analysis(package, &exports, &mut diagnostics);
    }
    Ok(MacroAnalysis {
        diagnostics,
        occurrences: macro_occurrences,
        source_map,
    })
}

fn prepare_proc_macro_analysis(
    package: &mut project::LoadedPackage,
    exports: &[proc_macro::HostMacroExport],
    diagnostics: &mut [type_checker::Diagnostic],
) {
    let prefix = proc_macro::host_prefix();
    let suffix = proc_macro::host_suffix(exports);
    let offset = prefix.len() + 1;
    let mut source = String::with_capacity(offset + package.source.source.len() + suffix.len());
    source.push_str(prefix);
    source.push('\n');
    source.push_str(&package.source.source);
    source.push_str(&suffix);

    let mut prefix_parser = frontend::incremental::IncrementalParser::new();
    let prefix_parse = prefix_parser.set_source(prefix).clone();
    let source_parse = package
        .macro_parse
        .as_ref()
        .expect("macro expansion always returns a parse");
    let parse = frontend::tree_builder::append_parse(&prefix_parse, "\n", source_parse);
    let mut suffix_parser = frontend::incremental::IncrementalParser::new();
    let suffix_parse = suffix_parser.set_source(&suffix).clone();
    package.macro_parse = Some(frontend::tree_builder::append_parse(
        &parse,
        "",
        &suffix_parse,
    ));

    let mut shifted_map = riddlec::pipeline::SourceMap::default();
    shifted_map.extend(std::mem::take(&mut package.source.source_map), offset);
    package.source.source_map = shifted_map;
    for range in &mut package.package_ranges {
        range.start += offset;
        range.end += offset;
    }
    if let Some(own_package) = package.package_ranges.last_mut() {
        own_package.start = 0;
        own_package.end = source.len();
    }
    package.source.source = source;

    let offset = rowan::TextSize::from(
        u32::try_from(offset).expect("combined package offset should fit in u32"),
    );
    for diagnostic in diagnostics {
        for label in &mut diagnostic.labels {
            label.range =
                rowan::TextRange::new(label.range.start() + offset, label.range.end() + offset);
        }
    }
}

fn remap_package_ranges(
    ranges: &mut [std::ops::Range<usize>],
    mappings: &[riddlec::proc_macro::ExpandedTokenMapping],
) {
    for range in ranges {
        let generated = mappings
            .iter()
            .filter(|mapping| {
                mapping.original.start < range.end && range.start < mapping.original.end
            })
            .map(|mapping| mapping.generated.clone())
            .collect::<Vec<_>>();
        if let (Some(start), Some(end)) = (
            generated.iter().map(|range| range.start).min(),
            generated.iter().map(|range| range.end).max(),
        ) {
            *range = start..end;
        } else {
            let anchor = mappings
                .iter()
                .filter(|mapping| mapping.original.end <= range.start)
                .map(|mapping| mapping.generated.end)
                .max()
                .unwrap_or(0);
            *range = anchor..anchor;
        }
    }
}

/// Checks a package for the host target.
///
/// # Errors
///
/// Returns an error when loading, target resolution, or compilation fails.
pub fn check(path: &Path) -> anyhow::Result<()> {
    check_for_target(path, None)
}

/// Checks a package for an optional explicit target.
///
/// # Errors
///
/// Returns an error when loading, target resolution, or compilation fails.
pub fn check_for_target(path: &Path, explicit_target: Option<TargetTriple>) -> anyhow::Result<()> {
    let analysis = check_project_with_options(
        path,
        &HashMap::new(),
        riddlec::pipeline::CompileOptions::default(),
    )?;
    target::resolve(explicit_target, analysis.build_target.as_deref())?;
    let errors = riddlec::diagnostics::report_mapped(
        &analysis.result,
        &analysis.source,
        &analysis.entry.display().to_string(),
    );
    if errors > 0 || !analysis.result.success() {
        bail!("check failed");
    }
    println!("clue: checked {}", path.display());
    Ok(())
}

/// Builds a package for the host target.
///
/// # Errors
///
/// Returns an error when project analysis, code generation, or linking fails.
pub fn build(path: &Path) -> anyhow::Result<()> {
    build_for_target(path, None)
}

/// Builds a package for an optional explicit target.
///
/// # Errors
///
/// Returns an error when project analysis, code generation, or linking fails.
pub fn build_for_target(path: &Path, explicit_target: Option<TargetTriple>) -> anyhow::Result<()> {
    build::run(path, explicit_target).map(|_| ())
}

/// Builds and runs a package for the host target.
///
/// # Errors
///
/// Returns an error when the package cannot be built or the executable cannot be started.
pub fn run(path: &Path, args: &[OsString]) -> anyhow::Result<ExitStatus> {
    run_for_target(path, args, None)
}

/// Builds and runs a package for an optional explicit target.
///
/// # Errors
///
/// Returns an error when the package cannot be built, cannot run on the host, or cannot start.
pub fn run_for_target(
    path: &Path,
    args: &[OsString],
    explicit_target: Option<TargetTriple>,
) -> anyhow::Result<ExitStatus> {
    let artifact = build::run(path, explicit_target)?;
    let build::BuildArtifact::Executable {
        path: executable,
        target,
    } = artifact
    else {
        bail!("cannot run a library package");
    };
    if target != TargetTriple::host().map_err(anyhow::Error::msg)? {
        bail!(
            "cannot run target `{target}` on host `{}`; run the built program on the target system",
            TargetTriple::host().map_err(anyhow::Error::msg)?
        );
    }
    Command::new(&executable)
        .args(args)
        .current_dir(path)
        .status()
        .map_err(|error| anyhow::anyhow!("failed to run `{}`: {error}", executable.display()))
}
