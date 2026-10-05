//! Artifact loading and runtime admission.
//!
//! Decoding is not authorization. `admit` parses a frozen artifact,
//! verifies its content identity, checks contract compatibility, and
//! resolves every callable entry against the code section before any
//! execution exists. Every refusal is typed; nothing collapses into
//! a stringly exit code.

use std::collections::BTreeMap;
use thiserror::Error;

use crate::artifact::{
    CodeSection, VmArtifact, ACCEPTED_SSA_SCHEMAS, ARTIFACT_SCHEMA_VERSION, VM_CONTRACT,
};

/// Why an artifact was refused admission.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AdmissionRefusal {
    /// Bytes do not parse as a VM artifact, or a structural invariant
    /// (duplicate callable identity, empty module/name) is violated.
    #[error("malformed artifact: {reason}")]
    Malformed { reason: String },
    /// The artifact needs executable content the v1 engine does not
    /// implement (unknown code kind, SSA schema, bound dimension).
    #[error("unsupported feature: {feature}")]
    UnsupportedFeature { feature: String },
    /// A runtime fact the VM requires is absent (callable entry with
    /// no matching function, invalid SSA identity).
    #[error("unresolved fact: {fact}")]
    UnresolvedFact { fact: String },
    /// Envelope contract mismatch (artifact schema, VM contract).
    #[error("incompatible contract: {detail}")]
    IncompatibleContract { detail: String },
    /// Content identity does not match sealed `artifact_id`.
    #[error("artifact identity mismatch")]
    IdentityMismatch,
}

/// Resource dimensions the v1 engine enforces.
pub const ENFORCED_BOUNDS: &[&str] = &[
    "steps",
    "call_depth",
    "memory_cells",
    "effects",
    "iterations",
];

/// An admitted artifact: frozen content plus resolved indexes.
/// The engine executes from this, never from raw bytes.
#[derive(Debug, Clone)]
pub struct Admitted {
    pub artifact: VmArtifact,
    /// (module, name) routing hint -> callable index.
    pub by_name: BTreeMap<(String, String), usize>,
    /// Stable function identity -> callable index. Execution binds here.
    pub by_function: BTreeMap<String, usize>,
    /// SSA function index by function identity.
    pub ssa_functions: BTreeMap<String, usize>,
    /// (generic module, generic name, normalized spellings) ->
    /// generic-entrypoint index. Generic calls bind here.
    pub generic_entries: BTreeMap<(String, String, Vec<String>), usize>,
    /// Immutable code routing built once at admission, shared by every call.
    pub(crate) execution_functions: BTreeMap<String, usize>,
    pub(crate) execution_blocks: Vec<BTreeMap<String, usize>>,
    /// Strictly nested regions reset at each outer activation.
    pub(crate) execution_nested_iterations: Vec<Vec<Vec<usize>>>,
}

impl Admitted {
    pub fn artifact_id(&self) -> &str {
        &self.artifact.artifact_id
    }

    pub fn callable_by_name(&self, module: &str, name: &str) -> Option<usize> {
        self.by_name
            .get(&(module.to_owned(), name.to_owned()))
            .copied()
    }

    pub fn callable_by_function(&self, function: &str) -> Option<usize> {
        self.by_function.get(function).copied()
    }

    pub fn generic_entry(&self, module: &str, name: &str, spellings: &[String]) -> Option<usize> {
        self.generic_entries
            .get(&(module.to_owned(), name.to_owned(), spellings.to_vec()))
            .copied()
    }

    /// Whether the artifact realizes any instantiation of a generic
    /// function, for precise call-boundary refusals.
    pub fn has_generic(&self, module: &str, name: &str) -> bool {
        self.generic_entries
            .keys()
            .any(|(generic_module, generic_function, _)| {
                generic_module == module && generic_function == name
            })
    }

    /// Normalized spellings of every realized instantiation of a
    /// generic function, for refusal diagnostics.
    pub fn generic_spellings(&self, module: &str, name: &str) -> Vec<Vec<String>> {
        let mut spellings: Vec<Vec<String>> = self
            .generic_entries
            .keys()
            .filter(|(generic_module, generic_function, _)| {
                generic_module == module && generic_function == name
            })
            .map(|(_, _, row)| row.clone())
            .collect();
        spellings.sort();
        spellings
    }

    /// Borrow the admitted SSA module. Fails closed when the code
    /// section is not SSA (today it always is; the match keeps the
    /// engine honest as new code kinds arrive).
    pub fn ssa_module(&self) -> Option<&mncs_model::SsaModule> {
        match &self.artifact.code {
            CodeSection::MncsSelectedSsa { module, .. } => Some(module),
        }
    }
}

