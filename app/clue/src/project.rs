use crate::manifest::{self, CLUE_PROJECT_FILE_NAME};
use crate::model::{self, DependencyKind, LibraryType};
use anyhow::{Context, bail};
use riddlec::pipeline;
use semver::Version;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::hash::BuildHasher;
use std::io::{self, Error, ErrorKind, Write as _};
use std::ops::Range;
use std::path::{Path, PathBuf};

const DEFAULT_MAIN: &str = "fun main() {\n}\n";
const DEFAULT_LIB: &str = "pub fun add(x: i32, y: i32) -> i32 {\n    x + y\n}\n";

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ProjectKind {
    Binary,
    Library,
    ProcMacro,
}

/// Initializes a project in an existing directory.
///
/// # Errors
///
/// Returns an error when the directory or project files cannot be created.
pub fn init(path: &Path, kind: ProjectKind) -> anyhow::Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create `{}`", path.display()))?;
    create(path, kind)
}

/// Creates a project in a new directory.
///
/// # Errors
///
/// Returns an error when the destination exists or project files cannot be created.
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
        ProjectKind::Library | ProjectKind::ProcMacro => "lib.rid",
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
            ProjectKind::Library | ProjectKind::ProcMacro => DEFAULT_LIB,
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

#[derive(Clone)]
pub struct LoadedPackage {
    pub root: PathBuf,
    pub name: String,
    pub target_name: Option<String>,
    pub version: Version,
    pub entry: PathBuf,
    pub kind: ProjectKind,
    pub build_target: Option<String>,
    pub runtime_source: Option<PathBuf>,
    pub gc_enabled: bool,
    pub manifest_fingerprint: String,
    pub source: pipeline::LoadedSource,
    pub macro_parse: Option<frontend::tree_builder::Parse>,
    pub package_ranges: Vec<Range<usize>>,
    pub package_names: Vec<String>,
    pub watched_files: Vec<PathBuf>,
    pub proc_macros: Vec<ProcMacroPackage>,
    pub library_types: Vec<LibraryType>,
}

#[derive(Clone)]
pub struct ProcMacroPackage {
    pub root: PathBuf,
    pub alias: String,
    pub name: String,
    pub entry: PathBuf,
    pub source: pipeline::LoadedSource,
    pub manifest_fingerprint: String,
}

#[derive(Default)]
pub struct LoadOptions {
    pub bin: Option<String>,
    pub entry: Option<PathBuf>,
    pub target_name: Option<String>,
    pub features: Vec<String>,
    pub no_default_features: bool,
    pub require_bin: bool,
    pub include_dev: bool,
}

struct LoadRequest<'a> {
    bin: Option<&'a str>,
    entry: Option<&'a Path>,
    target_name: Option<&'a str>,
    features: &'a [String],
    no_default_features: bool,
    require_bin: bool,
    include_dev: bool,
}

pub fn load_with_overlays<S: BuildHasher>(
    root: &Path,
    overlays: &HashMap<PathBuf, String, S>,
) -> io::Result<LoadedPackage> {
    load_with_overlays_and_options(root, overlays, &LoadOptions::default())
}

pub fn load_with_overlays_and_options<S: BuildHasher>(
    root: &Path,
    overlays: &HashMap<PathBuf, String, S>,
    options: &LoadOptions,
) -> io::Result<LoadedPackage> {
    let mut package = load_inner(
        root,
        ProjectKind::Binary,
        overlays,
        &mut HashSet::new(),
        LoadRequest {
            bin: options.bin.as_deref(),
            entry: options.entry.as_deref(),
            target_name: options.target_name.as_deref(),
            features: &options.features,
            no_default_features: options.no_default_features,
            require_bin: options.require_bin,
            include_dev: options.include_dev,
        },
    )?;
    if let Some(workspace_root) = crate::workspace::find_workspace_root(root)? {
        package.watched_files.push(workspace_root.join("Clue.lock"));
        package.watched_files.sort();
        package.watched_files.dedup();
    }
    Ok(package)
}

