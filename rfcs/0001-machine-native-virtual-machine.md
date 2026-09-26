# RFC 0001: Machine-Native Virtual Machine

- **Status:** Initial direction
- **Repository:** `mncs-vm`

## Summary

MNCS should have a dedicated native virtual machine that executes compiler-produced MNCS VM artifacts while preserving runtime-relevant semantics such as callable identity, capabilities, effects, resource bounds, structured failure, runtime-local memory, scheduling, and evidence-bearing observations.

The VM is a target/runtime layer, not a second source-language implementation and not a replacement for all other compiler backends.

## Motivation

`mncs-language` already demonstrates bounded execution through an experimental research-bytecode backend and interpreter. That path is useful, but keeping the canonical MNCS-native runtime embedded permanently in the language repository would blur several ownership boundaries:

- compiler semantics versus execution semantics;
- backend lowering versus runtime admission;
- artifact generation versus artifact execution;
- compiler experiments versus reusable runtime APIs;
- source-level verification versus runtime evidence.

A dedicated repository lets MNCS evolve a native execution environment whose semantics can be consumed independently by Fabric, Test, Debug, Forge, Actions/providers, and other systems.

## Decision

Create `mncs-vm` as the canonical owner of MNCS-native virtual-machine execution semantics.

The repository will evolve one canonical implementation.

The VM will eventually define and implement the runtime-specific side of:

- the executable VM artifact/container contract;
- artifact loading and validation;
- runtime admission;
- value/call/control execution;
- runtime-local memory;
- capability/effect mediation;
- bounded resource accounting;
- execution-local scheduling where required;
- structured completion, trap, refusal, and failure semantics;
- runtime observations, trace identities, and replay/debug hooks.

## Non-decision: source semantics

This RFC does not move source-language semantics into the VM.

`mncs-language` remains authoritative for:

- source meaning;
- type semantics;
- contracts;
- effects/capabilities as language declarations;
- compiler legality;
- HIR/SSA and compiler transformations;
- target-lowering decisions;
- translation evidence.

The VM receives an already-lowered artifact and executes it according to the VM contract.

## Non-decision: exclusive backend

The native VM is not the only valid execution path.

MNCS may continue to target WASM, LLVM/native code, C, Cranelift, eBPF, RISC-V, PTX, GPUs, accelerators, or other environments.

The VM exists because a native MNCS runtime provides benefits that other targets do not necessarily expose directly: precise runtime identity, portable execution semantics, capability mediation, deterministic metering, structured introspection, and tight integration with verification/debug tooling.

## Research-bytecode migration

The current `mncs-language` research-bytecode backend is the immediate empirical starting point.

It should be surveyed for:

- artifact structure;
- bounded SSA execution;
- value ABI;
- callable identity;
- session/call behavior;
- effect/capability behavior;
- bounded iteration;
- execution corpus behavior;
- evidence/observation structure.

Those concepts must then be classified into:

1. compiler-owned semantics/infrastructure;
2. canonical VM artifact semantics;
3. canonical runtime execution semantics;
4. generic cross-repository integration contracts;
5. experimental scaffolding that should be discarded.

The target state is one canonical MNCS-native VM runtime. Once migration is complete, obsolete duplicate interpreter/runtime code should be removed or reduced.

## Artifact contract

The VM artifact must be executable without compiler-internal mutable state.

At minimum, the artifact will likely bind:

- format/revision identity;
- artifact content identity;
- module/callable identities;
- runtime ABI/value information;
- executable code/control representation;
- constants;
- runtime-relevant capability/effect declarations;
- runtime-relevant resource bounds;
- compatibility requirements;
- references necessary to connect runtime observations to compiler translation/provenance evidence.

Exact encoding and instruction forms are deferred until the runtime survey.

## Runtime admission

An artifact that decodes successfully is not automatically authorized to execute.

Invocation should be admitted against an explicit runtime environment containing, where relevant:

- callable identity;
- input values;
- capability set;
- provider bindings;
- resource envelope;
- runtime configuration;
- host/target facts;
- observation policy.

