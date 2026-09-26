# P-VM-LANG-001 — requests are name-bound, not identity-bound

- **ID:** P-VM-LANG-001
- **Owner repository:** `epi13/mncs-language`
- **Observed from:** `ExecutionRequest { target: ExecutionTarget { module, function } }` and `SsaExecutionSession` target resolution (`crates/mncs-model/src/ssa_execution.rs`, oracle lines ~1060-1100, ~1440-1460)
- **Current upstream behavior:** callers name `(module, function)` strings; the runtime re-derives the namespace (`identity_namespace`) from the source `Program` and computes `function_id` to find the SSA function.
- **Required runtime behavior:** requests carry the stable callable/function identity as the primary target (strings remain as routing hints). The runtime binds execution to the identity without program-namespace inference.
- **Why current contract is insufficient:** the VM's identity-first rule cannot be honored end to end: the migration adapter must keep the payload `Program` JSON around solely to re-derive namespaces the compiler already knew, and any program-shape change risks silent re-routing. (`ExecutionRequest` already anticipates this with its `callable_identity` direction; the SSA session path does not accept it yet.)
- **Smallest general missing capability:** accept `callable_identity: SemanticId` as the primary execution target in the SSA session/oracle path, falling back to module/name routing only for compatibility.
- **Required upstream change:** thread an optional identity target through `ExecutionRequest` into `SsaExecutionSession`; resolve by `semantic_identity` first; record which binding was used in execution evidence.
- **Evidence / reproducer:** `mncs-vm/src/migrate.rs::resolve_export` (namespace re-derivation the VM should not need); `mncs-vm/src/engine.rs` dual function index (identity + semantic) bridging the two namings.
- **VM work blocked or degraded:** end-to-end identity-bound invocation; the VM binds identities internally after adapter resolution, so execution itself is identity-bound but admission is not.
- **Temporary behavior:** adapter resolves via the public `function_id` derivation against program routing data; ambiguous or missing routes are `UnresolvedFact` refusals.
- **Removal condition:** upstream accepts identity targets; the adapter drops program-JSON routing and resolves exports against SSA identities only.
