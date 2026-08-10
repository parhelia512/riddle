use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::Path;

pub(crate) const LOCK_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LockFile {
    pub(crate) version: u32,
    pub(crate) package: Vec<LockPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LockPackage {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) dependencies: Vec<String>,
}

pub(crate) fn read(path: &Path) -> io::Result<Option<LockFile>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    toml::from_str(&text).map(Some).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("invalid `{}`: {error}", path.display()),
        )
    })
}

pub(crate) fn write_if_changed(path: &Path, expected: &LockFile) -> io::Result<()> {
    let current = read(path)?;
    if current.as_ref() == Some(expected) {
        return Ok(());
    }
    let text = toml::to_string_pretty(expected).map_err(|error| {
        Error::new(
            ErrorKind::InvalidData,
            format!("failed to serialize `{}`: {error}", path.display()),
        )
    })?;
    fs::write(path, text)
}
