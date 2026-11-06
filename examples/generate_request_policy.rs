//! Regenerate the embedded VM policy artifact from its canonical MNCS source.
//!
//! This intentionally emits but does not admit the artifact: the updated
//! artifact contains the policy that the runtime will use to admit it.

use std::path::Path;

const SOURCE: &str = include_str!("../native/mncs/vm/request_policy.mncs");
const LOCATOR: &str = "native/mncs/vm/request_policy.mncs";

fn main() {
    let artifact = mncs_vm::harness::compile_source_artifact(SOURCE, LOCATOR)
        .expect("native VM policy source compiles through the selected Language and Stdlib");
    let bytes = serde_json::to_vec(&artifact).expect("artifact serializes");
    let artifact_id = artifact
        .get("artifact_id")
        .and_then(serde_json::Value::as_str)
        .expect("compiler emitted a sealed artifact identity");
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("native/mncs/vm/request_policy.artifact.json");
    std::fs::write(&path, &bytes).expect("write generated VM policy artifact");
    println!(
        "{}",
        serde_json::json!({
            "artifact_id": artifact_id,
            "bytes": bytes.len(),
            "output": path,
            "source_locator": LOCATOR
        })
    );
}
