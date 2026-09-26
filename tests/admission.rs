//! Admission boundary: valid artifacts admit, everything else is a
//! typed refusal. Tampering, version drift, and dangling references
//! all fail closed with distinct outcomes.

use mncs_vm::admit::{admit, admit_artifact, Admitted};
use mncs_vm::artifact::{
    ArtifactRequirements, ArtifactSource, BoundDecl, CallableEntry, CodeSection,
    ARTIFACT_SCHEMA_VERSION, VM_CONTRACT,
};
use mncs_vm::harness::tests_only::compile_corpus;

fn round_trip() -> Admitted {
    let admitted = compile_corpus("arith.mncs");
    let bytes = serde_json::to_vec(&admitted.artifact).unwrap();
    admit(&bytes).expect("frozen artifact re-admits")
}

#[test]
fn valid_artifact_round_trips() {
    let admitted = round_trip();
    assert!(!admitted.artifact.callables.is_empty());
    assert!(admitted
        .callable_by_name("mncs.vmcorpus.arith.v1", "add2")
        .is_some());
    assert!(admitted.ssa_module().is_some());
}

#[test]
fn garbage_is_malformed() {
    let refusal = admit(b"not json at all").unwrap_err();
    assert!(matches!(
        refusal,
        mncs_vm::admit::AdmissionRefusal::Malformed { .. }
    ));
}

#[test]
fn schema_drift_is_incompatible() {
    let admitted = compile_corpus("arith.mncs");
    let mut artifact = admitted.artifact.clone();
    artifact.schema_version = "mncs.vm.artifact/999".to_owned();
    artifact = artifact.seal();
    let refusal = admit_artifact(artifact).unwrap_err();
    assert!(
        matches!(
            refusal,
            mncs_vm::admit::AdmissionRefusal::IncompatibleContract { .. }
        ),
        "{refusal:?}"
    );
}

#[test]
fn tampered_identity_is_rejected() {
    let admitted = compile_corpus("arith.mncs");
    let mut artifact = admitted.artifact.clone();
    artifact.exports.push("forged::entry".to_owned());
    // Identity left as sealed: content no longer matches.
    let refusal = admit_artifact(artifact).unwrap_err();
    assert!(
        matches!(refusal, mncs_vm::admit::AdmissionRefusal::IdentityMismatch),
        "{refusal:?}"
    );
}

#[test]
fn unknown_bound_dimension_is_unsupported() {
    let admitted = compile_corpus("arith.mncs");
    let mut artifact = admitted.artifact.clone();
    artifact.requirements.bounds.push(BoundDecl {
        dimension: "hyperdrive".to_owned(),
        limit: 3,
    });
    artifact = artifact.seal();
    let refusal = admit_artifact(artifact).unwrap_err();
    assert!(
        matches!(
            refusal,
            mncs_vm::admit::AdmissionRefusal::UnsupportedFeature { .. }
        ),
        "{refusal:?}"
    );
}

#[test]
fn dangling_callable_is_unresolved() {
    let admitted = compile_corpus("arith.mncs");
    let mut artifact = admitted.artifact.clone();
    artifact.callables.push(CallableEntry {
        module: "ghost".to_owned(),
        name: "missing".to_owned(),
        function: "mncs:0.2:function:ghost::missing".to_owned(),
        semantic: None,
    });
    artifact = artifact.seal();
    let refusal = admit_artifact(artifact).unwrap_err();
    assert!(
        matches!(
            refusal,
            mncs_vm::admit::AdmissionRefusal::UnresolvedFact { .. }
        ),
        "{refusal:?}"
    );
}

#[test]
fn empty_function_identity_is_unresolved() {
    let admitted = compile_corpus("arith.mncs");
    let mut artifact = admitted.artifact.clone();
    artifact.callables.push(CallableEntry {
        module: "ghost".to_owned(),
        name: "missing".to_owned(),
        function: String::new(),
        semantic: None,
    });
    artifact = artifact.seal();
    let refusal = admit_artifact(artifact).unwrap_err();
    assert!(
        matches!(
            refusal,
            mncs_vm::admit::AdmissionRefusal::UnresolvedFact { .. }
        ),
        "{refusal:?}"
    );
}

#[test]
fn source_and_requirements_survive() {
    let admitted = compile_corpus("arith.mncs");
    let source: &ArtifactSource = &admitted.artifact.source;
    assert_eq!(source.backend_name, "mncs-research-bytecode");
    assert_eq!(admitted.artifact.schema_version, ARTIFACT_SCHEMA_VERSION);
    assert_eq!(admitted.artifact.requirements.vm_contract, VM_CONTRACT);
    let _needs: &ArtifactRequirements = &admitted.artifact.requirements;
    let _code: &CodeSection = &admitted.artifact.code;
}
