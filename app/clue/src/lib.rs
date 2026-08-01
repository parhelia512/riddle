mod build;
mod manifest;
mod proc_macro;
mod project;
mod target;

use anyhow::bail;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::SystemTime;

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
    fn load(
        &mut self,
        path: &Path,
        overlays: &HashMap<PathBuf, String>,
    ) -> anyhow::Result<project::LoadedPackage> {
        let normalized_overlays = normalized_overlays(overlays);
        let mut topology_changed = false;
        if let Some(cached) = &self.cached {
            let relevant_overlays =
                relevant_overlays(&normalized_overlays, &cached.package.watched_files);
            if relevant_overlays == cached.overlays
                && file_stamps(&cached.package.watched_files, &normalized_overlays) == cached.disk
            {
                return Ok(cached.package.clone());
            }
            topology_changed = relevant_overlays.keys().ne(cached.overlays.keys());
        }

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

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn inputs_are_current(&self, overlays: &HashMap<PathBuf, String>) -> bool {
        let normalized_overlays = normalized_overlays(overlays);
        self.cached.as_ref().is_some_and(|cached| {
            relevant_overlays(&normalized_overlays, &cached.package.watched_files)
                == cached.overlays
                && file_stamps(&cached.package.watched_files, &normalized_overlays) == cached.disk
        })
    }
}

fn normalized_overlays(overlays: &HashMap<PathBuf, String>) -> HashMap<PathBuf, String> {
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

pub fn analyze_project(
    path: &Path,
    overlays: &HashMap<PathBuf, String>,
) -> anyhow::Result<ProjectAnalysis> {
    analyze_project_with_options(path, overlays, riddlec::pipeline::CompileOptions::default())
}

pub fn analyze_project_with_options(
    path: &Path,
    overlays: &HashMap<PathBuf, String>,
    options: riddlec::pipeline::CompileOptions,
) -> anyhow::Result<ProjectAnalysis> {
    analyze_project_impl(path, overlays, options, true)
}

pub fn check_project_with_options(
    path: &Path,
    overlays: &HashMap<PathBuf, String>,
    options: riddlec::pipeline::CompileOptions,
) -> anyhow::Result<ProjectAnalysis> {
    analyze_project_impl(path, overlays, options, false)
}

pub fn resolve_project_with_session(
    path: &Path,
    overlays: &HashMap<PathBuf, String>,
    options: riddlec::pipeline::CompileOptions,
    session: &mut ProjectSession,
) -> anyhow::Result<ProjectAnalysis> {
    resolve_project_with_session_cancellable(path, overlays, options, session, || false)?
        .ok_or_else(|| anyhow::anyhow!("project analysis cancelled"))
}

pub fn resolve_project_with_session_cancellable(
    path: &Path,
    overlays: &HashMap<PathBuf, String>,
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
            .resolve_parsed_package_with_options_cancellable(
                &package.source.source,
                parse,
                &package.package_ranges,
                options,
                &cancelled,
            )
    } else {
        session.checker.resolve_package_with_options_cancellable(
            &package.source.source,
            &package.package_ranges,
            options,
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
        package_name: package.name,
        manifest_fingerprint: package.manifest_fingerprint,
    }))
}

pub fn check_project_with_session(
    path: &Path,
    overlays: &HashMap<PathBuf, String>,
    options: riddlec::pipeline::CompileOptions,
    session: &mut ProjectSession,
) -> anyhow::Result<ProjectAnalysis> {
    check_project_with_session_cancellable(path, overlays, options, session, || false)?
        .ok_or_else(|| anyhow::anyhow!("project analysis cancelled"))
}

pub fn check_project_with_session_cancellable(
    path: &Path,
    overlays: &HashMap<PathBuf, String>,
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
            .check_parsed_package_with_options_cancellable(
                &package.source.source,
                parse,
                &package.package_ranges,
                options,
                &cancelled,
            )
    } else {
        session.checker.check_package_with_options_cancellable(
            &package.source.source,
            &package.package_ranges,
            options,
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
        package_name: package.name,
        manifest_fingerprint: package.manifest_fingerprint,
    }))
}

fn analyze_project_impl(
    path: &Path,
    overlays: &HashMap<PathBuf, String>,
    options: riddlec::pipeline::CompileOptions,
    build: bool,
) -> anyhow::Result<ProjectAnalysis> {
    let mut package = project::load_with_overlays(path, overlays)?;
    let macro_analysis = expand_proc_macros(&mut package)?;
    let mut result = match (&package.macro_parse, build) {
        (Some(parse), true) => riddlec::pipeline::compile_parsed_package_with_options(
            &package.source.source,
            parse,
            &package.package_ranges,
            options,
        ),
        (Some(parse), false) => riddlec::pipeline::check_parsed_package_with_options(
            &package.source.source,
            parse,
            &package.package_ranges,
            options,
        ),
        (None, true) => riddlec::pipeline::compile_package_with_options(
            &package.source.source,
            &package.package_ranges,
            options,
        ),
        (None, false) => riddlec::pipeline::check_package_with_options(
            &package.source.source,
            &package.package_ranges,
            options,
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

    let offset = rowan::TextSize::from(offset as u32);
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

pub fn check(path: &Path) -> anyhow::Result<()> {
    check_for_target(path, None)
}

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

pub fn build(path: &Path) -> anyhow::Result<()> {
    build_for_target(path, None)
}

pub fn build_for_target(path: &Path, explicit_target: Option<TargetTriple>) -> anyhow::Result<()> {
    build::run(path, explicit_target).map(|_| ())
}

pub fn run(path: &Path, args: &[OsString]) -> anyhow::Result<ExitStatus> {
    run_for_target(path, args, None)
}

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
