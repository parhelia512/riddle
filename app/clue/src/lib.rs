mod build;
mod manifest;
mod project;

use anyhow::bail;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use project::{ProjectKind, init, new};

pub struct ProjectAnalysis {
    pub entry: PathBuf,
    pub source: riddlec::pipeline::LoadedSource,
    pub result: riddlec::pipeline::CompileResult,
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
    let package = project::load_with_overlays(path, overlays)?;
    let result = riddlec::pipeline::compile_with_options(&package.source.source, options);
    Ok(ProjectAnalysis {
        entry: package.entry,
        source: package.source,
        result,
        package_name: package.name,
        manifest_fingerprint: package.manifest_fingerprint,
    })
}

pub fn check(path: &Path) -> anyhow::Result<()> {
    let analysis = analyze_project(path, &HashMap::new())?;
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
    build::run(path)
}
