use crate::ProjectKind;
use crate::model::{
    DependencyKind, DependencySource, DependencySpec, GitReference, LibraryType, Target, TargetKind,
};
use anyhow::bail;
use semver::{Version, VersionReq};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};
use toml::{Table, Value};

pub const CLUE_PROJECT_FILE_NAME: &str = "Clue.toml";
pub const WORKSPACE_CRATES_KEY: &str = "crates";

pub fn new_manifest(package_name: &str, kind: ProjectKind) -> String {
    let mut package = Table::new();
    package.insert("name".into(), Value::String(package_name.into()));
    package.insert("version".into(), Value::String("0.1.0".into()));

    let mut target = Table::new();
    target.insert("name".into(), Value::String(package_name.into()));
    target.insert(
        "path".into(),
        Value::String(
            match kind {
                ProjectKind::Binary => "src/main.rid",
                ProjectKind::Library | ProjectKind::ProcMacro => "src/lib.rid",
            }
            .into(),
        ),
    );
    if kind == ProjectKind::ProcMacro {
        target.insert("proc-macro".into(), Value::Boolean(true));
    }

    let package = document("package", Value::Table(package));
    let target = match kind {
        ProjectKind::Binary => document("bin", Value::Array(vec![Value::Table(target)])),
        ProjectKind::Library | ProjectKind::ProcMacro => document("lib", Value::Table(target)),
    };
    let dependencies = document("dependencies", Value::Table(Table::new()));
    format!("{package}\n{target}\n{dependencies}")
}

fn document(name: &str, value: Value) -> String {
    let mut root = Table::new();
    root.insert(name.into(), value);
    toml::to_string(&Value::Table(root)).expect("generated Clue manifest should serialize")
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: Version,
    pub license: Option<String>,
    pub publish: Option<Vec<String>>,
    pub entry: PathBuf,
    pub kind: ProjectKind,
    pub build_target: Option<String>,
    pub runtime_source: Option<PathBuf>,
    pub gc_enabled: bool,
    pub fingerprint: String,
    pub dependencies: Vec<DependencySpec>,
    pub features: BTreeMap<String, Vec<String>>,
    pub source_hash: String,
    pub binaries: Vec<BinaryTarget>,
    pub tests: Vec<Target>,
    pub examples: Vec<Target>,
    pub benches: Vec<Target>,
    pub library: Option<Target>,
    pub target_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryTarget {
    pub name: String,
    pub path: PathBuf,
    pub required_features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceManifest {
    pub crates: Vec<PathBuf>,
}

pub fn read_workspace(root: &Path) -> io::Result<Option<WorkspaceManifest>> {
    let manifest_path = root.join(CLUE_PROJECT_FILE_NAME);
    let text = fs::read_to_string(&manifest_path)?;
    let value = text.parse::<Table>().map(Value::Table).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid `{}`: {error}", manifest_path.display()),
        )
    })?;
    let Some(workspace) = table(&value, "workspace") else {
        return Ok(None);
    };
    let crates = workspace_crates(workspace)?;
    Ok(Some(WorkspaceManifest { crates }))
}

pub fn is_virtual_workspace_root(root: &Path) -> io::Result<bool> {
    let manifest_path = root.join(CLUE_PROJECT_FILE_NAME);
    let text = fs::read_to_string(&manifest_path)?;
    let value = text.parse::<Table>().map(Value::Table).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid `{}`: {error}", manifest_path.display()),
        )
    })?;
    Ok(table(&value, "workspace").is_some() && table(&value, "package").is_none())
}

