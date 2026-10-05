//! Development and test harness: source file to admitted artifact.
//!
//! This is VM-side tooling, not compiler machinery. It drives the
//! read-only upstream pipeline (parse, compile, lower) in-process
//! and emits the canonical VM artifact with the compiler-owned emitter.
//! Corpus programs are self-contained (no `use` imports), so the
//! public null resolver is sufficient and no resolution semantics
//! are reimplemented here.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::admit::{AdmissionRefusal, Admitted};

/// Why the harness could not produce an admitted artifact.
#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("unreadable source: {0}")]
    Unreadable(String),
    #[error("source front end is invalid: {0}")]
    InvalidSource(String),
    #[error("compilation failed: {0}")]
    CompilationFailed(String),
    #[error("admission refused: {0}")]
    Refused(#[from] AdmissionRefusal),
}

/// Compile one self-contained `.mncs` source file through the
/// read-only upstream pipeline and admit the result into the VM.
/// `workspace_root` locates sibling checkouts (mncs-language) only
/// for diagnostics; compilation itself uses linked crates.
pub fn compile_file(path: &Path) -> Result<Admitted, HarnessError> {
    let source = std::fs::read_to_string(path)
        .map_err(|error| HarnessError::Unreadable(format!("{}: {error}", path.display())))?;
    compile_source(&source, &path.to_string_lossy())
}

/// Compile to the upstream program plus directly admitted VM artifact pair.
/// Exposed so differential tests can drive the read-only oracle
/// (`execute_ssa`) over the exact same compilation the VM admits.
pub fn compile_direct(
    source: &str,
    locator: &str,
) -> Result<(mncs_model::Program, Admitted), HarnessError> {
    compile_direct_seeded(source, locator, &[])
}

/// Seeded variant: host-requested generic instantiations are compiled
/// in, so the emitted artifact carries generic entrypoints for them.
pub fn compile_direct_seeded(
    source: &str,
    locator: &str,
    seeds: &[mncs_model::HostGenericSeedRequest],
) -> Result<(mncs_model::Program, Admitted), HarnessError> {
    let envelope = mncs_syntax::SourceEnvelope::inline(
        mncs_syntax::SourceArtifactKind::Program,
        locator,
        source.to_owned(),
    );
    let compiler = mncs_compiler::ReferenceCompiler::default();
    let front_end =
        compiler.front_end_with_resolver_and_seeds(envelope, &mncs_compiler::NullResolver, seeds);
    if !front_end.is_valid() {
        return Err(HarnessError::InvalidSource(format!(
            "{:?}",
            front_end.diagnostics
        )));
    }
    let program = front_end
        .program
        .ok_or_else(|| HarnessError::InvalidSource("no program elaborated".to_owned()))?;
    let artifact = direct_emitter::emit_vm_artifact(&compiler, &program)
        .map_err(HarnessError::CompilationFailed)?;
    let bytes = serde_json::to_vec(&artifact).expect("direct artifact serializes");
    let admitted = crate::admit::admit(&bytes)?;
    Ok((program, admitted))
}

/// Compile inline source with an explicit locator label.
pub fn compile_source(source: &str, locator: &str) -> Result<Admitted, HarnessError> {
    compile_direct(source, locator).map(|(_, admitted)| admitted)
}

/// Locate the workspace root from inside the mncs-vm checkout.
pub fn workspace_root() -> PathBuf {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root
}

/// Test/dev-only helpers. Linked into test targets; not part of the
/// runtime contract.
pub mod tests_only {
    use super::*;

    /// Compile a corpus file by name under tests/corpus.
    pub fn compile_corpus(name: &str) -> Admitted {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests");
        path.push("corpus");
        path.push(name);
        compile_file(&path).expect("corpus compiles and admits")
    }
}

// Development tooling consumes one compiler-owned emitter, never a second
// lowering architecture. Runtime load/admit needs only frozen bytes.
#[path = "../../mncs-compiler/tools/vm_emit.rs"]
mod direct_emitter;
