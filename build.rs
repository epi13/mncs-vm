//! Embed a local build-input receipt in the VM executable. This records
//! producer observations; it is not an independent build attestation.
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hash_file(path: &Path) -> String {
    digest(&std::fs::read(path).expect("read build input"))
}

fn resolve_executable(configured: &str) -> PathBuf {
    let path = PathBuf::from(configured);
    let resolved = if path.components().count() > 1 || path.is_absolute() {
        path.canonicalize().expect("canonical build tool path")
    } else {
        std::env::split_paths(&std::env::var_os("PATH").expect("build PATH"))
            .map(|directory| directory.join(&path))
            .find_map(|candidate| candidate.canonicalize().ok())
            .expect("resolve build tool through PATH")
    };
    resolved
}

fn executable_identity(name: &str) -> serde_json::Value {
    let configured = std::env::var(name).expect("selected build tool path");
    let path = resolve_executable(&configured);
    println!("cargo:rerun-if-env-changed={name}");
    println!("cargo:rerun-if-changed={}", path.display());
    serde_json::json!({
        "configured_path": configured,
        "resolved_path": path.display().to_string(),
        "sha256": hash_file(&path),
    })
}

fn collect(path: &Path, files: &mut BTreeSet<PathBuf>) {
    if path.is_dir() {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if matches!(name, "target" | ".git" | ".worktrees" | ".mncs") {
            return;
        }
        let mut children: Vec<_> = std::fs::read_dir(path)
            .expect("enumerate build input directory")
            .map(|entry| entry.expect("read build input entry").path())
            .collect();
        children.sort();
        for child in children {
            collect(&child, files);
        }
    } else if path.is_file() {
        files.insert(path.canonicalize().expect("canonicalize build input"));
    }
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    let vm_root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .canonicalize()
        .expect("canonical VM root");
    let workspace = vm_root.parent().expect("workspace parent");
    let language_root = workspace
        .join("mncs-language")
        .canonicalize()
        .expect("selected language source");
    let compiler_root = workspace
        .join("mncs-compiler")
        .canonicalize()
        .expect("selected compiler source");

    let roots = [
        ("mncs-vm", vm_root.clone()),
        ("mncs-language", language_root.clone()),
        ("mncs-compiler", compiler_root.clone()),
    ];
    let inputs = [
        vm_root.join("src"),
        vm_root.join("Cargo.toml"),
        vm_root.join("Cargo.lock"),
        vm_root.join("build.rs"),
        vm_root.join(".cargo/config.toml"),
        language_root.join("Cargo.toml"),
        language_root.join("Cargo.lock"),
        language_root.join("crates/mncs-model/src"),
        language_root.join("crates/mncs-model/Cargo.toml"),
        language_root.join("crates/mncs-compiler/src"),
        language_root.join("crates/mncs-compiler/Cargo.toml"),
        language_root.join("crates/mncs-syntax/src"),
        language_root.join("crates/mncs-syntax/Cargo.toml"),
        language_root.join("crates/mncs-codegen/src"),
        language_root.join("crates/mncs-codegen/Cargo.toml"),
        compiler_root.join("Cargo.toml"),
        compiler_root.join("tools/vm-artifact-codec/src"),
        compiler_root.join("tools/vm-artifact-codec/Cargo.toml"),
    ];
    let mut files = BTreeSet::new();
    for input in inputs {
        if input.exists() {
            collect(&input, &mut files);
        }
    }
    let source_inputs: BTreeMap<String, String> = files
        .iter()
        .map(|path| (path.display().to_string(), hash_file(path)))
        .collect();
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let mut revisions = BTreeMap::new();
    let mut dirty_inputs = BTreeMap::new();
    let mut closure = Vec::new();
    for (name, root) in &roots {
        let revision = git(root, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_owned());
        if let Some(head_path) = git(root, &["rev-parse", "--git-path", "HEAD"]) {
            let path = root.join(head_path);
            println!("cargo:rerun-if-changed={}", path.display());
            if let Some(reference) = git(root, &["symbolic-ref", "-q", "HEAD"])
                .and_then(|reference| git(root, &["rev-parse", "--git-path", &reference]))
            {
                println!("cargo:rerun-if-changed={}", root.join(reference).display());
            }
        }
        revisions.insert((*name).to_owned(), revision.clone());
        let status =
            git(root, &["status", "--porcelain=v1", "--untracked-files=all"]).unwrap_or_default();
        let changed: BTreeSet<String> = status
            .lines()
            .filter_map(|line| {
                (line.len() >= 4).then(|| {
                    line[3..]
                        .rsplit(" -> ")
                        .next()
                        .unwrap_or(&line[3..])
                        .to_owned()
                })
            })
            .collect();
        let prefix = format!("{}{}{}", root.display(), std::path::MAIN_SEPARATOR, "");
        let mut rows = BTreeMap::new();
        for (path, identity) in &source_inputs {
            let Some(relative) = path.strip_prefix(&prefix) else {
                continue;
            };
            rows.insert(relative.to_owned(), identity.clone());
            if changed.contains(relative) {
                dirty_inputs.insert(path.clone(), identity.clone());
            }
        }
        let identity =
            digest(&serde_json::to_vec(&rows).expect("serialize dependency input table"));
        closure.push(serde_json::json!({
            "repository": name,
            "checkout": root.display().to_string(),
            "revision": revision,
            "source_input_count": rows.len(),
            "source_inputs_identity": identity,
        }));
    }

    for key in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_TARGET_DIR",
        "CARGO_HOME",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let rustc_identity = executable_identity("RUSTC");
    let rustc_path = rustc_identity["configured_path"].as_str().unwrap();
    let rustc = Command::new(&rustc_path)
        .arg("-vV")
        .output()
        .expect("query selected rustc");
    let cargo_identity = executable_identity("CARGO");
    let cargo_path = cargo_identity["configured_path"].as_str().unwrap();
    let cargo = if cargo_path.is_empty() {
        String::new()
    } else {
        Command::new(&cargo_path)
            .arg("--version")
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .unwrap_or_default()
    };
    let features: BTreeMap<String, String> = std::env::vars()
        .filter(|(name, _)| name.starts_with("CARGO_FEATURE_"))
        .collect();
    let build_configuration = serde_json::json!({
        "profile": std::env::var("PROFILE").ok(),
        "target": std::env::var("TARGET").ok(),
        "opt_level": std::env::var("OPT_LEVEL").ok(),
        "debug": std::env::var("DEBUG").ok(),
        "rustflags": std::env::var("RUSTFLAGS").ok(),
        "encoded_rustflags": std::env::var("CARGO_ENCODED_RUSTFLAGS").ok(),
        "features": features,
        "cargo_path": cargo_path,
        "cargo_version": cargo,
        "toolchain_executables": {"cargo": cargo_identity, "rustc": rustc_identity},
        "rustc_path": rustc_path,
        "rustc_version": String::from_utf8_lossy(&rustc.stdout).trim(),
        "cargo_home": std::env::var("CARGO_HOME").ok(),
    });
    let inputs_identity =
        digest(&serde_json::to_vec(&source_inputs).expect("serialize source inputs"));
    let receipt = serde_json::json!({
        "schema_version": "mncs.vm-build-receipt/1",
        "repository": "mncs-vm",
        "producer_kind": "canonical-mncs-vm",
        "source_revisions": revisions,
        "dirty_content_identity": digest(&serde_json::to_vec(&dirty_inputs).expect("serialize dirty inputs")),
        "dirty_input_count": dirty_inputs.len(),
        "source_inputs": source_inputs,
        "source_inputs_identity": inputs_identity,
        "dependency_closure": closure,
        "build_configuration": build_configuration,
        "assurance": "local build observation; not independent attestation",
    });
    let receipt_bytes = serde_json::to_vec(&receipt).expect("serialize build receipt");
    let embedded = serde_json::json!({
        "identity": format!("sha256:{}", digest(&receipt_bytes)),
        "receipt": receipt,
    });
    let output = PathBuf::from(std::env::var("OUT_DIR").expect("build output dir"))
        .join("mncs-vm-build-receipt.json");
    std::fs::write(
        output,
        serde_json::to_vec(&embedded).expect("serialize embedded receipt"),
    )
    .expect("write embedded receipt");
}
