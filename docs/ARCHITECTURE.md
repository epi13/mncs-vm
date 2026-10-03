# MNCS VM Architecture

This document defines the architectural shape of `mncs-vm`. The
provisional sections below record design intent; the
"Implemented state" section records what campaign 1 actually built,
driven by current `mncs-language` artifacts and real execution
pressure. Where the two disagree, the implemented state wins and
the intent section is updated.

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

The VM exposes machine-readable observations suitable for Test, Debug, Forge, Doctor, Fabric, and evidence workflows.

Live observations reuse the shared language-owned stream shape
(`mncs.execution-observation/1`): event kinds, value captures,
frames, effects, policy bounds, and completeness mean exactly what
they mean for completed reference-runtime runs. The VM emits
`execution_enter/exit`, `frame_enter/exit`, `block_enter`,
`operation_enter/result` (paired exactly, including calls),
`return`, `failure`, and `effect_invoke/result`. Values are captured
only for retained operation events, so selected capture retains its
own value closure rather than the ambient value universe.

Useful observations include:

- exact runtime/artifact/call/execution identities
  (`mncs:vm:execution:<digest>` is content-derived over artifact,
  callable, arguments, capabilities, and envelope);
- deterministic transition digests (per stop: execution, steps,
  effects, safe point, state);
- bounded trace records under the shared policy vocabulary;
- resource counters;
- capability checks;
- provider transitions;
- trap/failure records;
- nondeterministic inputs/observations (recorded per effect with a
  replayability class);
- final value digest or materialized value according to policy.

Observation is not automatically proof. The consuming system must preserve the scope of any claim made from runtime evidence.

### 13a. Live debugging (safe points, stops, resume)

`src/debug.rs` owns the live-debug contract (`mncs.vm.debug/1`).
Run state is plain data and the drive loop is re-entrant, so the VM
suspends truthfully: a stop holds the same frames, values, usage,
and effect log that resume continues from. Nothing re-executes to
fake a stop.

Safe points (`operation`, `effect_before`, `effect_after`,
`terminal`) guarantee: the frame value map is consistent, the stack
above is untouched, usage is charged exactly for completed
transitions, and the pending transition has not executed. Stopping
before an effect never repeats or skips it; suspending after an
effect advances past the instruction first, so resume cannot
dispatch twice.

Stop conditions name authoritative identities only (SSA
instruction, function/semantic identity, effect boundary,
failure/trap class) — never filename/line heuristics. Each stop
carries a single-owner continuation token bound to the execution
and stop sequence; stale and foreign tokens fail closed. Terminal
stops are inspectable but not resumable; terminate finalizes them.
Every stop record also carries `pending_terminal`: `None` on
ordinary stops, the abnormal outcome (failure/trap/denial detail)
on terminal stops, so a consumer never has to guess which failure
suspended the run.

Stepping is defined over observable semantic transitions: `step`
stops at the next operation (into calls), `step over` at the next
operation at or above the entry depth, `step out` below it.
Resuming from a stop never re-fires stops at the same arrival.

Value instances carry runtime-local identities
(`mncs:vm:value:<digest>` over execution, frame, binding, version),
stable while unchanged and deterministic across replay of
identical admitted inputs. Semantic origin travels alongside via
the SSA producer map; the two are never conflated.

Explicitly unsupported: live watch stops, branch-condition stops,
async effect suspension (dispatch is synchronous), expression
evaluation, time travel, and multi-execution control. See
`unsupported_debug_capabilities()`.

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

## Implemented state (campaign 1)

The first executable foundation exists as one Rust crate with no
parallel implementations. Module map:

```text
src/artifact.rs    canonical mncs.vm.artifact/1 contract (externally
                   tagged code section; content identity by sha256)
src/admit.rs       loader/admission with typed refusals (Malformed,
                   UnsupportedFeature, UnresolvedFact,
                   IncompatibleContract, IdentityMismatch)
src/migrate.rs     TEMPORARY adapter: research-bytecode payload in,
                   canonical artifact out (removal: P-VM-COMPILER-001)
src/value.rs       VM-owned values (int/bool/byte/float-bits/finite/
                   record/sequence/vector/mask) with wire marshalling
src/engine.rs      reference interpreter: explicit frame stack,
                   identity-bound calls, SSA executable subset,
                   region iteration bounds, capability dispatch,
                   per-instruction step accounting
src/resource.rs    envelopes (steps, call_depth, memory_cells,
                   effects, iterations) with fail-closed charging
src/capability.rs  admitted authority + provider boundary
                   (ConstProvider/FailingProvider fixtures)
src/outcome.rs     Completed, ProgramFailure, Trap, CapabilityDenied,
                   Unsupported, InvalidRequest, BudgetExhausted,
                   ProviderFailure, HostFailure
src/evidence.rs    ExecutionRecord with identities, usage, effect
                   observations, return/effects digests
src/session.rs     Session::open/call binding artifact, callable,
                   args, capabilities, envelope explicitly
src/harness.rs     dev/test compile helper driving the read-only
                   upstream pipeline in-process
```

Resolved from the open questions above:

- The artifact stays close to selected SSA (code-only section on
  the roadmap; the payload program is parsed for routing and then
  dropped — P-VM-LANG-002).
- Type information survives as the SSA value/operand types; the
  engine checks arity plus coarse kind/identity at call and branch
  boundaries.
- Memory is frames plus reference-counted immutable values with
  cell accounting; no collector, no linear memory, no handles yet.
- Capabilities bind per call through an explicit environment;
  providers are foreign traits, never VM logic.
- No execution-local concurrency exists: current programs need
  none, so no scheduler was built (deferred by evidence, not by
  omission).
- Accounting is exactly one step per executed instruction plus
  one per taken terminator edge; iteration bounds come from SSA
  region metadata.
- Trace granularity is the shared observation stream (operation,
  block, frame, effect, failure events plus bounded values) under
  an explicit capture policy; unobserved runs emit nothing.
- Live debugging exists: safe points, identity-bound stops, typed
  inspection, resume/step/terminate, transition digests, and
  deterministic value identities (`src/debug.rs`, `mncs.vm.debug/1`).
  Replay is forward re-execution with recorded observations;
  reverse execution does not exist.
- Cross-artifact calls and dynamic loading do not exist.
- The upstream `Session` API stays upstream; the VM exposes its
  own smaller session over admitted artifacts.
- Integer meaning is never redefined: the engine evaluates
  through public `IntegerOperation::evaluate` and mirrors oracle
  outcome mapping, pinned by differential tests.

Executable subset and explicit UNSUPPORTED list: Constant,
Integer, IntegerCompare, BooleanOp, BooleanCompare, BooleanNot,
ByteBitwise, ByteShift, ByteCompare, scalar Select,
RecordConstruct, RecordProject, FiniteConstruct,
FinitePayloadProject, FiniteIsVariant, SequenceConstruct,
SequenceProject, SequenceLength, SequenceReplace, BoundCheck,
integer/boolean/byte Convert, Call, Effect (recorded),
HostCall (provider-dispatched), Return, Branch,
ConditionalBranch, Failure. Everything else — floats ops,
vectors, masks ops, views, SequenceCopy, RuntimeCheck (also
Unsupported upstream) — is explicit `Unsupported`, never
approximated.
