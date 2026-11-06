//! The checked-in MNCS admission policy is compiled from its source and
//! consumed by the ordinary frozen-artifact admission path.

const SOURCE: &str = include_str!("../native/mncs/vm/request_policy.mncs");
const LOCATOR: &str = "native/mncs/vm/request_policy.mncs";
const EMBEDDED: &[u8] = include_bytes!("../native/mncs/vm/request_policy.artifact.json");

#[test]
fn embedded_policy_artifact_matches_native_source() {
    let artifact = mncs_vm::harness::compile_source_artifact(SOURCE, LOCATOR)
        .expect("VM policy source compiles through Language and selected Stdlib");
    let bytes = serde_json::to_vec(&artifact).expect("compiled policy serializes");
    assert_eq!(bytes, EMBEDDED);
    assert!(artifact["artifact_id"]
        .as_str()
        .is_some_and(|identity| identity.starts_with("sha256:")));
    for function in [
        "classify_artifact_schema",
        "classify_vm_contract",
        "classify_bound_dimensions",
        "classify_ssa_schema",
    ] {
        assert!(artifact["callables"]
            .as_array()
            .is_some_and(|callables| callables
                .iter()
                .any(|callable| callable["name"] == function)));
    }
}
