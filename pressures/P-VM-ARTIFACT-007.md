# P-VM-ARTIFACT-007 — VM-owned bignum-exact frozen encoding

- **ID:** P-VM-ARTIFACT-007
- **Owner repository:** `epi13/mncs-vm` (sibling of P-VM-COMPILER-002, which tracks the upstream schema half)
- **Observed from:** `mncs-vm/src/admit.rs` two-step parse; `mncs-vm/tests/admission.rs::valid_artifact_round_trips`
- **Current behavior:** admission parses text to `Value` then decodes typed, so in-range integers widen exactly and out-of-range integers refuse as `Malformed`. Internally-tagged enums are avoided in the envelope because the buffered-content deserializer cannot target 128-bit integers in this toolchain (`CodeSection` is externally tagged for exactly this reason).
- **Required behavior:** a canonical frozen encoding that round-trips every integer the compiler can emit, owned by the artifact schema (this file tracks the VM half; the schema rule lands upstream per P-VM-COMPILER-002).
- **Why this is a VM pressure, not just upstream:** even with a perfect upstream schema, the VM envelope must state its decoding guarantees and refuse (never truncate) outside them. That rule lives here.
- **Smallest general missing capability:** schema-level wide-integer encoding plus VM admission vectors at the boundaries (u64::MAX, i64::MIN, wider).
- **Required change (here):** adopt the upstream encoding once declared; extend `valid_artifact_round_trips` with boundary-integer corpus programs.
- **Evidence / reproducer:** the `i128` probe sequence in this campaign (small values decode; full-payload decode is range-limited by the toolchain deserializer, not by the VM).
- **VM work blocked or degraded:** frozen transport of wide-integer programs only.
- **Temporary behavior:** fail-closed `Malformed` refusal with a pointer to this record.
- **Removal condition:** encoding declared upstream and covered by boundary round-trip tests.
