use crate::analyze_project;
use anyhow::{Context, bail};
use riddlec::pipeline;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;

pub(crate) fn run(root: &Path) -> anyhow::Result<()> {
    let analysis = analyze_project(root, &HashMap::new())?;
    let errors = riddlec::diagnostics::report_mapped(
        &analysis.result,
        &analysis.source,
        &analysis.entry.display().to_string(),
    );
    if errors > 0 || !analysis.result.success() {
        bail!("build failed");
    }

    let build_dir = root.join(".clue").join("build");
    fs::create_dir_all(&build_dir)?;
    let c_path = build_dir.join(format!("{}.c", analysis.package_name));
    let hash_path = build_dir.join(format!("{}.hash", analysis.package_name));
    let hash = fingerprint(&analysis.manifest_fingerprint, &analysis.source.source);
    if c_path.is_file() && fs::read_to_string(&hash_path).unwrap_or_default() == hash {
        println!("clue: fresh {}", c_path.display());
        return Ok(());
    }

    let module = analysis
        .result
        .mir_module
        .as_ref()
        .context("successful compilation did not produce MIR")?;
    let c_code = pipeline::generate_c(module).map_err(anyhow::Error::msg)?;
    fs::write(&c_path, c_code)?;
    fs::write(&hash_path, hash)?;
    println!("clue: built {}", c_path.display());
    Ok(())
}

fn fingerprint(manifest: &str, source: &str) -> String {
    let mut hasher = DefaultHasher::new();
    manifest.hash(&mut hasher);
    source.hash(&mut hasher);
    riddlec::GIT_HASH.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
