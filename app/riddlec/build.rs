use std::{
    collections::HashSet,
    env, fs, io,
    path::{Path, PathBuf},
};

fn main() {
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_HASH={hash}");
    println!("cargo:rerun-if-changed=.git/HEAD");

    let mut files = Vec::new();
    let std = expand_module_file(
        Path::new("../../std/lib.rid"),
        &mut HashSet::new(),
        &mut files,
    )
    .expect("standard library should expand");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR should be set")).join("std.rid"),
        std,
    )
    .expect("expanded standard library should write");

    for file in files {
        println!("cargo:rerun-if-changed={}", file.display());
    }
}

fn expand_module_file(
    path: &Path,
    stack: &mut HashSet<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> io::Result<String> {
    let path = fs::canonicalize(path)?;
    if !stack.insert(path.clone()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "cyclic standard library module import involving `{}`",
                path.display()
            ),
        ));
    }

    files.push(path.clone());
    let source = fs::read_to_string(&path)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        if let Some((indent, visibility, name)) = external_mod(line) {
            let child = find_module_file(dir, &name)?;
            let child_source = expand_module_file(&child, stack, files)?;
            out.push_str(&format!(
                "{indent}{visibility}mod {name} {{\n{child_source}\n{indent}}}\n"
            ));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }

    stack.remove(&path);
    Ok(out)
}

fn external_mod(line: &str) -> Option<(String, &'static str, String)> {
    let indent_len = line.len() - line.trim_start().len();
    let indent = line[..indent_len].to_string();
    let trimmed = line.trim();
    let (visibility, rest) = trimmed
        .strip_prefix("pub mod ")
        .map(|rest| ("pub ", rest))
        .or_else(|| trimmed.strip_prefix("mod ").map(|rest| ("", rest)))?;
    let name = rest.strip_suffix(';')?.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return None;
    }
    Some((indent, visibility, name.to_string()))
}

fn find_module_file(dir: &Path, name: &str) -> io::Result<PathBuf> {
    let flat = dir.join(format!("{name}.rid"));
    if flat.is_file() {
        return Ok(flat);
    }

    let nested = dir.join(name).join("mod.rid");
    if nested.is_file() {
        return Ok(nested);
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "standard library module `{name}` not found; expected `{}` or `{}`",
            flat.display(),
            nested.display()
        ),
    ))
}
