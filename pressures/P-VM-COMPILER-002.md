# P-VM-COMPILER-002 — frozen interchange integer fidelity

- **ID:** P-VM-COMPILER-002
- **Owner repository:** `epi13/mncs-compiler` (schema), `epi13/mncs-vm` (adapter behavior, see P-VM-ARTIFACT-007)
- **Observed from:** `BackendArtifact::bytes_hex` / `EmbedArtifact::from_json` round-trips; `mncs-vm/tests/admission.rs::valid_artifact_round_trips`
- **Current upstream behavior:** executable payloads serialize 128-bit integers (SSA `Constant` values, operation bounds) as JSON numbers. Stock JSON parsing cannot target 128-bit integers, so any out-of-64-bit-range integer silently fails to decode (or refuses, depending on the parser configuration). The research payload has no bignum-exact encoding.
- **Required runtime behavior:** frozen artifacts must round-trip every integer the compiler can emit, exactly. Either the artifact schema encodes wide integers losslessly (decimal strings, `{hi,lo}` pairs — owned by the schema), or the compiler guarantees emitted integers fit explicit bounds the runtime can check at admission.
- **Why current contract is insufficient:** the VM's admission guarantee ("frozen bytes re-admit identically") holds only while emitted integers stay in 64-bit range. A corpus that legitimately uses 128-bit constants would admit in-process but fail from frozen bytes — an interchange hazard, not a VM defect.
- **Smallest general missing capability:** a schema-level integer encoding rule for executable sections, shared by emitter and all consumers.
- **Required upstream change:** pick and document the wide-integer encoding in the artifact contract; add a round-trip vector with boundary integers (u64::MAX, i64::MIN, u128-adjacent values the compiler admits).
- **Evidence / reproducer:** `mncs-vm/src/admit.rs` two-step parse with the `Malformed` refusal path; the `i128` probe in this campaign (`serde_json::from_str::<i128>("8")` succeeds, full-payload decode is range-limited).
- **VM work blocked or degraded:** frozen transport of wide-integer programs; in-process execution is unaffected.
- **Temporary behavior:** admission refuses undecodable payloads as `Malformed` (never truncates); the limitation is documented in `src/admit.rs` and `docs/ARCHITECTURE.md`.
- **Removal condition:** upstream schema owns the encoding; the VM adopts it and extends the admission round-trip tests to boundary integers.
