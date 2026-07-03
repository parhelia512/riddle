use std::fs;
use std::io;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use toml::{Table, Value};

pub const CLUE_PROJECT_FILE_NAME: &str = "Clue.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    Binary,
    Library,
}

pub fn new_manifest(package_name: &str, kind: PackageKind) -> String {
    let mut package = Table::new();
    package.insert("name".to_string(), Value::String(package_name.to_string()));
    package.insert("version".to_string(), Value::String("0.1.0".to_string()));
    if kind == PackageKind::Library {
        package.insert(
            "entry".to_string(),
            Value::String("src/lib.rid".to_string()),
        );
    }

    let package = document_table("package", package);
    let dependencies = document_table("dependencies", Table::new());
    format!("{package}\n{dependencies}")
}

fn document_table(name: &str, table: Table) -> String {
    let mut document = Table::new();
    document.insert(name.to_string(), Value::Table(table));
    toml::to_string(&Value::Table(document)).expect("generated Clue manifest should serialize")
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub entry: PathBuf,
    pub fingerprint: String,
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub alias: String,
    pub package: String,
    pub path: PathBuf,
}

pub fn read(root: &Path, kind: PackageKind) -> io::Result<Manifest> {
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
    let entry = match optional_string_field(package, "entry", "package")? {
        Some(path) => root.join(path),
        None => entry_file(root, &name, kind)?,
    };
    if !entry.is_file() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("entry file `{}` does not exist", entry.display()),
        ));
    }

    Ok(Manifest {
        name,
        entry,
        fingerprint: value.to_string(),
        dependencies: dependencies(&value)?,
    })
}

fn dependencies(value: &Value) -> io::Result<Vec<Dependency>> {
    let Some(dependencies) = table(value, "dependencies") else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (alias, value) in dependencies {
        let Some(config) = value.as_table() else {
            return Err(unsupported_dependency(alias));
        };
        let Some(path) = optional_string_field(config, "path", alias)? else {
            return Err(unsupported_dependency(alias));
        };
        let package =
            optional_string_field(config, "package", alias)?.unwrap_or_else(|| alias.to_string());
        out.push(Dependency {
            alias: alias.to_string(),
            package,
            path: PathBuf::from(path),
        });
    }
    Ok(out)
}

fn entry_file(root: &Path, package_name: &str, kind: PackageKind) -> io::Result<PathBuf> {
    let candidates = match kind {
        PackageKind::Binary => vec![
            root.join("src").join("main.rid"),
            root.join("src").join("lib.rid"),
            root.join(format!("{package_name}.rid")),
            root.join("main.rid"),
        ],
        PackageKind::Library => vec![
            root.join("src").join("lib.rid"),
            root.join(format!("{package_name}.rid")),
            root.join("lib.rid"),
            root.join("src").join("main.rid"),
        ],
    };
    for path in candidates {
        if path.is_file() {
            return Ok(path);
        }
    }

    Err(Error::new(
        ErrorKind::NotFound,
        "missing entry file; expected src/main.rid, src/lib.rid, <package>.rid, main.rid, or lib.rid",
    ))
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
        .map(|value| Some(value.to_string()))
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