pub fn read(root: &Path, kind: ProjectKind) -> io::Result<Manifest> {
    let manifest_path = root.join(CLUE_PROJECT_FILE_NAME);
    let text = fs::read_to_string(&manifest_path)?;
    let value = text.parse::<Table>().map(Value::Table).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid `{}`: {error}", manifest_path.display()),
        )
    })?;
    let package = table(&value, "package").ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("missing [package] in `{}`", manifest_path.display()),
        )
    })?;
    let name = string_field(package, "name", "package")?;
    let version_text =
        optional_string_field(package, "version", "package")?.unwrap_or_else(|| "0.1.0".into());
    let version = Version::parse(&version_text).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid package version `{version_text}`: {error}"),
        )
    })?;
    let license = optional_string_field(package, "license", "package")?;
    let publish = publish_registries(package)?;
    validate_package_name(&name).map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
    let binaries = binary_targets(root, &value, &name)?;
    let library = library_target(root, &value, &name)?;
    let tests = executable_targets(root, &value, "test", TargetKind::Test, "tests")?;
    let examples = executable_targets(root, &value, "example", TargetKind::Example, "examples")?;
    let benches = executable_targets(root, &value, "bench", TargetKind::Bench, "benches")?;
    let target = target_path(root, &value, kind, &binaries)?;
    let target_kind = target.as_ref().map_or(kind, |(_, kind)| *kind);
    let (entry, kind) = match optional_string_field(package, "entry", "package")? {
        Some(path) => (root.join(path), target_kind),
        None => match target {
            Some(target) => target,
            None => (entry_file(root, &name, target_kind)?, target_kind),
        },
    };
    if !entry.is_file() && !(target_kind == ProjectKind::Binary && binaries.len() > 1) {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("entry file `{}` does not exist", entry.display()),
        ));
    }
    let (runtime_source, gc_enabled) = runtime_config(root, &value, kind)?;
    let build_target = table(&value, "build")
        .map(|build| optional_string_field(build, "target", "build"))
        .transpose()?
        .flatten();

    let dependencies = dependencies(&value)?;
    let mut features = feature_names(&value)?;
    for dependency in dependencies.iter().filter(|dependency| dependency.optional) {
        features.entry(dependency.alias.clone()).or_default();
    }
    let source_hash = package_hash(root)?;
    Ok(Manifest {
        name,
        version,
        license,
        publish,
        entry,
        kind,
        build_target,
        runtime_source,
        gc_enabled,
        fingerprint: value.to_string(),
        dependencies,
        features,
        source_hash,
        target_name: (target_kind == ProjectKind::Binary)
            .then(|| binaries.first())
            .flatten()
            .map(|target| target.name.clone()),
        binaries,
        tests,
        examples,
        benches,
        library,
    })
}

fn publish_registries(package: &Table) -> io::Result<Option<Vec<String>>> {
    let Some(value) = package.get("publish") else {
        return Ok(None);
    };
    if let Some(enabled) = value.as_bool() {
        return Ok((!enabled).then(Vec::new));
    }
    let values = value.as_array().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            "`package.publish` must be a boolean or an array of registry names",
        )
    })?;
    values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    "`package.publish` entries must be strings",
                )
            })
        })
        .collect::<io::Result<Vec<_>>>()
        .map(Some)
}

fn feature_names(value: &Value) -> io::Result<BTreeMap<String, Vec<String>>> {
    let Some(features) = table(value, "features") else {
        return Ok(BTreeMap::new());
    };
    let mut result = BTreeMap::new();
    for (name, value) in features {
        validate_feature_name(name).map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        let values = value
            .as_array()
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("`features.{name}` must be an array"),
                )
            })?
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("`features.{name}` entries must be strings"),
                    )
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        result.insert(name.clone(), values);
    }
    Ok(result)
}

fn validate_feature_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty()
        || !name.chars().all(|character| {
            character == '_' || character == '-' || character.is_ascii_alphanumeric()
        })
        || !name
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        bail!("invalid feature name `{name}`")
    }
    Ok(())
}

fn package_hash(root: &Path) -> io::Result<String> {
    let mut files = Vec::new();
    collect_hash_files(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (path, bytes) in files {
        path.hash(&mut hasher);
        bytes.hash(&mut hasher);
    }
    Ok(format!("{:016x}", hasher.finish()))
}

fn collect_hash_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<(String, Vec<u8>)>,
) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_str(), Some(".clue" | ".git" | "target"))
            || name.to_string_lossy().starts_with("Clue.lock")
        {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_hash_files(root, &path, files)?;
        } else if entry.file_type()?.is_file() {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            files.push((relative, fs::read(path)?));
        }
    }
    Ok(())
}

fn workspace_crates(workspace: &Table) -> io::Result<Vec<PathBuf>> {
    let Some(value) = workspace.get(WORKSPACE_CRATES_KEY) else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("missing `workspace.{WORKSPACE_CRATES_KEY}`"),
        ));
    };
    let Some(entries) = value.as_array() else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("`workspace.{WORKSPACE_CRATES_KEY}` must be an array of paths"),
        ));
    };
    entries
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value.as_str().map(PathBuf::from).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("`workspace.{WORKSPACE_CRATES_KEY}[{index}]` must be a string path"),
                )
            })
        })
        .collect()
}

