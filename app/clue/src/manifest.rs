use std::fs;
use std::io;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use toml::{Table, Value};

pub const CLUE_PROJECT_FILE_NAME: &str = "Clue.toml";
pub const CLUE_PROJECT_FILE_TEMPLATE: &str = r#"[package]
name = "{package_name}"
version = "0.1.0"

[dependencies]
"#;

#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub entry: PathBuf,
    pub text: String,
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, Copy)]
pub enum PackageKind {
    Binary,
    Library,
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
        text,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "riddle-clue-manifest-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn reads_explicit_entry_and_cargo_style_path_dependency() {
        let root = temp_root("read");
        fs::create_dir_all(root.join("src").join("bin")).unwrap();
        fs::write(
            root.join(CLUE_PROJECT_FILE_NAME),
            r#"[package]
name = "app"
entry = "src/bin/app.rid"
version = "0.1.0"

[dependencies]
math = { package = "math-core", path = "../math-core" }
"#,
        )
        .unwrap();
        fs::write(
            root.join("src").join("bin").join("app.rid"),
            "fun main() -> i32 { 0 }",
        )
        .unwrap();

        let manifest = read(&root, PackageKind::Binary).unwrap();
        assert_eq!(manifest.name, "app");
        assert_eq!(manifest.entry, root.join("src").join("bin").join("app.rid"));
        assert_eq!(
            manifest.dependencies,
            vec![Dependency {
                alias: "math".to_string(),
                package: "math-core".to_string(),
                path: PathBuf::from("../math-core"),
            }]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_version_dependency() {
        let root = temp_root("version");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join(CLUE_PROJECT_FILE_NAME),
            r#"[package]
name = "app"

[dependencies]
math = "1.0"
"#,
        )
        .unwrap();
        fs::write(root.join("src").join("main.rid"), "fun main() -> i32 { 0 }").unwrap();

        let error = read(&root, PackageKind::Binary).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(error.to_string().contains("local path dependency"));

        let _ = fs::remove_dir_all(root);
    }
}