Admission failure must be structured and distinguish malformed artifacts, unsupported features, missing authority, incompatible runtime facts, and invalid inputs as appropriate.

## Determinism

The VM should provide a deterministic execution core where semantics allow it.

External nondeterminism—network data, clocks, random sources, devices, users, remote models/providers—must cross explicit interfaces and remain identifiable in runtime observations.

The VM must prevent incidental host behavior such as allocator addresses, hash iteration order, or OS thread timing from changing defined semantics unless such behavior is explicitly part of the VM contract.

## Capabilities and effects

Capability declarations are runtime-relevant authority.

When an instruction/call requests an effectful external transition, the VM must verify that the admitted execution possesses the required authority before dispatching to a provider.

Provider logic remains outside the VM.

Capability enforcement alone is not a claim that the VM is a complete hostile-code sandbox.

## Resource bounds

The VM should enforce runtime bounds carried by the admitted artifact/environment where the VM contract defines them.

Potential resources include:

- steps/instructions;
- calls/call depth;
- iterations;
- memory/allocation;
- tasks;
- effects/provider calls.

Each metric must state what it actually measures. VM steps must not be represented as CPU time or cost without evidence establishing such a relation.

## Runtime memory

The VM owns execution-local memory semantics required to execute artifacts.

This may include frames, stacks, regions, aggregates, heaps/arenas, references/handles, and snapshots.

It does not own semantic memory (`mncs-memory`) or durable persistence (`mncs-store`).

## Scheduling

The VM may own execution-local scheduling needed by program semantics, such as cooperative tasks, yields, waits, joins, cancellation, or provider suspension.

It does not own wall-clock workflow scheduling (`mncs-automation`) or distributed placement (`mncs-fabric`).

## Structured execution result

Execution must produce machine-readable outcomes.

The final taxonomy will be implementation-driven, but it must preserve meaningful distinctions among successful completion, declared failure, capability refusal, budget exhaustion, traps, unsupported features, provider failures, suspension, malformed artifacts, and runtime/host failures where those distinctions exist.

## Observability and evidence

The VM should expose structured observations that can be consumed by other MNCS repositories without log scraping.

Observations may include:

- execution/artifact/call identities;
- resource counters;
- capability decisions;
- provider transitions;
- transition/trace digests;
- traps/failures;
- nondeterministic inputs/observations;
- final value/result identity.

Runtime observation is evidence about an execution, not universal proof of semantics.

## Reference implementation

The first engine should prioritize correctness, inspectability, and conformance.

Optimized interpreters, JIT, AOT, vectorized execution, and accelerator paths may be added later behind the same VM semantics.

A faster implementation that cannot preserve a required semantic property must refuse or expose the limitation rather than silently diverge.

## Evolution policy

There is currently no external compatibility burden that justifies frozen parallel VM implementations.

The project will maintain one evolving canonical implementation and migrate known MNCS consumers forward as the contract changes.

Artifact schemas may carry explicit revisions for decoding and evidence. This does not imply permanent `v1`/`v2` runtime trees.

## Initial implementation sequence

1. Survey current runtime/execution code and contracts.
2. Define the smallest standalone VM artifact.
3. Implement loader/validation.
4. Implement a reference execution core.
5. Bind exact callable identity and structured outcomes.
6. Add runtime memory under real workload pressure.
7. Add capability/provider mediation.
8. Add resource enforcement.
9. Add execution-local scheduling if required.
10. Expose structured test/debug/evidence interfaces.
11. Integrate with Fabric and other consumers.
12. Migrate from the language-owned research runtime and delete obsolete duplication.
13. Optimize only after conformance is stable.

## Success criteria

This RFC is successfully realized when:

- `mncs-language` can emit an identity-bound native VM artifact through its backend boundary;
- `mncs-vm` can execute that artifact independently of compiler internals;
- runtime authority and resource envelopes are explicit and enforceable;
- deterministic runtime behavior is reproducible and external nondeterminism is explicit;
- structured failures and observations are available to Test/Debug/Fabric/Forge;
- other backends remain first-class realization paths;
- the old embedded research-runtime path is no longer required as a permanent duplicate implementation.
