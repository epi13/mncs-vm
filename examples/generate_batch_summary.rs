//! Regenerate the checked-in native VM batch-summary artifact from its
//! canonical MNCS source using the VM's compiler-owned harness.

use std::path::Path;

const SOURCE: &str = include_str!("../native/mncs/vm/batch_summary.mncs");
const LOCATOR: &str = "native/mncs/vm/batch_summary.mncs";

fn main() {
    let admitted = mncs_vm::harness::compile_source(SOURCE, LOCATOR)
        .expect("native batch-summary source compiles and admits");
    let bytes = serde_json::to_vec(&admitted.artifact).expect("artifact serializes");
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("native/mncs/vm/batch_summary.artifact.json");
    std::fs::write(&path, &bytes).expect("write generated batch-summary artifact");
    println!(
        "{}",
        serde_json::json!({
            "artifact_id": admitted.artifact_id(),
            "bytes": bytes.len(),
            "output": path,
            "source_locator": LOCATOR
        })
    );
}