fn runtime_config(
    root: &Path,
    value: &Value,
    kind: ProjectKind,
) -> io::Result<(Option<PathBuf>, bool)> {
    let Some(runtime) = table(value, "runtime") else {
        return Ok((None, true));
    };
    if kind != ProjectKind::Binary {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "[runtime] is only supported for binary packages",
        ));
    }
    let gc_enabled = optional_bool_field(runtime, "gc", "runtime")?.unwrap_or(true);
    let source = optional_string_field(runtime, "source", "runtime")?.map(PathBuf::from);
    if !gc_enabled && source.is_some() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "`runtime.gc = false` cannot be combined with `runtime.source`",
        ));
    }
    let Some(source) = source else {
        return Ok((None, gc_enabled));
    };
    let resolved = root.join(&source);
    if !resolved.is_file() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("runtime source `{}` does not exist", resolved.display()),
        ));
    }
    Ok((Some(source), gc_enabled))
}

pub fn validate_package_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() || matches!(name, "." | "..") || name.chars().any(std::path::is_separator) {
        bail!("invalid package name `{name}`");
    }
    Ok(())
}

fn target_path(
    root: &Path,
    value: &Value,
    kind: ProjectKind,
    binaries: &[BinaryTarget],
) -> io::Result<Option<(PathBuf, ProjectKind)>> {
    if kind == ProjectKind::Binary
        && let Some(binary) = binaries.first()
    {
        return Ok(Some((binary.path.clone(), ProjectKind::Binary)));
    }
    if let Some(lib) = table(value, "lib") {
        let kind = if optional_bool_field(lib, "proc-macro", "lib")?.unwrap_or(false) {
            ProjectKind::ProcMacro
        } else {
            ProjectKind::Library
        };
        return Ok(Some((
            root.join(
                optional_string_field(lib, "path", "lib")?.unwrap_or_else(|| "src/lib.rid".into()),
            ),
            kind,
        )));
    }
    if !binaries.is_empty() && kind != ProjectKind::Binary {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "library dependencies must declare a `[lib]` target",
        ));
    }
    Ok(None)
}

fn binary_targets(root: &Path, value: &Value, package_name: &str) -> io::Result<Vec<BinaryTarget>> {
    let explicit = value
        .get("bin")
        .map(|targets| {
            targets.as_array().ok_or_else(|| {
                Error::new(ErrorKind::InvalidData, "`bin` must be an array of targets")
            })
        })
        .transpose()?
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut targets = explicit
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let target = value.as_table().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("`bin[{index}]` must be a table"),
                )
            })?;
            let path = root.join(
                optional_string_field(target, "path", "bin")?
                    .unwrap_or_else(|| "src/main.rid".into()),
            );
            let name = optional_string_field(target, "name", "bin")?.unwrap_or_else(|| {
                let stem = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or(package_name);
                if stem == "main" {
                    package_name.to_owned()
                } else {
                    stem.to_owned()
                }
            });
            validate_package_name(&name)
                .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
            Ok(BinaryTarget {
                name,
                path,
                required_features: optional_string_array(target, "required-features", "bin")?,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let main = root.join("src/main.rid");
    if main.is_file() && !targets.iter().any(|target| target.path == main) {
        targets.push(BinaryTarget {
            name: package_name.into(),
            path: main,
            required_features: Vec::new(),
        });
    }
    let bin_dir = root.join("src/bin");
    if let Ok(entries) = fs::read_dir(bin_dir) {
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("rid")
                || targets.iter().any(|target| target.path == path)
            {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| Error::new(ErrorKind::InvalidData, "binary name is not UTF-8"))?
                .to_owned();
            targets.push(BinaryTarget {
                name,
                path,
                required_features: Vec::new(),
            });
        }
    }
    let mut names = BTreeSet::new();
    for target in &targets {
        if !names.insert(&target.name) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("duplicate binary target name `{}`", target.name),
            ));
        }
    }
    Ok(targets)
}

