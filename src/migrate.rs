//! Temporary migration adapter: research-bytecode payloads in,
//! canonical artifacts out.
//!
//! The current compiler emits `research_bytecode` backend artifacts
//! whose payload embeds the full `Program` plus `SsaModule`. That
//! envelope is compiler internals, not a VM contract. This adapter
//! translates it at the boundary into [`VmArtifact`]:
//!
//! - verifies the upstream artifact (kind, status, identity);
//! - lifts the selected SSA into the code section verbatim
//!   (meaning stays upstream-owned; the VM executes it under
//!   VM-owned runtime rules);
//! - builds the identity-bound callable table from exports;
//! - carries declared bounds, capabilities, assumptions, and
//!   provenance refs without reinterpreting them.
//!
//! TEMPORARY. Removal condition: `mncs-compiler` emits
//! `mncs.vm.artifact/1` directly (pressure P-VM-COMPILER-001).
//! The adapter never synthesizes semantics: anything it cannot
//! translate is a refusal, never a guess.

use mncs_model::BackendArtifact;

use crate::admit::{AdmissionRefusal, Admitted};
use crate::artifact::{
    ArtifactRequirements, ArtifactSource, BoundDecl, CallableEntry, CodeSection, GenericEntrypoint,
    VmArtifact, ARTIFACT_SCHEMA_VERSION, VM_CONTRACT,
};

/// Upstream identifiers this adapter accepts.
pub const RESEARCH_ARTIFACT_KIND: &str = "research_bytecode";
pub const RESEARCH_BACKEND_NAME: &str = "mncs-research-bytecode";

/// Translate one research-bytecode backend artifact into a sealed
/// canonical artifact. Pure function of the input bytes.
pub fn translate_research_artifact(
    backend: &BackendArtifact,
) -> Result<VmArtifact, AdmissionRefusal> {
    if backend.artifact_kind != RESEARCH_ARTIFACT_KIND {
        return Err(AdmissionRefusal::Malformed {
            reason: format!(
                "not a research-bytecode artifact: {}",
                backend.artifact_kind
            ),
        });
    }
    if backend.backend.name != RESEARCH_BACKEND_NAME {
        return Err(AdmissionRefusal::IncompatibleContract {
            detail: format!(
                "backend {} is not the research backend",
                backend.backend.name
            ),
        });
    }
    if !backend.identity_is_valid() {
        return Err(AdmissionRefusal::IdentityMismatch);
    }
    if backend.status != mncs_model::TransformationStatus::Pass {
        return Err(AdmissionRefusal::UnresolvedFact {
            fact: format!("backend lowering did not pass: {:?}", backend.status),
        });
    }
    let bytes = backend
        .bytes()
        .map_err(|reason| AdmissionRefusal::Malformed {
            reason: format!("artifact payload undecodable: {reason}"),
        })?;
    let payload: ResearchPayload =
        serde_json::from_slice(&bytes).map_err(|error| AdmissionRefusal::Malformed {
            reason: format!("research payload does not parse: {error}"),
        })?;
    if payload.schema_version != "0.1" {
        return Err(AdmissionRefusal::UnsupportedFeature {
            feature: format!("research-payload:{}", payload.schema_version),
        });
    }
    if !payload.ssa.identity_is_valid() {
        return Err(AdmissionRefusal::UnresolvedFact {
            fact: "embedded ssa module identity is invalid".to_owned(),
        });
    }
    let mut callables: Vec<CallableEntry> = Vec::with_capacity(backend.exports.len());
    let mut unsupported: Vec<String> = backend.unsupported.clone();
    for name in &backend.exports {
        match resolve_export(&payload, name)? {
            Some(entry) => callables.push(entry),
            None => unsupported.push(format!("export {name}: no ssa instance")),
        }
    }
    // Compiler-determined generic instantiations: each upstream row
    // already names the concrete (module, function) the backend
    // emitted, so the adapter only binds it to the SSA instance
    // identity execution uses. Unresolvable rows are recorded, never
    // guessed, mirroring uninstantiated exports.
    let mut generic_entrypoints = Vec::with_capacity(backend.generic_entrypoints.len());
    for row in &backend.generic_entrypoints {
        match resolve_generic_entry(&payload, row)? {
            Some(entry) => generic_entrypoints.push(entry),
            None => unsupported.push(format!(
                "generic {}::{}({}): no ssa instance",
                row.generic_module,
                row.generic_function,
                row.args_spellings.join(", ")
            )),
        }
    }
    generic_entrypoints.sort_by(|left: &GenericEntrypoint, right: &GenericEntrypoint| {
        (
            &left.generic_module,
            &left.generic_function,
            &left.args_spellings,
            &left.canonical_args,
            &left.function,
        )
            .cmp(&(
                &right.generic_module,
                &right.generic_function,
                &right.args_spellings,
                &right.canonical_args,
                &right.function,
            ))
    });
    let mut capabilities: Vec<String> = payload
        .ssa
        .functions
        .iter()
        .flat_map(|function| {
            function.blocks.iter().flat_map(|block| {
                block.instructions.iter().flat_map(|instruction| {
                    instruction
                        .capability_uses
                        .iter()
                        .map(|use_| use_.capability.0.clone())
                })
            })
        })
        .collect();
    capabilities.sort();
    capabilities.dedup();
    // Region bounds ride per-region inside the code section, where the
    // engine enforces them directly; they are not envelope dimensions
    // and are not duplicated here.
    let bounds: Vec<BoundDecl> = Vec::new();
    let artifact = VmArtifact {
        schema_version: ARTIFACT_SCHEMA_VERSION.to_owned(),
        artifact_id: String::new(),
        source: ArtifactSource {
            backend_name: backend.backend.name.clone(),
            backend_version: backend.backend.version.clone(),
            selected_ssa_identity: backend.input.identity.0.clone(),
            selected_ssa_fingerprint: backend.input.fingerprint.clone(),
            target: serde_json::to_value(&backend.target).unwrap_or(serde_json::Value::Null),
            lowering_refs: backend
                .evidence_dependencies
                .iter()
                .map(|id| id.0.clone())
                .collect(),
            interface_identity: backend.interface_identity.clone(),
        },
        callables,
        generic_entrypoints,
        code: CodeSection::MncsSelectedSsa {
            ssa_schema: payload.ssa.schema_version.clone(),
            module: payload.ssa,
        },
        requirements: ArtifactRequirements {
            capabilities,
            bounds,
            vm_contract: VM_CONTRACT.to_owned(),
        },
        exports: backend.exports.clone(),
        assumptions: backend.assumptions.clone(),
        unsupported,
    };
    Ok(artifact.seal())
}

