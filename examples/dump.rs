fn main() {
    let name = std::env::args().nth(1).unwrap();
    let path = format!("tests/corpus/{name}");
    let source = std::fs::read_to_string(&path).unwrap();
    let (_, backend) = mncs_vm::harness::compile_to_backend(&source, &name).unwrap();
    let admitted = mncs_vm::migrate::admit_research_artifact(&backend).unwrap();
    let module = admitted.ssa_module().unwrap();
    for function in &module.functions {
        if !function.bounded_iterations.is_empty() {
            println!("=== fn {}", function.semantic_identity.0);
            for region in &function.bounded_iterations {
                println!(
                    "  region {} bound={} header={} body={:?}",
                    region.identity.0,
                    region.bound,
                    region.header.0,
                    region
                        .body_blocks
                        .iter()
                        .map(|b| b.0.clone())
                        .collect::<Vec<_>>()
                );
            }
            for block in &function.blocks {
                println!("  block {}", block.identity.0);
            }
        }
    }
}
