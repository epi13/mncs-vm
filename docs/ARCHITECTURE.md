# MNCS VM Architecture

This document defines the initial architectural shape of `mncs-vm`. It is intentionally provisional. Concrete instruction forms, value layouts, memory structures, and scheduling mechanics must be driven by current `mncs-language` artifacts and real execution pressure.

The architecture is constrained by one rule above all others:

> The VM executes MNCS-native artifacts without becoming a second source-language semantics engine.

## Execution boundary

The expected high-level boundary is:

```text
compiler-owned
-------------
MNCS source
semantic model
HIR / SSA / selected SSA
legality + lowering plan
translation/evidence relation
        |
        v
runtime-owned
-------------
MNCS VM artifact
loader / verifier
runtime admission
execution engine
structured completion + observations
```

The VM accepts a frozen artifact. It must not require compiler-internal mutable state in order to execute it.

## Provisional major components

### 1. Artifact format

The VM artifact is the complete executable unit consumed by the runtime.

It will likely need to represent:

- format/revision identity;
- content identity;
- module/callable identities;
- value/type ABI required at runtime;
- code/instruction payload;
- constants;
- control-flow metadata required by execution;
- runtime-relevant resource bounds;
- runtime-relevant capability/effect requirements;
- compatibility/runtime requirements;
- references to compiler/translation provenance and evidence where required.

The artifact should not embed arbitrary compiler implementation state merely because that state is convenient to serialize.

### 2. Loader and verifier

The loader turns encoded artifact bytes/data into an admitted executable runtime object.

Responsibilities may include:

- format decoding;
- content identity verification;
- structural integrity checks;
- instruction/control target validation;
- callable table validation;
- runtime ABI/value validation;
- bound/resource-envelope validation;
- required capability declaration validation;
- compatibility checks against the current VM/runtime feature envelope;
- rejection of malformed, internally inconsistent, or unsupported artifacts.

Runtime verification is not a replacement for compiler proofs or translation validation. It verifies what the runtime must know before safely executing the artifact.

### 3. Runtime admission

Loading an artifact is not necessarily enough to execute it.

An invocation may need to bind:

```text
ExecutionAdmission
  artifact_identity
  callable_identity
  input_values
  capability_environment
  provider_bindings
  resource_envelope
  runtime_configuration
  host/runtime facts
  observation policy
```

Admission should produce either an exact executable session/invocation contract or a structured refusal.

### 4. Value model

The VM needs a stable runtime representation for values crossing calls, operations, and provider boundaries.

The value model should be derived from the language ABI and supported artifact contract rather than invented independently.

Likely pressure includes:

- booleans and bounded integers;
- enums/status values;
- records/tuples;
- bounded sequences/views;
- vectors/masks;
- references/handles or region-relative values;
- opaque provider/runtime handles where explicitly allowed.

Runtime representation may be optimized internally so long as observable semantics and identity/evidence requirements remain preserved.

### 5. Execution engine

The reference engine should initially favor semantic clarity over speed.

Likely concepts:

```text
Runtime
  ArtifactInstance
  Invocation
  Frame
  Value
  ProgramCounter / BlockPosition
  RuntimeMemory
  ResourceMeter
  CapabilityContext
  ProviderContext
  Scheduler
  ObservationSink
```

The instruction set should remain small and general. Do not create one instruction per high-level source construct when lower-level reusable primitives preserve the required semantics.

### 6. Call model

Call dispatch should preserve stable callable identity.

The VM should support invocation by artifact-bound identity rather than requiring callers to reconstruct module/function strings as the authoritative execution key.

Where human-friendly names exist, they are metadata or lookup aids; the runtime identity is the canonical binding.

The call model must define:

- argument validation;
- frame creation;
- return value handling;
- declared/structured failures;
- call-depth/count accounting;
- capability propagation or narrowing;
- provider-call transitions;
- cross-artifact calls if/when supported.

### 7. Control flow

Control flow should represent compiler-lowered meaning directly enough to preserve bounded iteration and structured failure semantics.

The VM must not infer a language loop merely because control flow is cyclic. If the compiler artifact carries explicit bounded-iteration metadata or obligations, the runtime contract should preserve the parts needed for execution and accounting.

### 8. Runtime memory

Runtime memory is transient execution machinery.

Potential layers include:

- frame-local values;
- immutable constants;
- regions/arenas;
- bounded aggregate storage;
- heap-like allocation where required;
- handles/references;
- provider-owned opaque handles;
- snapshots/checkpoints used for debugging or replay.

The final design must define:

- allocation behavior;
- lifetime/reclamation;
- aliasing rules that require runtime enforcement;
- bounds/failure behavior;
- reference validity;
- snapshot semantics.

This is distinct from semantic memory (`mncs-memory`) and durable persistence (`mncs-store`).

### 9. Capability and effect boundary

An effectful VM operation must identify the authority required to perform it.

A typical transition is:

