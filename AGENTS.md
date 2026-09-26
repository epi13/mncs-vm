# AGENTS.md

## Purpose

This repository builds the canonical MNCS-native virtual machine and execution runtime.

The VM executes compiler-produced MNCS VM artifacts while preserving the semantics that matter at runtime: artifact identity, call identity, explicit effects, capabilities, resource bounds, structured failure, execution-local memory, scheduling, and evidence-bearing observations.

The repository is documentation-first at initialization. Do not mistake an empty implementation tree for permission to invent a host-centric runtime without first reading the surrounding MNCS contracts.

## Read before changing code

At minimum, inspect:

1. this file;
2. `README.md`;
3. `rfcs/0001-machine-native-virtual-machine.md`;
4. `docs/ARCHITECTURE.md`;
5. `docs/INTEGRATION.md`;
6. `ROADMAP.md`;
7. current `mncs-language` backend/runtime specifications and research-bytecode implementation;
8. current relevant pressures/RFCs in `MNCS-Commons`;
9. current consumers such as Fabric, Actions, Test, Debug, Forge, Doctor, Store, and Memory where the task touches them.

Never rely on this initial scaffold as a substitute for checking current repository heads.

## Core invariants

### One canonical implementation

Build and improve one VM.

Do not create `v1`, `v2`, `legacy`, `next`, compatibility forks, or parallel half-implementations merely because a design is evolving. MNCS currently controls the relevant consumers. When the canonical design changes, migrate the consumers and remove obsolete machinery.

Serialized artifacts may require explicit schema/revision identities. That is not the same thing as maintaining multiple competing VM products.

### The VM does not define source-language meaning

`mncs-language` owns source semantics, types, contracts, effects/capabilities as language constructs, legality, IR, compiler transformations, and lowering decisions.

The VM may:

- validate a VM artifact;
- reject unsupported or malformed artifacts;
- require runtime facts, authority, or resource envelopes;
- execute the admitted artifact according to VM semantics.

The VM must not silently reinterpret language meaning to make execution convenient.

### Native VM is one backend, not the only backend

Do not route every MNCS program through this VM by architectural decree. WASM, native code, accelerators, GPUs, eBPF, RISC-V, PTX, or future targets may remain better realization paths for particular workloads.

The dedicated VM exists because MNCS needs a native execution environment with strong semantic/evidence continuity, not because all other backends are invalid.

### Current research bytecode is migration input

`mncs-language` already contains a research-bytecode backend and bounded SSA interpreter. Treat it as evidence and implementation pressure.

Do not blindly copy it and declare the VM complete. Determine which concepts belong in the canonical VM contract and which are temporary language-repository scaffolding.

The intended end state is not two permanent MNCS bytecode runtimes.

### Deterministic core, explicit nondeterminism

Make reproducible behavior deterministic where semantics allow it.

External I/O, clocks, randomness, remote providers, models, users, devices, or other external state may be nondeterministic. Route those observations through explicit boundaries and retain enough identity/context to explain what influenced execution.

Never make a universal determinism claim merely because the interpreter loop itself is deterministic.

### Capabilities are enforced, not annotated

If an executing operation crosses an effect/authority boundary, runtime authority must be checked at that boundary.

Fail closed when required authority is absent or cannot be established.

Do not overclaim sandboxing. Capability enforcement, process isolation, memory safety, provider isolation, and hostile-code containment are separate claims requiring separate evidence.

### Bounds must survive into execution

When the admitted artifact carries relevant bounds—iterations, recursion, calls, instructions, memory, effects, task counts, or related resource envelopes—the VM must preserve and enforce them where the VM contract says it can.

Do not erase compiler-visible bounds during lowering and reintroduce unrelated host limits later.

### Structured outcomes over process accidents

Expected runtime outcomes must be machine-readable. Distinguish, where relevant:

- successful completion;
- declared failure/status;
- capability refusal;
- budget exhaustion;
- trap;
- unsupported operation;
- malformed artifact;
- suspension/yield;
- provider failure;
- host/runtime failure.

Do not collapse all of these into a generic nonzero process exit.

### Identity must remain visible

Preserve exact identities for the artifact, callable, runtime configuration, capability/provider bindings, resource envelope, and important transitions/observations where the surrounding contracts require them.

Logs are not identity.

## Repository ownership

This repository owns VM execution semantics and runtime-local mechanisms required to implement them.

It does not own:

- source language/compiler semantics (`mncs-language`);
- durable storage (`mncs-store`);
- semantic memory (`mncs-memory`);
- host/device/distributed placement (`mncs-fabric`);
- concrete action/provider business logic (`mncs-actions` and providers);
- wall-clock workflow automation (`mncs-automation`);
- model architecture or learning policy (`mncs-models`, `mncs-learn`);
- verification orchestration, diagnosis, or experimentation policy (`mncs-test`, `mncs-debug`, `mncs-doctor`, `mncs-forge`);
- rights/provenance authority (`mncs-rights-provenance`).

Interfaces to those systems are expected. Ownership transfer is not.

## Implementation discipline

Before adding a primitive, ask:

1. Is this genuinely a VM/runtime semantic concept?
2. Is it already defined upstream by the language/compiler?
3. Is it actually an external provider/host concern?
4. Does a current consumer require it, or are we speculating?
5. Can the same requirement be expressed with a smaller general primitive?
6. What identity, failure, authority, and resource behavior does it require?
7. What evidence would show that the implementation preserves the intended semantics?

Prefer narrow general runtime primitives over architecture-specific special cases.

## Host-language bootstrap

A host language will almost certainly be required to bootstrap the VM. That is acceptable.

The host implementation must remain an implementation of explicit MNCS VM semantics rather than becoming the source of truth by accident.

When a desired runtime contract cannot yet be represented cleanly by `mncs-language` or another MNCS subsystem:

- implement the smallest honest bootstrap needed;
- document the missing capability;
- file/propagate pressure to the owning repository;
- avoid inventing fake `.mncs` syntax or unsupported guarantees;
- migrate toward the native contract when the upstream capability exists.

## Testing and evidence

Tests should distinguish semantic conformance from implementation convenience.

Important classes will likely include:

- artifact validation/admission;
- value/call/control semantics;
- deterministic execution/replay cases;
- malformed-artifact refusal;
- capability allow/deny cases;
- resource-budget exhaustion;
- memory-region and lifetime behavior;
- structured trap/failure behavior;
- scheduler ordering/bounds;
- provider-boundary behavior;
- execution identity and trace integrity;
- differential comparison with the current research-bytecode path during migration;
- cross-backend bounded agreement where useful without treating agreement as universal proof.

Keep fast conformance tests available. Expensive family-wide verification should not become the only way to determine whether a local VM change works.

## Migration rule

When the dedicated VM supersedes functionality currently embedded in another repository:

1. establish the new canonical contract;
2. implement and test it;
3. migrate the known consumers;
4. verify the migrated path;
5. remove the obsolete duplicate path when safe.

Do not leave permanent duplicate runtimes solely to avoid coordinated changes across repositories.

## Completion standard

A change is not complete merely because bytecode executes.

For runtime-semantic changes, verify as applicable that:

- exact artifacts/callables are identity-bound;
- invalid artifacts fail closed;
- authority is checked at the effect boundary;
- bounds are enforced;
- structured outcomes remain distinguishable;
- deterministic behavior is reproducible under equal admitted inputs;
- external nondeterminism is explicit;
- consumers can observe what they need without scraping log text;
- repository ownership boundaries remain intact;
- documentation/RFCs reflect any changed contract.
