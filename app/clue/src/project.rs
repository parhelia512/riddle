use crate::manifest::{self, CLUE_PROJECT_FILE_NAME};
use anyhow::{Context, bail};
use riddlec::pipeline;
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Error, ErrorKind, Write};
use std::path::{Path, PathBuf};

const DEFAULT_MAIN: &str = "fun main() {\n}\n";
const DEFAULT_LIB: &str = "pub fun add(x: i32, y: i32) -> i32 {\n    x + y\n}\n";

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ProjectKind {
    Binary,
    Library,
}

pub fn init(path: &Path, kind: ProjectKind) -> anyhow::Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create `{}`", path.display()))?;
    create(path, kind)
}

pub fn new(path: &Path, kind: ProjectKind) -> anyhow::Result<()> {
    if path.exists() {
        bail!("destination `{}` already exists", path.display());
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create `{}`", path.display()))?;
    create(path, kind)
}

fn create(path: &Path, kind: ProjectKind) -> anyhow::Result<()> {
    let root = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve `{}`", path.display()))?;
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("project path must end with a valid UTF-8 name"))?;
    manifest::validate_package_name(name)?;

    let source_path = root.join("src").join(match kind {
        ProjectKind::Binary => "main.rid",
        ProjectKind::Library => "lib.rid",
    });
    let manifest_path = root.join(CLUE_PROJECT_FILE_NAME);
    if manifest_path.exists() || source_path.exists() {
        bail!("refusing to overwrite files in `{}`", root.display());
    }

    fs::create_dir_all(root.join("src"))?;
    write_new(
        &source_path,
        match kind {
            ProjectKind::Binary => DEFAULT_MAIN,
            ProjectKind::Library => DEFAULT_LIB,
        },
    )?;
    write_new(&manifest_path, &manifest::new_manifest(name, kind))?;
    update_gitignore(&root.join(".gitignore"))?;
    Ok(())
}

fn write_new(path: &Path, content: &str) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("failed to create `{}`", path.display()))?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

fn update_gitignore(path: &Path) -> anyhow::Result<()> {
    let existing = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    if existing
        .lines()
        .any(|line| line.trim().trim_start_matches('/').trim_end_matches('/') == ".clue")
    {
        return Ok(());
    }

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    file.write_all(b"/.clue\n")?;
    Ok(())
}

pub(crate) struct LoadedPackage {
    pub name: String,
    pub entry: PathBuf,
    pub kind: ProjectKind,
    pub manifest_fingerprint: String,
    pub source: pipeline::LoadedSource,
}

pub(crate) fn load_with_overlays(
    root: &Path,
    overlays: &HashMap<PathBuf, String>,
) -> io::Result<LoadedPackage> {
    load_inner(root, ProjectKind::Binary, overlays, &mut HashSet::new())
}

fn load_inner(
    root: &Path,
    kind: ProjectKind,
    overlays: &HashMap<PathBuf, String>,
    stack: &mut HashSet<PathBuf>,
) -> io::Result<LoadedPackage> {
    let root = fs::canonicalize(root)?;
    if !stack.insert(root.clone()) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("cyclic package dependency involving `{}`", root.display()),
        ));
    }

    let manifest = manifest::read(&root, kind)?;
    let mut source = String::new();
    let mut files = Vec::new();
    let mut source_map = pipeline::SourceMap::default();
    let mut manifest_fingerprint = manifest.fingerprint.clone();
    for dependency in &manifest.dependencies {
        if !is_ident(&dependency.alias) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "dependency name `{}` must be a valid module name",
                    dependency.alias
                ),
            ));
        }

        let dependency_package = load_inner(
            &root.join(&dependency.path),
            ProjectKind::Library,
            overlays,
            stack,
        )?;
        if dependency_package.name != dependency.package {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "dependency `{}` expected package `{}`, found `{}`",
                    dependency.alias, dependency.package, dependency_package.name
                ),
            ));
        }

        manifest_fingerprint.push_str(&dependency_package.manifest_fingerprint);
        let pipeline::LoadedSource {
            source: dependency_source,
            files: dependency_files,
            source_map: dependency_map,
        } = dependency_package.source;
        files.extend(dependency_files);
        source.push_str(&format!("mod {} {{\n", dependency.alias));
        let dependency_start = source.len();
        source.push_str(&dependency_source);
        source_map.extend(dependency_map, dependency_start);
        source.push_str("\n}\n\n");
    }

    let own_source = pipeline::load_source_file_with_overlays(&manifest.entry, overlays)?;
    let pipeline::LoadedSource {
        source: own_text,
        files: own_files,
        source_map: own_map,
    } = own_source;
    files.extend(own_files);
    let own_start = source.len();
    source.push_str(&own_text);
    source_map.extend(own_map, own_start);
    stack.remove(&root);
    Ok(LoadedPackage {
        name: manifest.name,
        entry: manifest.entry,
        kind: manifest.kind,
        manifest_fingerprint,
        source: pipeline::LoadedSource {
            source,
            files,
            source_map,
        },
    })
}

fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}