```text
instruction requests effect
        |
        v
resolve required capability
        |
        v
check admitted capability environment
        |
   +----+----+
   |         |
 deny      authorize
   |         |
refusal      v
        provider binding
             |
             v
        external operation
             |
             v
      structured observation
```

Provider calls must not silently inherit unrestricted host authority from the VM process.

The first implementation may use host-language interfaces, but those interfaces should model explicit capability/provider contracts.

### 10. Resource metering

Resource accounting should be part of execution state, not an afterthought.

Possible counters/envelopes:

- instruction/step budget;
- call count/depth;
- iteration budget;
- allocation/memory limits;
- task count;
- effect count;
- provider-specific quotas.

The VM must distinguish exact deterministic counters from approximate host measurements. For example, VM steps are not CPU cycles unless evidence establishes that relation.

### 11. Scheduler

Scheduling belongs in the VM only to the extent needed to execute an admitted program.

Potential runtime concepts:

- ready tasks;
- deterministic queue ordering;
- bounded spawn;
- yield/suspend;
- join/wait;
- cancellation;
- provider wait/resume.

The scheduler does not own:

- cron-like automation;
- user workflow scheduling;
- distributed node placement;
- cluster resource assignment.

Those belong to other MNCS layers.

### 12. Structured outcomes

Execution should return a typed result envelope, not only a host process status.

A provisional shape is:

```text
ExecutionResult
  execution_identity
  artifact_identity
  callable_identity
  outcome
    Completed(value)
    DeclaredFailure(value/status)
    CapabilityDenied(...)
    BudgetExhausted(...)
    Trap(...)
    Unsupported(...)
    ProviderFailure(...)
    Suspended(...)
    RuntimeFailure(...)
  resource_observations
  effect_observations
  transition/trace identity
```

The exact categories must follow actual language/runtime contracts.

### 13. Observation and evidence

The VM should expose machine-readable observations suitable for Test, Debug, Forge, Doctor, Fabric, and evidence workflows.

Useful observations may include:

- exact runtime/artifact/call identities;
- deterministic transition digests;
- bounded trace records;
- resource counters;
- capability checks;
- provider transitions;
- trap/failure records;
- nondeterministic inputs/observations;
- final value digest or materialized value according to policy.

Observation is not automatically proof. The consuming system must preserve the scope of any claim made from runtime evidence.

## Determinism model

The VM should separate three categories.

### Deterministic core

Given the same:

- admitted artifact;
- invocation values;
- runtime configuration;
- resource envelope;
- deterministic provider responses/recorded observations;

pure execution should produce the same VM-level transitions and result according to the VM specification.

### Explicit external nondeterminism

Sources such as clocks, randomness, network responses, models, devices, or users must cross explicit interfaces.

The VM should record enough information to state whether:

- the observation was deterministic;
- it was captured and can be replayed;
- it was identified but cannot be faithfully replayed;
- its provenance/identity is only partially known.

### Host nondeterminism that must not leak into semantics

Things like hash-map iteration order, host pointer values, thread scheduling, allocator address choices, and incidental OS timing must not change defined VM semantics unless the VM specification explicitly exposes them.

## Security posture

Initial VM development should be precise about what has actually been secured.

Potential claims are separate:

- artifact structural validation;
- capability enforcement;
- bounds enforcement;
- memory safety of the VM implementation;
- isolation between VM invocations;
- isolation from the host process/OS;
- provider isolation;
- malicious bytecode containment.

Do not combine them into a generic "sandboxed" claim without supporting evidence.

## Reference engine and optimized engines

The reference engine should remain the semantic oracle for executable VM behavior to the extent practical.

Later engines may include:

- faster interpreters;
- cached/decoded execution;
- JIT;
- AOT translation;
- vectorized runtime primitives;
- host/accelerator specialization.

They must conform to the same observable VM semantics. Where an optimization cannot preserve a required property, it should refuse or surface the limitation rather than silently diverge.

## Migration from current language runtime

The current research-bytecode implementation already contains valuable execution semantics. Migration should identify and classify each piece:

- compiler-only experiment machinery;
- VM artifact semantics;
- runtime execution semantics;
- generic call/session API;
- corpus/test harness behavior;
- evidence/identity behavior.

Move only what belongs here. Leave language semantics upstream and general orchestration in the appropriate repositories.

## Open architectural questions

The first implementation campaign should resolve, through real code pressure:

- whether the VM artifact remains close to selected SSA or introduces a distinct compact instruction representation;
- which type information must survive into runtime artifacts;
- the canonical runtime memory model;
- how capabilities/providers are bound to an invocation;
- how execution-local concurrency is represented;
- how exact resource accounting is defined;
- what trace granularity is useful without making execution prohibitively expensive;
- how debug/replay snapshots interact with effects;
- how cross-artifact calls and dynamic loading should work, if they are needed at all;
- which existing `Session` APIs should move here versus remain as higher-level wrappers.

These should be answered by maintaining a small explicit contract and applying pressure from real MNCS workloads.
