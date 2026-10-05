# P-VM-LANG-002 — executable section embeds compiler internals

- **ID:** P-VM-LANG-002
- **Owner repository:** `epi13/mncs-language` (SSA schema owner)
- **Observed from:** `ResearchPayload { program, ssa }` (`crates/mncs-codegen/src/lib.rs`, payload schema `0.1`)
- **Current upstream behavior:** the executable artifact carries the full source `Program` next to the `SsaModule`. The program is lowering provenance, but nothing distinguishes "provenance the runtime may ignore" from "meaning the runtime must load".
- **Required runtime behavior:** a code-only executable section: the selected SSA plus its identity/fingerprint, with program provenance referenced by digest rather than embedded by value.
- **Why current contract is insufficient:** the VM must parse (and trust the shape of) the entire program only to route a handful of exports. Every program-schema change threatens runtime loading even when SSA semantics did not move. It also doubles frozen artifact size with non-executable content.
- **Smallest general missing capability:** a declared code-only projection of the research payload (same SSA, program replaced by its identity + fingerprint, which the payload already computes).
- **Required upstream change:** emit (or document as stable) the code-only projection; keep full-payload emission for debugging.
- **Evidence / reproducer:** `mncs-vm/src/migrate.rs::ResearchPayload` (program field is parsed and then deliberately dropped — only routing data is read); corpus artifact sizes in `target/` evidence runs.
- **VM work blocked or degraded:** none functionally; the adapter absorbs it. Cost is complexity and payload size, not correctness.
- **Temporary behavior:** adapter reads export routing from program JSON and drops everything else; the canonical artifact never stores the program.
- **Removal condition:** consume the code-only projection; delete program-JSON handling from the adapter.


2026-10-05 scope update: historical `src/migrate.rs` reproductions refer to
commit `a3ab04c`. That module is now deleted with zero consumers. Direct
compiler-owned callable emission and lossless frozen SSA encoding remove
this friction from the dedicated VM path; remaining upstream research
backend behavior is not changed or claimed resolved by this cleanup.
