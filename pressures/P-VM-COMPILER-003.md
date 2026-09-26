# P-VM-COMPILER-003 — no CLI surface emits frozen artifact bytes

- **ID:** P-VM-COMPILER-003
- **Owner repository:** `epi13/mncs-compiler` (CLI surface lives in `epi13/mncs-language` today; either owner can land the flag)
- **Observed from:** `mncs test --artifacts DIR` and `mncs call` (`crates/mncs-cli/src/main.rs`)
- **Current upstream behavior:** `mncs test`/`mncs call` compile and execute in-process. `--artifacts DIR` stores request JSON and results, not the backend artifact bytes. No subcommand emits the frozen executable artifact (`BackendArtifact` JSON with payload) to stdout or a file.
- **Required runtime behavior:** a stable command (e.g. `mncs emit-artifact`) that writes the frozen backend artifact bytes for a source file plus library roots, so out-of-process runtimes, stores, and evidence pipelines can consume artifacts without linking the compiler.
- **Why current contract is insufficient:** `mncs-vm` can only reach current artifacts by linking compiler crates in-process. That is correct for now but couples every downstream runtime to the compiler's process image and makes Store/Fabric handoff of frozen bytes impossible through public interfaces.
- **Smallest general missing capability:** one serialization-stable emission command reusing the existing `BackendArtifact` JSON shape.
- **Required upstream change:** add the emission command; document byte-stability expectations (same source+flags produce comparable bytes or explicitly do not).
- **Evidence / reproducer:** `main.rs` `request_artifact` handling (~line 2340) stores `request.to_json()` only; `mncs-vm/src/harness.rs` links crates because no bytes cross the CLI boundary.
- **VM work blocked or degraded:** out-of-process VM drivers; Store persistence of VM artifacts; Fabric shipment of frozen bytes.
- **Temporary behavior:** `mncs-vm` consumes artifacts in-process via public crate APIs (use, not mutation). No CLI scraping, no cache scraping.
- **Removal condition:** add a subprocess-based artifact supplier to `harness.rs` once the command exists; keep the in-process path for the fast loop.
