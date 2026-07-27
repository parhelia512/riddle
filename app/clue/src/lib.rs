mod build;
mod manifest;
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
        let normalized_overlays = overlays
            .iter()
            .map(|(path, source)| (normalized_path(path), source.clone()))
            .collect::<HashMap<_, _>>();
        let mut topology_changed = false;
        if let Some(cached) = &self.cached {
            let relevant_overlays =
                relevant_overlays(&normalized_overlays, &cached.package.source.files);
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
        self.cached = Some(CachedProject {
            overlays: relevant_overlays(&normalized_overlays, &package.source.files),
            disk: file_stamps(&package.watched_files, &normalized_overlays),
            package: package.clone(),
        });
        Ok(package)
    }
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
    let package = session.load(path, overlays)?;
    if cancelled() {
        return Ok(None);
    }
    let Some(result) = session.checker.resolve_package_with_options_cancellable(
        &package.source.source,
        &package.package_ranges,
        options,
        cancelled,
    ) else {
        return Ok(None);
    };
    Ok(Some(ProjectAnalysis {
        entry: package.entry,
        source: package.source,
        result,
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
    let package = session.load(path, overlays)?;
    if cancelled() {
        return Ok(None);
    }
    let Some(result) = session.checker.check_package_with_options_cancellable(
        &package.source.source,
        &package.package_ranges,
        options,
        cancelled,
    ) else {
        return Ok(None);
    };
    Ok(Some(ProjectAnalysis {
        entry: package.entry,
        source: package.source,
        result,
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
    let package = project::load_with_overlays(path, overlays)?;
    let result = if build {
        riddlec::pipeline::compile_package_with_options(
            &package.source.source,
            &package.package_ranges,
            options,
        )
    } else {
        riddlec::pipeline::check_package_with_options(
            &package.source.source,
            &package.package_ranges,
            options,
        )
    };
    Ok(ProjectAnalysis {
        entry: package.entry,
        source: package.source,
        result,
        kind: package.kind,
        build_target: package.build_target,
        runtime_source: package.runtime_source,
        package_name: package.name,
        manifest_fingerprint: package.manifest_fingerprint,
    })
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
