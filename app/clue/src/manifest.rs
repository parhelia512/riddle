use crate::ProjectKind;
use anyhow::bail;
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};
use toml::{Table, Value};

pub(crate) const CLUE_PROJECT_FILE_NAME: &str = "Clue.toml";

pub(crate) fn new_manifest(package_name: &str, kind: ProjectKind) -> String {
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
                ProjectKind::Library => "src/lib.rid",
            }
            .into(),
        ),
    );

    let package = document("package", Value::Table(package));
    let target = match kind {
        ProjectKind::Binary => document("bin", Value::Array(vec![Value::Table(target)])),
        ProjectKind::Library => document("lib", Value::Table(target)),
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
    pub entry: PathBuf,
    pub kind: ProjectKind,
    pub runtime_source: Option<PathBuf>,
    pub fingerprint: String,
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub alias: String,
    pub package: String,
    pub path: PathBuf,
}

pub(crate) fn read(root: &Path, kind: ProjectKind) -> io::Result<Manifest> {
    let manifest_path = root.join(CLUE_PROJECT_FILE_NAME);
    let text = fs::read_to_string(&manifest_path)?;
    let value = text.parse::<Value>().map_err(|error| {
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
    validate_package_name(&name).map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
    let (entry, kind) = match optional_string_field(package, "entry", "package")? {
        Some(path) => (root.join(path), kind),
        None => target_path(root, &value, kind)?.unwrap_or((entry_file(root, &name, kind)?, kind)),
    };
    if !entry.is_file() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("entry file `{}` does not exist", entry.display()),
        ));
    }
    let runtime_source = runtime_source(root, &value, kind)?;

    Ok(Manifest {
        name,
        entry,
        kind,
        runtime_source,
        fingerprint: value.to_string(),
        dependencies: dependencies(&value)?,
    })
}

fn runtime_source(root: &Path, value: &Value, kind: ProjectKind) -> io::Result<Option<PathBuf>> {
    let Some(runtime) = table(value, "runtime") else {
        return Ok(None);
    };
    if kind == ProjectKind::Library {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "[runtime] is only supported for binary packages",
        ));
    }
    let source = PathBuf::from(string_field(runtime, "source", "runtime")?);
    let resolved = root.join(&source);
    if !resolved.is_file() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("runtime source `{}` does not exist", resolved.display()),
        ));
    }
    Ok(Some(source))
}

pub(crate) fn validate_package_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() || matches!(name, "." | "..") || name.chars().any(std::path::is_separator) {
        bail!("invalid package name `{name}`");
    }
    Ok(())
}

fn target_path(
    root: &Path,
    value: &Value,
    kind: ProjectKind,
) -> io::Result<Option<(PathBuf, ProjectKind)>> {
    if kind == ProjectKind::Binary
        && let Some(bin) = value
            .get("bin")
            .and_then(Value::as_array)
            .and_then(|targets| targets.first())
            .and_then(Value::as_table)
    {
        return Ok(Some((
            root.join(
                optional_string_field(bin, "path", "bin")?.unwrap_or_else(|| "src/main.rid".into()),
            ),
            ProjectKind::Binary,
        )));
    }
    if let Some(lib) = table(value, "lib") {
        return Ok(Some((
            root.join(
                optional_string_field(lib, "path", "lib")?.unwrap_or_else(|| "src/lib.rid".into()),
            ),
            ProjectKind::Library,
        )));
    }
    Ok(None)
}

fn dependencies(value: &Value) -> io::Result<Vec<Dependency>> {
    let Some(dependencies) = table(value, "dependencies") else {
        return Ok(Vec::new());
    };
    dependencies
        .iter()
        .map(|(alias, value)| {
            let config = value
                .as_table()
                .ok_or_else(|| unsupported_dependency(alias))?;
            let path = optional_string_field(config, "path", alias)?
                .ok_or_else(|| unsupported_dependency(alias))?;
            let package = optional_string_field(config, "package", alias)?
                .unwrap_or_else(|| alias.to_string());
            Ok(Dependency {
                alias: alias.to_string(),
                package,
                path: PathBuf::from(path),
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
        ProjectKind::Library => vec![
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

fn unsupported_dependency(name: &str) -> Error {
    Error::new(
        ErrorKind::InvalidData,
        format!(
            "dependency `{name}` must be a local path dependency like `{name} = {{ path = \"../{name}\" }}`"
        ),
    )
}
