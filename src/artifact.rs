//! Canonical MNCS VM artifact contract (`mncs.vm.artifact/1`).
//!
//! The artifact is a frozen, self-describing executable container that can
//! be loaded and admitted without compiler internals. Identity is content:
//! `artifact_id` is the sha256 of the canonical serialization of every
//! other field, verified at admission so laundered identities fail closed.

use serde::{Deserialize, Serialize};

/// Canonical artifact schema identifier.
pub const ARTIFACT_SCHEMA_VERSION: &str = "mncs.vm.artifact/1";
/// VM contract revision this artifact was built for.
pub const VM_CONTRACT: &str = "mncs.vm/0.1";
/// SSA schema versions the v1 reference engine accepts in code sections.
pub const ACCEPTED_SSA_SCHEMAS: &[&str] = &["0.5"];

/// Where this artifact came from. Provenance, not authority: the VM
/// re-validates everything it enforces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSource {
    pub backend_name: String,
    pub backend_version: String,
    pub selected_ssa_identity: String,
    pub selected_ssa_fingerprint: String,
    pub target: serde_json::Value,
    #[serde(default)]
    pub lowering_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_identity: Option<String>,
}

/// One identity-bound callable entry in the artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallableEntry {
    /// Source-level module path (routing hint, never the identity).
    pub module: String,
    /// Source-level function name (routing hint, never the identity).
    pub name: String,
    /// Stable function identity. This is what execution binds to.
    pub function: String,
    /// Stable semantic identity of the callable, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<String>,
}

/// Executable code section. v1 carries the compiler-selected SSA the
/// artifact was lowered from; the engine executes SSA semantics under
/// VM-owned runtime rules (frames, metering, capabilities, outcomes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodeSection {
    /// Compiler-selected SSA, schema-checked at admission.
    MncsSelectedSsa {
        ssa_schema: String,
        module: mncs_model::SsaModule,
    },
}

/// A resource dimension the artifact declares for enforcement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundDecl {
    pub dimension: String,
    pub limit: u64,
}

/// What the artifact needs from the runtime environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRequirements {
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub bounds: Vec<BoundDecl>,
    pub vm_contract: String,
}

/// The canonical artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmArtifact {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub artifact_id: String,
    pub source: ArtifactSource,
    pub callables: Vec<CallableEntry>,
    pub code: CodeSection,
    pub requirements: ArtifactRequirements,
    #[serde(default)]
    pub exports: Vec<String>,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub unsupported: Vec<String>,
}

impl VmArtifact {
    /// Canonical bytes: deterministic serialization with the identity
    /// field blanked, so identity is content and nothing else.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut clone = self.clone();
        clone.artifact_id = String::new();
        serde_json::to_vec(&clone).expect("artifact is serializable")
    }

    /// Bind (or rebind) the content identity after construction.
    pub fn seal(mut self) -> Self {
        self.artifact_id = artifact_id_of(&self.canonical_bytes());
        self
    }

    /// Verify the sealed identity against current content.
    pub fn identity_is_valid(&self) -> bool {
        !self.artifact_id.is_empty() && self.artifact_id == artifact_id_of(&self.canonical_bytes())
    }
}

/// `sha256:<hex>` over canonical bytes.
pub fn artifact_id_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}
