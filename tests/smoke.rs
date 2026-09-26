use mncs_vm::harness;

#[test]
fn corpus_compiles_and_admits() {
    for name in ["arith.mncs", "shapes.mncs", "fail.mncs"] {
        let admitted = harness::tests_only::compile_corpus(name);
        assert!(!admitted.artifact_id().is_empty(), "{name}");
        assert!(!admitted.artifact.callables.is_empty(), "{name}");
    }
}