/// Translate and admit in one step.
pub fn admit_research_artifact(backend: &BackendArtifact) -> Result<Admitted, AdmissionRefusal> {
    let artifact = translate_research_artifact(backend)?;
    crate::admit::admit_artifact(artifact)
}

/// The research payload envelope: program plus SSA. Only the SSA
/// crosses into the canonical artifact; the program stays behind
/// as lowering provenance (its fingerprint is already bound into
/// the selected-SSA identity upstream).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ResearchPayload {
    schema_version: String,
    program: serde_json::Value,
    ssa: mncs_model::SsaModule,
}

/// Resolve one export name to an identity-bound callable entry.
///
/// Mirrors the oracle's own target resolution (`SsaExecutionSession`):
/// the payload program routes `name` through its home-module
/// namespace, the public `function_id` derivation binds it, and the
/// SSA semantic identity confirms it. Only routing data crosses this
/// boundary; program semantics stay upstream. Ambiguous routes are
/// refusals, never guesses; names with no SSA instance (generic
/// exports without a seeded monomorphization) resolve to `None` so
/// the caller can record them as explicitly unsupported instead of
/// refusing the whole artifact.
fn resolve_export(
    payload: &ResearchPayload,
    name: &str,
) -> Result<Option<CallableEntry>, AdmissionRefusal> {
    let program_module = payload
        .program
        .get("module")
        .and_then(|module| module.as_str())
        .unwrap_or_default();
    let mut namespaces = Vec::new();
    if let Some(functions) = payload.program.get("functions").and_then(|f| f.as_array()) {
        for function in functions {
            let matches = function
                .get("name")
                .and_then(|name_value| name_value.as_str())
                .is_some_and(|candidate| candidate == name);
            if !matches {
                continue;
            }
            let namespace = function
                .get("home_module")
                .and_then(|home| home.as_str())
                .unwrap_or(program_module);
            if !namespaces.contains(&namespace.to_owned()) {
                namespaces.push(namespace.to_owned());
            }
        }
    }
    let mut candidates = Vec::new();
    for namespace in &namespaces {
        let id = mncs_model::function_id(namespace, name);
        for function in &payload.ssa.functions {
            if function.semantic_identity == id {
                candidates.push((namespace.clone(), function.identity.0.clone()));
            }
        }
    }
    match candidates.len() {
        1 => {
            let (namespace, function) = candidates.pop().unwrap_or_default();
            Ok(Some(CallableEntry {
                module: namespace,
                name: name.to_owned(),
                function,
                semantic: None,
            }))
        }
        0 => Ok(None),
        _ => Err(AdmissionRefusal::UnresolvedFact {
            fact: format!("export {name} is ambiguous across functions"),
        }),
    }
}

/// Bind one upstream generic-instantiation row to its SSA instance.
///
/// The compiler already selected the concrete entry; this only
/// resolves it through the same public identity derivation the
/// export path uses. Ambiguity is a refusal; a missing instance
/// resolves to `None` for explicit recording.
fn resolve_generic_entry(
    payload: &ResearchPayload,
    row: &mncs_model::GenericEntrypointRecord,
) -> Result<Option<GenericEntrypoint>, AdmissionRefusal> {
    let id = mncs_model::function_id(&row.entry_module, &row.entry_function);
    let mut candidates = Vec::new();
    for function in &payload.ssa.functions {
        if function.semantic_identity == id {
            candidates.push(function.identity.0.clone());
        }
    }
    match candidates.len() {
        1 => Ok(Some(GenericEntrypoint {
            generic_module: row.generic_module.clone(),
            generic_function: row.generic_function.clone(),
            args_spellings: row.args_spellings.clone(),
            canonical_args: row.canonical_args.clone(),
            function: candidates.pop().unwrap_or_default(),
        })),
        0 => Ok(None),
        _ => Err(AdmissionRefusal::UnresolvedFact {
            fact: format!(
                "generic {}::{} entry {}::{} is ambiguous across ssa instances",
                row.generic_module, row.generic_function, row.entry_module, row.entry_function
            ),
        }),
    }
}
