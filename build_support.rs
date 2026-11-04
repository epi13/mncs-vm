//! Build-time helpers that resolve the source Cargo actually consumes.

use std::path::{Path, PathBuf};

/// Resolve one direct path dependency from this crate's Cargo manifest.
///
/// The VM's embedded build receipt must follow Cargo's selected checkout,
/// including an explicitly bound linked worktree. Assuming sibling repository
/// names can fingerprint different source from the code that was compiled.
pub fn path_dependency(manifest_dir: &Path, dependency: &str) -> PathBuf {
    let manifest_path = manifest_dir.join("Cargo.toml");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", manifest_path.display()));
    let manifest: toml::Value = toml::from_str(&manifest_text)
        .unwrap_or_else(|error| panic!("parse {}: {error}", manifest_path.display()));
    let dependency_spec = manifest
        .get("dependencies")
        .and_then(|dependencies| dependencies.get(dependency))
        .unwrap_or_else(|| panic!("Cargo.toml does not declare dependency {dependency:?}"));
    let relative = dependency_spec
        .get("path")
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| panic!("dependency {dependency:?} must use a local path"));
    manifest_dir
        .join(relative)
        .canonicalize()
        .unwrap_or_else(|error| panic!("resolve dependency {dependency:?}: {error}"))
}