fn load_inner<S: BuildHasher>(
    root: &Path,
    kind: ProjectKind,
    overlays: &HashMap<PathBuf, String, S>,
    stack: &mut HashSet<PathBuf>,
    request: LoadRequest<'_>,
) -> io::Result<LoadedPackage> {
    let root = fs::canonicalize(root)?;
    if !stack.insert(root.clone()) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("cyclic package dependency involving `{}`", root.display()),
        ));
    }

    let mut manifest = manifest::read(&root, kind)?;
    let feature_resolution = model::resolve_features(
        &manifest.features,
        &manifest.dependencies,
        request.features,
        !request.no_default_features,
    )
    .map_err(|error| Error::new(ErrorKind::InvalidInput, error))?;
    let enabled_features = &feature_resolution.active;
    let selected_target = if let Some(entry) = request.entry {
        if !entry.is_file() {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("entry file `{}` does not exist", entry.display()),
            ));
        }
        manifest.entry = entry.to_path_buf();
        manifest.target_name = request.target_name.map(str::to_owned);
        request.target_name.map(str::to_owned)
    } else if kind == ProjectKind::Binary {
        select_binary(
            &mut manifest,
            request.bin,
            request.require_bin,
            enabled_features,
        )?
    } else {
        None
    };
    let mut source = String::new();
    let mut files = Vec::new();
    let mut source_map = pipeline::SourceMap::default();
    let mut package_ranges = Vec::new();
    let mut package_names = Vec::new();
    let mut proc_macros = Vec::new();
    let mut watched_files = vec![root.join(CLUE_PROJECT_FILE_NAME)];
    let mut manifest_fingerprint = manifest.fingerprint.clone();
    for dependency in &manifest.dependencies {
        if dependency.kind == DependencyKind::Development && !request.include_dev {
            continue;
        }
        if dependency.optional && !enabled_features.contains(&dependency.alias) {
            continue;
        }
        validate_dependency_alias(&dependency.alias)?;

        let dependency_root = crate::package::dependency_root(&root, dependency)?;

        let mut dependency_features = dependency.features.clone();
        if let Some(forwarded) = feature_resolution
            .dependency_features
            .get(&dependency.alias)
        {
            dependency_features.extend(forwarded.iter().cloned());
            dependency_features.sort();
            dependency_features.dedup();
        }
        let dependency_package = load_inner(
            &dependency_root,
            ProjectKind::Library,
            overlays,
            stack,
            LoadRequest {
                bin: None,
                entry: None,
                target_name: None,
                features: &dependency_features,
                no_default_features: !dependency.default_features,
                require_bin: false,
                include_dev: false,
            },
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
        if let Some(requirement) = &dependency.requirement
            && !requirement.matches(&dependency_package.version)
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "dependency `{}` requires version `{requirement}`, found `{}`",
                    dependency.alias, dependency_package.version
                ),
            ));
        }

        manifest_fingerprint.push_str(&dependency_package.manifest_fingerprint);
        proc_macros.extend(dependency_package.proc_macros.iter().cloned());
        if dependency_package.kind == ProjectKind::ProcMacro {
            watched_files.extend(dependency_package.watched_files.iter().cloned());
            proc_macros.push(ProcMacroPackage {
                root: dependency_package.root,
                alias: dependency.alias.clone(),
                name: dependency_package.name,
                entry: dependency_package.entry,
                source: dependency_package.source,
                manifest_fingerprint: dependency_package.manifest_fingerprint,
            });
            continue;
        }
        let dependency_ranges = dependency_package.package_ranges;
        package_names.extend(dependency_package.package_names);
        watched_files.extend(dependency_package.watched_files);
        let pipeline::LoadedSource {
            source: dependency_source,
            files: dependency_files,
            source_map: dependency_map,
        } = dependency_package.source;
        files.extend(dependency_files);
        writeln!(source, "mod {} {{", dependency.alias)
            .expect("writing package source to a String should not fail");
        let dependency_start = source.len();
        source.push_str(&dependency_source);
        source_map.extend(dependency_map, dependency_start);
        package_ranges.extend(
            dependency_ranges
                .into_iter()
                .map(|range| range.start + dependency_start..range.end + dependency_start),
        );
        source.push_str("\n}\n\n");
    }

    let own_source = pipeline::load_source_file_with_overlays(&manifest.entry, overlays)?;
    let pipeline::LoadedSource {
        source: own_text,
        files: own_files,
        source_map: own_map,
    } = own_source;
    files.extend(own_files);
    watched_files.extend(files.iter().cloned());
    watched_files.sort();
    watched_files.dedup();
    let own_start = source.len();
    source.push_str(&own_text);
    source_map.extend(own_map, own_start);
    package_ranges.push(own_start..source.len());
    package_names.push(manifest.name.clone());
    stack.remove(&root);
    Ok(LoadedPackage {
        root,
        name: manifest.name,
        target_name: selected_target.or(manifest.target_name),
        version: manifest.version,
        entry: manifest.entry,
        kind: manifest.kind,
        build_target: manifest.build_target,
        runtime_source: manifest.runtime_source,
        gc_enabled: manifest.gc_enabled,
        manifest_fingerprint,
        source: pipeline::LoadedSource {
            source,
            files,
            source_map,
        },
        macro_parse: None,
        package_ranges,
        package_names,
        watched_files,
        proc_macros,
        library_types: manifest
            .library
            .map(|target| target.library_types)
            .unwrap_or_default(),
    })
}

fn select_binary(
    manifest: &mut manifest::Manifest,
    selected: Option<&str>,
    require_bin: bool,
    enabled_features: &BTreeSet<String>,
) -> io::Result<Option<String>> {
    if manifest.binaries.is_empty() {
        if selected.is_some() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("package `{}` has no binary target", manifest.name),
            ));
        }
        return Ok(None);
    }
    if selected.is_none() && require_bin && manifest.binaries.len() > 1 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "package `{}` has multiple binary targets; choose one with `--bin <name>`",
                manifest.name
            ),
        ));
    }
    let target = selected
        .map(|name| {
            manifest
                .binaries
                .iter()
                .find(|target| target.name == name)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::NotFound,
                        format!(
                            "binary target `{name}` was not found in package `{}`",
                            manifest.name
                        ),
                    )
                })
        })
        .transpose()?
        .or_else(|| manifest.binaries.first());
    let target = target.expect("binary target list was checked above");
    let missing = target
        .required_features
        .iter()
        .filter(|feature| !enabled_features.contains(*feature))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "target `{}` requires features: {}",
                target.name,
                missing.join(", ")
            ),
        ));
    }
    if !target.path.is_file() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("entry file `{}` does not exist", target.path.display()),
        ));
    }
    if manifest.binaries.len() > 1 {
        manifest.entry = target.path.clone();
    }
    manifest.target_name = Some(target.name.clone());
    Ok(Some(target.name.clone()))
}

pub(crate) fn enabled_features(
    manifest: &manifest::Manifest,
    requested: &[String],
    no_default_features: bool,
) -> io::Result<BTreeSet<String>> {
    model::resolve_features(
        &manifest.features,
        &manifest.dependencies,
        requested,
        !no_default_features,
    )
    .map(|resolution| resolution.active)
    .map_err(|error| Error::new(ErrorKind::InvalidInput, error))
}

fn validate_dependency_alias(alias: &str) -> io::Result<()> {
    if is_ident(alias) {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::InvalidData,
            format!("dependency name `{alias}` must be a valid module name"),
        ))
    }
}

fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}