fn library_target(root: &Path, value: &Value, package_name: &str) -> io::Result<Option<Target>> {
    let Some(target) = table(value, "lib") else {
        return Ok(None);
    };
    let proc_macro = optional_bool_field(target, "proc-macro", "lib")?.unwrap_or(false);
    let name = optional_string_field(target, "name", "lib")?
        .unwrap_or_else(|| package_name.replace('-', "_"));
    validate_package_name(&name).map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
    let path = root.join(
        optional_string_field(target, "path", "lib")?.unwrap_or_else(|| "src/lib.rid".into()),
    );
    let kind = if proc_macro {
        TargetKind::ProcMacro
    } else {
        TargetKind::Library
    };
    let library_types = if proc_macro {
        Vec::new()
    } else {
        library_types(target)?
    };
    Ok(Some(Target {
        name,
        path,
        kind,
        required_features: Vec::new(),
        library_types,
    }))
}

fn executable_targets(
    root: &Path,
    value: &Value,
    table_name: &str,
    kind: TargetKind,
    directory: &str,
) -> io::Result<Vec<Target>> {
    let values = value
        .get(table_name)
        .map(|values| {
            values.as_array().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("`{table_name}` must be an array of targets"),
                )
            })
        })
        .transpose()?
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut targets = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let target = value.as_table().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("`{table_name}[{index}]` must be a table"),
                )
            })?;
            let path_text =
                optional_string_field(target, "path", table_name)?.ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("`{table_name}[{index}].path` is required"),
                    )
                })?;
            let path = root.join(path_text);
            let name = optional_string_field(target, "name", table_name)?.unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or(table_name)
                    .to_owned()
            });
            validate_package_name(&name)
                .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
            Ok(Target {
                name,
                path,
                kind,
                required_features: optional_string_array(target, "required-features", table_name)?,
                library_types: Vec::new(),
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    if let Ok(entries) = fs::read_dir(root.join(directory)) {
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("rid")
                || targets.iter().any(|target| target.path == path)
            {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| Error::new(ErrorKind::InvalidData, "target name is not UTF-8"))?
                .to_owned();
            targets.push(Target {
                name,
                path,
                kind,
                required_features: Vec::new(),
                library_types: Vec::new(),
            });
        }
    }
    validate_unique_targets(table_name, &targets)?;
    Ok(targets)
}

fn library_types(target: &Table) -> io::Result<Vec<LibraryType>> {
    let values = optional_string_array(target, "crate-type", "lib")?;
    if values.is_empty() {
        return Ok(vec![LibraryType::RiddleLib]);
    }
    let mut result = Vec::new();
    for value in values {
        let library_type = match value.as_str() {
            "riddlelib" => LibraryType::RiddleLib,
            "staticlib" => LibraryType::StaticLib,
            "cdylib" => LibraryType::Cdylib,
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!("unsupported library crate type `{value}`"),
                ));
            }
        };
        if !result.contains(&library_type) {
            result.push(library_type);
        }
    }
    Ok(result)
}

fn validate_unique_targets(table_name: &str, targets: &[Target]) -> io::Result<()> {
    let mut names = BTreeSet::new();
    for target in targets {
        if !names.insert(&target.name) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("duplicate {table_name} target name `{}`", target.name),
            ));
        }
    }
    Ok(())
}

fn dependencies(value: &Value) -> io::Result<Vec<DependencySpec>> {
    let mut result = dependency_table(value, "dependencies", DependencyKind::Normal)?;
    result.extend(dependency_table(
        value,
        "dev-dependencies",
        DependencyKind::Development,
    )?);
    Ok(result)
}

fn dependency_table(
    value: &Value,
    table_name: &str,
    kind: DependencyKind,
) -> io::Result<Vec<DependencySpec>> {
    let Some(dependencies) = table(value, table_name) else {
        return Ok(Vec::new());
    };
    dependencies
        .iter()
        .map(|(alias, value)| dependency(alias, value, kind))
        .collect()
}

