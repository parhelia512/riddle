mod build;
mod manifest;
mod project;

use anyhow::bail;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

pub use project::{ProjectKind, init, new};

pub struct ProjectAnalysis {
    pub entry: PathBuf,
    pub source: riddlec::pipeline::LoadedSource,
    pub result: riddlec::pipeline::CompileResult,
    pub kind: ProjectKind,
    runtime_source: Option<PathBuf>,
    package_name: String,
    manifest_fingerprint: String,
}

pub fn find_project_root(path: &Path) -> Option<PathBuf> {
    let path = std::fs::canonicalize(path).ok()?;
    let start = if path.is_dir() {
        path.as_path()
    } else {
        path.parent()?
    };
    start
        .ancestors()
        .find(|path| path.join(manifest::CLUE_PROJECT_FILE_NAME).is_file())
        .map(Path::to_path_buf)
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

pub fn check_project_with_session(
    path: &Path,
    overlays: &HashMap<PathBuf, String>,
    options: riddlec::pipeline::CompileOptions,
    session: &mut riddlec::pipeline::CheckSession,
) -> anyhow::Result<ProjectAnalysis> {
    let package = project::load_with_overlays(path, overlays)?;
    let result = session.check_package_with_options(
        &package.source.source,
        &package.package_ranges,
        options,
    );
    Ok(ProjectAnalysis {
        entry: package.entry,
        source: package.source,
        result,
        kind: package.kind,
        runtime_source: package.runtime_source,
        package_name: package.name,
        manifest_fingerprint: package.manifest_fingerprint,
    })
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
        runtime_source: package.runtime_source,
        package_name: package.name,
        manifest_fingerprint: package.manifest_fingerprint,
    })
}

pub fn check(path: &Path) -> anyhow::Result<()> {
    let analysis = check_project_with_options(
        path,
        &HashMap::new(),
        riddlec::pipeline::CompileOptions::default(),
    )?;
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
    build::run(path).map(|_| ())
}

pub fn run(path: &Path, args: &[OsString]) -> anyhow::Result<ExitStatus> {
    let artifact = build::run(path)?;
    let build::BuildArtifact::Executable(executable) = artifact else {
        bail!("cannot run a library package");
    };
    Command::new(&executable)
        .args(args)
        .current_dir(path)
        .status()
        .map_err(|error| anyhow::anyhow!("failed to run `{}`: {error}", executable.display()))
}