/// Admit frozen artifact bytes. Pure: no execution, no providers.
///
/// Parses in two steps (text to `Value`, then typed): the JSON text
/// parser cannot target 128-bit integers without an extra
/// representation feature, while the `Value` deserializer widens
/// in-range numbers exactly. Out-of-range integers fail here as
/// `Malformed` — a refusal, never silent truncation. See
/// pressures/P-VM-ARTIFACT-007: frozen interchange needs a
/// bignum-exact encoding owned by the artifact schema.
pub fn admit(bytes: &[u8]) -> Result<Admitted, AdmissionRefusal> {
    admit_artifact(decode_artifact(bytes)?)
}

/// Own a file/transport buffer so it can be released immediately after
/// decoding, before identity validation builds temporary canonical tables.
/// The decode and admission contracts are the same as borrowed `admit`.
pub fn admit_owned(bytes: Vec<u8>) -> Result<Admitted, AdmissionRefusal> {
    let artifact = decode_artifact(&bytes)?;
    drop(bytes);
    admit_artifact(artifact)
}

fn decode_artifact(bytes: &[u8]) -> Result<VmArtifact, AdmissionRefusal> {
    // Fast path: decode straight into the artifact (one JSON pass, no
    // intermediate Value). Only failures fall back to the two-step
    // diagnosis so refusal reasons stay exactly as before.
    match serde_json::from_slice::<VmArtifact>(bytes) {
        Ok(artifact) => Ok(artifact),
        Err(_) => {
            let raw: serde_json::Value =
                serde_json::from_slice(bytes).map_err(|error| AdmissionRefusal::Malformed {
                    reason: format!("artifact JSON does not parse: {error}"),
                })?;
            let artifact: VmArtifact =
                serde_json::from_value(raw).map_err(|error| AdmissionRefusal::Malformed {
                    reason: format!("artifact JSON does not decode: {error}"),
                })?;
            Ok(artifact)
        }
    }
}

