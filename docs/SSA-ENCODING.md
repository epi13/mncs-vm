# Frozen SSA shared-node encoding

The canonical `mncs.vm.artifact/1` code section keeps decoded SSA schema
`0.5`. `code.mncs-selected-ssa.module` now contains a standalone JSON DAG:
`{"nodes": [...], "root": N}`. Regenerate artifacts emitted before this
encoding change; the loader requires this representation. There is one
encoder/decoder in compiler-owned `tools/vm-artifact-codec`, consumed by
both the pinned compiler probe and VM. No external dictionary, compression
service, or compiler session is needed to load the sealed artifact.

Each node is `"N"` (null), `{"B": bool}`, `{"I": number}`, `{"S": string}`,
`{"A": [node indices]}`, or `{"O": [[key index, value index], ...]}`.
Object keys reference string nodes and are strictly sorted and unique.
References point backward; the root is the final node; every node must be
reachable. Strings and identical arrays/objects/types/evidence share nodes.
The decoder checks expanded-value count (8,000,000 maximum) and nesting
(128 maximum) before typed decoding. These artifact-decoder limits are
independent of VM execution fuel and make cyclic or exponential expansion
an explicit malformed-artifact refusal.

Canonical node order is deterministic postorder following sorted object
keys and array order. Hash tables only find existing nodes; collisions are
checked for equality and iteration order never determines output. Typed
SSA serialization walks borrowed fields directly; decoding walks shared
nodes directly into typed SSA. The compiler freezes this graph once before
sealing and transport. VM identity validation serializes borrowed artifact
fields rather than cloning the entire SSA graph.

Every original SSA fact remains present: executable instructions, callable
and specialization identities, type declarations, semantic bindings,
source occurrence coordinates, obligations, trace maps, transformation
records, machine intent, effects/capabilities, and bounded-iteration facts.
Admission still validates the decoded code and sealed content identity.
This changes deterministic artifact content identities because the encoding
changed; it does not change callable identities or SSA facts. The debug
identity golden scenario deliberately freezes its historical artifact ID,
so its derivation algorithm remains independently checked.

`mncs-compiler/tools/account_vm_artifact.py BEFORE AFTER` gives physical
section accounting and asserts exact equality of all expanded SSA and
outer contract facts. VM `examples/measure.rs` separates read, decode,
admission/index construction, session construction, and two execution runs,
with allocator/reallocator counts and cumulative requested bytes. Requested
allocation bytes are churn, not live memory; `/usr/bin/time -v` measures RSS.
The example consumes the same `[{"id": ..., "request": ExecutionRequest}]`
batch contract as the CLI and drops each completed result after digesting it.
Its RSS therefore differs from the CLI, which retains batch results. The CLI
uses `admit_owned` to release the encoded transport buffer immediately after
decode; borrowed and owned admissions share the same decoder and validator.

Admission builds immutable function/block routing and nested-iteration
reset indexes once. Each session call borrows them and creates fresh frames,
iteration counters, usage, and effect state. The engine also borrows code
block identities and avoids allocating an existing iteration counter's key.
Instruction, terminator, loop-activation, and live-set accounting are unchanged.
