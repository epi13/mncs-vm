# P-VM-COMPILER-001 — no direct canonical artifact emission

- **ID:** P-VM-COMPILER-001
- **Owner repository:** `epi13/mncs-compiler` (with `epi13/mncs-language` backend registry)
- **Observed from:** `mncs-codegen` research-bytecode adapter (`crates/mncs-codegen/src/lib.rs`, `ResearchBytecodePayload`, schema `0.1`)
- **Current upstream behavior:** the only executable artifact the stack emits is a `research_bytecode` backend artifact whose payload embeds the full source `Program` plus the `SsaModule` as one JSON document (`application/vnd.mncs.research-bytecode+json; version=0.1`).
- **Required runtime behavior:** the compiler emits `mncs.vm.artifact/1` directly: content-identified, code-only executable section, identity-bound callable table, declared bounds/capabilities, lowering provenance — without compiler internals.
- **Why current contract is insufficient:** the VM cannot treat compiler memory as a frozen artifact. Every consumer must understand `Program` layout to reach the code, so the "artifact" is really a compiler session dump, and two runtimes (embedded interpreter, VM) both parse compiler internals with no shared executable contract between them.
- **Smallest general missing capability:** a compiler-owned lowering from selected SSA to the VM artifact schema, reusing the existing identity/provenance machinery (`BackendArtifact` already carries identity, fingerprint, evidence refs, exports, assumptions).
- **Required upstream change:** implement `mncs.vm.artifact/1` emission in the compiler (new backend or research-backend evolution), with a conformance vector showing artifact equivalence across compilations of the same source.
- **Evidence / reproducer:** `mncs-vm/src/migrate.rs` (`translate_research_artifact`); corpus round-trips in `mncs-vm/tests/admission.rs`.
- **VM work blocked or degraded:** canonical execution must pass through the temporary migration adapter; frozen interchange carries compiler internals until this lands.
- **Temporary behavior:** `mncs-vm/src/migrate.rs` translates at the boundary, verifies upstream identity, and refuses anything it cannot translate. No semantics are synthesized.
- **Removal condition:** delete `src/migrate.rs` and the `ResearchPayload` shape once the compiler emits `mncs.vm.artifact/1` and the admission tests pass against direct emission.