/// Admit an already-parsed artifact value.
pub fn admit_artifact(artifact: VmArtifact) -> Result<Admitted, AdmissionRefusal> {
    if artifact.schema_version != ARTIFACT_SCHEMA_VERSION {
        return Err(AdmissionRefusal::IncompatibleContract {
            detail: format!(
                "artifact schema {} != {}",
                artifact.schema_version, ARTIFACT_SCHEMA_VERSION
            ),
        });
    }
    if !artifact.identity_is_valid() {
        return Err(AdmissionRefusal::IdentityMismatch);
    }
    if artifact.requirements.vm_contract != VM_CONTRACT {
        return Err(AdmissionRefusal::IncompatibleContract {
            detail: format!(
                "vm contract {} != {}",
                artifact.requirements.vm_contract, VM_CONTRACT
            ),
        });
    }
    for bound in &artifact.requirements.bounds {
        if !ENFORCED_BOUNDS.contains(&bound.dimension.as_str()) {
            return Err(AdmissionRefusal::UnsupportedFeature {
                feature: format!("bound-dimension:{}", bound.dimension),
            });
        }
    }
    let ssa = match &artifact.code {
        CodeSection::MncsSelectedSsa { ssa_schema, module } => {
            if !ACCEPTED_SSA_SCHEMAS.contains(&ssa_schema.as_str()) {
                return Err(AdmissionRefusal::UnsupportedFeature {
                    feature: format!("ssa-schema:{ssa_schema}"),
                });
            }
            if !module.identity_is_valid() {
                return Err(AdmissionRefusal::UnresolvedFact {
                    fact: "ssa module identity is invalid".to_owned(),
                });
            }
            module
        }
    };
    let mut ssa_functions = BTreeMap::new();
    for (index, function) in ssa.functions.iter().enumerate() {
        let id = function.identity.0.clone();
        if id.trim().is_empty() {
            return Err(AdmissionRefusal::Malformed {
                reason: "ssa function with empty identity".to_owned(),
            });
        }
        if ssa_functions.insert(id, index).is_some() {
            return Err(AdmissionRefusal::Malformed {
                reason: "duplicate ssa function identity".to_owned(),
            });
        }
    }
    for function in &ssa.functions {
        let blocks: std::collections::BTreeSet<&str> = function
            .blocks
            .iter()
            .map(|block| block.identity.0.as_str())
            .collect();
        for region in &function.bounded_iterations {
            if !blocks.contains(region.header.0.as_str()) {
                return Err(AdmissionRefusal::UnresolvedFact {
                    fact: format!(
                        "iteration region {} names unknown header",
                        region.identity.0
                    ),
                });
            }
            for body in &region.body_blocks {
                if !blocks.contains(body.0.as_str()) {
                    return Err(AdmissionRefusal::UnresolvedFact {
                        fact: format!(
                            "iteration region {} names unknown body block",
                            region.identity.0
                        ),
                    });
                }
            }
        }
    }
    let mut by_name = BTreeMap::new();
    let mut by_function = BTreeMap::new();
    for (index, callable) in artifact.callables.iter().enumerate() {
        if callable.module.trim().is_empty() || callable.name.trim().is_empty() {
            return Err(AdmissionRefusal::Malformed {
                reason: "callable entry with empty module/name".to_owned(),
            });
        }
        if callable.function.trim().is_empty() {
            return Err(AdmissionRefusal::UnresolvedFact {
                fact: format!(
                    "callable {}::{} has no function identity",
                    callable.module, callable.name
                ),
            });
        }
        if !ssa_functions.contains_key(&callable.function) {
            return Err(AdmissionRefusal::UnresolvedFact {
                fact: format!(
                    "callable {}::{} names unknown function {}",
                    callable.module, callable.name, callable.function
                ),
            });
        }
        if by_function
            .insert(callable.function.clone(), index)
            .is_some()
        {
            return Err(AdmissionRefusal::Malformed {
                reason: format!("duplicate callable function {}", callable.function),
            });
        }
        by_name.insert((callable.module.clone(), callable.name.clone()), index);
    }
    let mut generic_entries = BTreeMap::new();
    for (index, entry) in artifact.generic_entrypoints.iter().enumerate() {
        if entry.generic_module.trim().is_empty() || entry.generic_function.trim().is_empty() {
            return Err(AdmissionRefusal::Malformed {
                reason: "generic entrypoint with empty module/name".to_owned(),
            });
        }
        if entry.args_spellings.is_empty()
            || entry
                .args_spellings
                .iter()
                .any(|spelling| spelling.trim().is_empty())
        {
            return Err(AdmissionRefusal::Malformed {
                reason: format!(
                    "generic entrypoint {}::{} has no argument spellings",
                    entry.generic_module, entry.generic_function
                ),
            });
        }
        if entry.canonical_args.trim().is_empty() {
            return Err(AdmissionRefusal::Malformed {
                reason: format!(
                    "generic entrypoint {}::{} has no specialization identity",
                    entry.generic_module, entry.generic_function
                ),
            });
        }
        if entry.function.trim().is_empty() {
            return Err(AdmissionRefusal::UnresolvedFact {
                fact: format!(
                    "generic entrypoint {}::{} has no function identity",
                    entry.generic_module, entry.generic_function
                ),
            });
        }
        if !ssa_functions.contains_key(&entry.function) {
            return Err(AdmissionRefusal::UnresolvedFact {
                fact: format!(
                    "generic entrypoint {}::{} names unknown function {}",
                    entry.generic_module, entry.generic_function, entry.function
                ),
            });
        }
        if generic_entries
            .insert(
                (
                    entry.generic_module.clone(),
                    entry.generic_function.clone(),
                    entry.args_spellings.clone(),
                ),
                index,
            )
            .is_some()
        {
            return Err(AdmissionRefusal::Malformed {
                reason: format!(
                    "duplicate generic entrypoint {}::{}({})",
                    entry.generic_module,
                    entry.generic_function,
                    entry.args_spellings.join(", ")
                ),
            });
        }
    }
    let mut execution_functions = BTreeMap::new();
    let mut execution_blocks = Vec::with_capacity(ssa.functions.len());
    for (index, function) in ssa.functions.iter().enumerate() {
        execution_functions.insert(function.identity.0.clone(), index);
        execution_functions.insert(function.semantic_identity.0.clone(), index);
        execution_blocks.push(
            function
                .blocks
                .iter()
                .enumerate()
                .map(|(i, b)| (b.identity.0.clone(), i))
                .collect(),
        );
    }
    let execution_nested_iterations = ssa
        .functions
        .iter()
        .map(|function| {
            function
                .bounded_iterations
                .iter()
                .map(|region| {
                    function
                        .bounded_iterations
                        .iter()
                        .enumerate()
                        .filter(|(_, other)| {
                            other.identity != region.identity
                                && region
                                    .body_blocks
                                    .iter()
                                    .any(|body| body.0 == other.body_entry.0)
                        })
                        .map(|(i, _)| i)
                        .collect()
                })
                .collect()
        })
        .collect();
    Ok(Admitted {
        artifact,
        by_name,
        by_function,
        ssa_functions,
        generic_entries,
        execution_functions,
        execution_blocks,
        execution_nested_iterations,
    })
}
