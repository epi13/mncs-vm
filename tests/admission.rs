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
fn schema_refusal_keeps_precedence_over_identity() {
    let mut artifact = round_trip().artifact;
    artifact.schema_version = "mncs.vm.artifact/999".to_owned();
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
fn vm_contract_drift_is_incompatible_before_bound_checks() {
    let mut artifact = round_trip().artifact;
    artifact.requirements.vm_contract = "mncs.vm/999".to_owned();
    artifact.requirements.bounds.push(BoundDecl {
        dimension: "hyperdrive".to_owned(),
        limit: 3,
    });
    let refusal = admit_artifact(artifact.seal()).unwrap_err();
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
fn bounds_stream_in_order_past_one_native_packet() {
    let mut artifact = round_trip().artifact;
    artifact.requirements.bounds = (0..20)
        .map(|_| BoundDecl {
            dimension: "steps".to_owned(),
            limit: 3,
        })
        .collect();
    artifact.requirements.bounds.push(BoundDecl {
        dimension: "hyperdrive".to_owned(),
        limit: 3,
    });
    let refusal = admit_artifact(artifact.seal()).unwrap_err();
    assert!(
        matches!(
            refusal,
            mncs_vm::admit::AdmissionRefusal::UnsupportedFeature { ref feature }
                if feature == "bound-dimension:hyperdrive"
        ),
        "{refusal:?}"
    );
}

#[test]
fn long_bound_dimension_is_refused_without_truncating_diagnostic() {
    let mut artifact = round_trip().artifact;
    let dimension = "x".repeat(80);
    artifact.requirements.bounds.push(BoundDecl {
        dimension: dimension.clone(),
        limit: 3,
    });
    let refusal = admit_artifact(artifact.seal()).unwrap_err();
    assert!(
        matches!(
            refusal,
            mncs_vm::admit::AdmissionRefusal::UnsupportedFeature { ref feature }
                if feature == &format!("bound-dimension:{dimension}")
        ),
        "{refusal:?}"
    );
}

#[test]
fn ssa_schema_is_native_classified_after_bounds() {
    let mut artifact = round_trip().artifact;
    let CodeSection::MncsSelectedSsa { ssa_schema, .. } = &mut artifact.code;
    *ssa_schema = "0.99".to_owned();
    artifact = artifact.seal();
    let refusal = admit_artifact(artifact).unwrap_err();
    assert!(
        matches!(
            refusal,
            mncs_vm::admit::AdmissionRefusal::UnsupportedFeature { ref feature }
                if feature == "ssa-schema:0.99"
        ),
        "{refusal:?}"
    );
}

#[test]
fn both_declared_ssa_schemas_remain_admissible() {
    for schema in ["0.5", "0.6"] {
        let mut artifact = round_trip().artifact;
        let CodeSection::MncsSelectedSsa { ssa_schema, .. } = &mut artifact.code;
        *ssa_schema = schema.to_owned();
        let admitted = admit_artifact(artifact.seal()).expect("declared SSA schema accepted");
        assert_eq!(
            match &admitted.artifact.code {
                CodeSection::MncsSelectedSsa { ssa_schema, .. } => ssa_schema,
            },
            schema
        );
    }
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
        test_binding: None,
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
        test_binding: None,
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
    assert_eq!(source.backend_name, "mncs-vm-direct");
    assert_eq!(admitted.artifact.schema_version, ARTIFACT_SCHEMA_VERSION);
    assert_eq!(admitted.artifact.requirements.vm_contract, VM_CONTRACT);
    let _needs: &ArtifactRequirements = &admitted.artifact.requirements;
    let _code: &CodeSection = &admitted.artifact.code;
}
