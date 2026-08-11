use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Error, ErrorKind};
use std::path::Path;

pub(crate) const LOCK_VERSION: u32 = 3;

pub(crate) struct FileGuard {
    file: fs::File,
}

pub(crate) fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        let source = source
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let destination = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        // SAFETY: both vectors are NUL-terminated UTF-16 paths that stay alive for the call;
        // MoveFileExW only reads them and performs the replacement in the same directory.
        let success = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } != 0;
        if !success {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        fs::rename(source, destination)
    }
}

#[cfg(windows)]
const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

#[cfg(windows)]
unsafe extern "system" {
    fn MoveFileExW(existing_name: *const u16, new_name: *const u16, flags: u32) -> i32;
}

impl FileGuard {
    pub(crate) fn acquire(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.lock()?;
        Ok(Self { file })
    }
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LockFile {
    pub(crate) version: u32,
    pub(crate) package: Vec<LockPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LockPackage {
    pub(crate) name: String,
    pub(crate) version: String,
    #[serde(default)]
    pub(crate) source: String,
    pub(crate) path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) dependencies: Vec<String>,
    #[serde(default)]
    pub(crate) source_hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) checksum: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) features: Vec<String>,
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
    let guard_path = path.with_extension("lock.guard");
    let guard = FileGuard::acquire(&guard_path)?;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let result = (|| {
        fs::write(&temp, text)?;
        replace_file(&temp, path)
    })();
    let _ = fs::remove_file(&temp);
    drop(guard);
    result
}