fn dependency(alias: &str, value: &Value, kind: DependencyKind) -> io::Result<DependencySpec> {
    if let Some(version) = value.as_str() {
        return Ok(DependencySpec {
            alias: alias.to_owned(),
            package: alias.to_owned(),
            source: DependencySource::Registry { registry: None },
            requirement: Some(version_requirement(alias, version)?),
            optional: false,
            features: Vec::new(),
            default_features: true,
            kind,
        });
    }
    let config = value.as_table().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("dependency `{alias}` must be a version string or table"),
        )
    })?;
    let package = optional_string_field(config, "package", alias)?.unwrap_or_else(|| alias.into());
    let version = optional_string_field(config, "version", alias)?;
    let path = optional_string_field(config, "path", alias)?;
    let git = optional_string_field(config, "git", alias)?;
    let registry = optional_string_field(config, "registry", alias)?;
    if usize::from(path.is_some()) + usize::from(git.is_some()) > 1 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("dependency `{alias}` cannot specify both `path` and `git`"),
        ));
    }
    let source = if let Some(path) = path {
        DependencySource::Path(PathBuf::from(path))
    } else if let Some(url) = git {
        DependencySource::Git {
            url,
            reference: git_reference(config, alias)?,
        }
    } else {
        DependencySource::Registry { registry }
    };
    Ok(DependencySpec {
        alias: alias.to_owned(),
        package,
        source,
        requirement: version
            .as_deref()
            .map(|version| version_requirement(alias, version))
            .transpose()?,
        optional: optional_bool_field(config, "optional", alias)?.unwrap_or(false),
        features: optional_string_array(config, "features", alias)?,
        default_features: optional_bool_field(config, "default-features", alias)?.unwrap_or(true),
        kind,
    })
}

fn version_requirement(alias: &str, value: &str) -> io::Result<VersionReq> {
    VersionReq::parse(value).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid version requirement `{value}` for dependency `{alias}`: {error}"),
        )
    })
}

fn git_reference(config: &Table, alias: &str) -> io::Result<GitReference> {
    let branch = optional_string_field(config, "branch", alias)?;
    let tag = optional_string_field(config, "tag", alias)?;
    let rev = optional_string_field(config, "rev", alias)?;
    if usize::from(branch.is_some()) + usize::from(tag.is_some()) + usize::from(rev.is_some()) > 1 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("dependency `{alias}` may specify only one of `branch`, `tag`, or `rev`"),
        ));
    }
    Ok(if let Some(branch) = branch {
        GitReference::Branch(branch)
    } else if let Some(tag) = tag {
        GitReference::Tag(tag)
    } else if let Some(rev) = rev {
        GitReference::Rev(rev)
    } else {
        GitReference::DefaultBranch
    })
}

fn optional_string_array(table: &Table, key: &str, owner: &str) -> io::Result<Vec<String>> {
    let Some(value) = table.get(key) else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!("`{owner}.{key}` must be an array"),
            )
        })?
        .iter()
        .map(|item| {
            item.as_str().map(str::to_owned).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("`{owner}.{key}` entries must be strings"),
                )
            })
        })
        .collect()
}

fn entry_file(root: &Path, package_name: &str, kind: ProjectKind) -> io::Result<PathBuf> {
    let candidates = match kind {
        ProjectKind::Binary => vec![
            root.join("src/main.rid"),
            root.join("src/lib.rid"),
            root.join(format!("{package_name}.rid")),
            root.join("main.rid"),
        ],
        ProjectKind::Library | ProjectKind::ProcMacro => vec![
            root.join("src/lib.rid"),
            root.join(format!("{package_name}.rid")),
            root.join("lib.rid"),
            root.join("src/main.rid"),
        ],
    };
    candidates.into_iter().find(|path| path.is_file()).ok_or_else(|| {
        Error::new(
            ErrorKind::NotFound,
            "missing entry file; expected src/main.rid, src/lib.rid, <package>.rid, main.rid, or lib.rid",
        )
    })
}

fn table<'a>(value: &'a Value, name: &str) -> Option<&'a Table> {
    value.get(name).and_then(Value::as_table)
}

fn string_field(table: &Table, key: &str, owner: &str) -> io::Result<String> {
    optional_string_field(table, key, owner)?.ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("missing `{key}` in `{owner}`"),
        )
    })
}

fn optional_string_field(table: &Table, key: &str, owner: &str) -> io::Result<Option<String>> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(|value| Some(value.into()))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!("`{owner}.{key}` must be a string"),
            )
        })
}

fn optional_bool_field(table: &Table, key: &str, owner: &str) -> io::Result<Option<bool>> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    value.as_bool().map(Some).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("`{owner}.{key}` must be a boolean"),
        )
    })
}
