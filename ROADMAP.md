# mncs-vm Roadmap

<!-- MNCS:generated:begin -->
## Evidence-bound roadmap

- **complete** — Declared ambient projection surfaces satisfy their contract (`mncs-vm:projection-conformance`)
<!-- MNCS:generated:end -->

This roadmap describes the path from an empty repository to the canonical MNCS-native virtual machine. It is intentionally capability-oriented rather than version-oriented.

The project should evolve one implementation forward. Milestones are checkpoints in that implementation, not frozen product generations.

## Phase 0 — Runtime survey and contract extraction

Before implementation, establish the actual current boundary.

Tasks:

- inventory the research-bytecode backend and bounded SSA interpreter in `mncs-language`;
- inspect current compiler `BackendArtifact`, execution corpus, callable identity, `Session`/call APIs, effects/capabilities, bounded iteration, and execution evidence;
- inspect relevant `MNCS-Commons` pressures and cross-repository contracts;
- inspect Fabric packaging/execution expectations;
- inspect Actions/provider admission and capability contracts;
- inspect Test, Debug, Doctor, Forge, Store, Memory, and other consumers that currently depend on runtime behavior;
- identify runtime behavior currently duplicated, implied, or owned by the wrong repository;
- write focused RFC updates where implementation pressure changes the initial model.

Exit condition: the first VM implementation can be derived from current contracts rather than from assumptions in this scaffold.

Status: COMPLETE (campaign 1). Survey covered the research backend, `BackendArtifact`, execution corpus, callable identity, session/call APIs, effects/capabilities, bounded iteration, evidence, Commons pressure exchange, and Fabric/Actions/Test/Debug/Store/Memory boundaries. Pressures live in `pressures/`.

## Phase 1 — Canonical VM artifact and loader

Define the smallest executable artifact that can stand independently of compiler internals.

The artifact should bind, as required:

- artifact identity and format/revision identity;
- callable/module identities;
- typed value interface;
- executable instructions/control structure;
- constants and metadata required for execution;
- declared effects and capability requirements;
- relevant resource/bound information;
- translation/provenance references needed to connect the artifact back to compiler evidence;
- compatibility requirements for the VM/runtime environment.

Build:

- decoder/loader;
- structural validation;
- compatibility checking;
- deterministic artifact identity validation;
- machine-readable admission diagnostics.

Exit condition: a frozen VM artifact can be loaded and either admitted or refused without invoking compiler internals.

Status: COMPLETE (campaign 1) as `mncs.vm.artifact/1` (`src/artifact.rs`, `src/admit.rs`) with content identity, callable resolution, bound-dimension checks, region consistency checks, and typed refusals. One honest deviation: the code section still carries compiler-selected SSA (P-VM-COMPILER-001 tracks direct emission), and frozen interchange of wide integers is range-limited with fail-closed refusal (P-VM-COMPILER-002, P-VM-ARTIFACT-007).

## Phase 2 — Reference execution core

Implement a small, boring, inspectable reference engine before optimizing it.

Establish:

- machine values;
- frames and call/return behavior;
- control transfer;
- arithmetic/logical operations required by the first artifact profile;
- structured completion/failure/traps;
- deterministic step semantics;
- instruction/step accounting;
- exact callable invocation by identity.

Use the existing research-bytecode interpreter as a differential pressure source where useful, but do not preserve accidental implementation details.

Exit condition: the compiler can emit at least one VM artifact and the standalone VM can execute a bounded corpus with identity-bound results.

Status: COMPLETE (campaign 1). The engine (`src/engine.rs`) executes the admitted SSA subset over `tests/corpus/` with identity-bound calls; `tests/differential.rs` pins value and status agreement with the unmodified research interpreter. Known boundary: general recursion is rejected upstream (MNE130), so depth comes from bounded iteration, not unbounded calls.

## Phase 3 — Runtime memory semantics

Define and implement the execution-local memory model required by real MNCS programs.

Likely pressure areas:

- stack/frame-local values;
- regions/arenas;
- bounded aggregates;
- references/handles;
- allocation and reclamation;
- aliasing/provenance constraints where runtime enforcement is required;
- explicit lifetime/failure behavior;
- snapshots or transition digests needed by verification/debug consumers.

Do not confuse runtime memory with `mncs-memory` semantic memory or `mncs-store` persistence.

Exit condition: representative bounded data/collection workloads execute without relying on hidden compiler-owned interpreter state.

Status: COMPLETE for current workloads (campaign 1). Frames, immutable reference-counted values, and cell accounting cover records, finite values, and bounded sequences with no collector and no hidden interpreter state. Handles, linear memory, snapshots, and explicit regions remain future scope pending real demand.

## Phase 4 — Capabilities and effect mediation

Make runtime authority real.

Build a host/provider interface in which:

- an execution is admitted with an explicit capability environment;
- effectful operations identify the authority they require;
- calls fail closed when authority is absent;
- provider identity/configuration can be retained where relevant;
- provider failure and capability refusal are distinct outcomes;
- externally observed values/events can be represented without laundering nondeterminism into the deterministic VM core.

Exit condition: effectful test workloads can prove allow/deny behavior at the runtime boundary.

Status: COMPLETE (campaign 1). `src/capability.rs` plus `tests/capabilities.rs` prove allow/deny/failure-split over a real `clock_read` HostCall with stub providers. No sandbox claim is made (see boundary invariants).

## Phase 5 — Resource envelopes and bounded execution

Turn compiler/runtime bounds into enforceable execution contracts.

Potential resource dimensions include:

- instruction/step count;
- call depth/count;
- bounded iteration;
- task count;
- memory/region growth;
- effect count;
- provider-specific budgets where explicitly contracted.

Accounting must state exactly what is measured. Do not present a VM step counter as universal CPU cost.

Exit condition: budget exhaustion is deterministic, structured, testable, and visible to callers.

Status: COMPLETE (campaign 1). Steps, call depth, memory cells, effects, and iterations exhaust as structured `BudgetExhausted` outcomes (`tests/resources.rs`). Accounting is stated exactly in `docs/ARCHITECTURE.md`.

## Phase 6 — Execution-local scheduling

Add scheduling only under demonstrated language/runtime pressure.

Possible concepts:

- cooperative tasks;
- yields/suspensions;
- bounded queues;
- deterministic ready ordering;
- joins/waits;
- cancellation;
- effect/provider waits.

This phase does not own wall-clock workflow automation or distributed placement.

Exit condition: concurrency/suspension semantics can be executed and reproduced according to an explicit VM contract.

## Phase 7 — Evidence, replay, debug, and test surfaces

Make the VM a high-quality machine-native runtime target.

Provide structured interfaces for:

- execution identity;
- transition traces/digests;
- resource observations;
- capability/effect observations;
- traps/failures;
- stepping/breakpoints where semantically meaningful;
- snapshots and bounded replay;
- correlation to source/compiler identities without embedding the compiler.

Integrate with `mncs-test` and `mncs-debug` through structured contracts rather than log parsing.

Exit condition: test/debug tooling can inspect a VM execution without custom knowledge of the interpreter implementation.

Status: SUBSTANTIALLY COMPLETE (campaign 2). `mncs.vm.debug/1`
(`src/debug.rs`) ships observation hooks in the shared stream shape,
safe points with stated invariants, identity-bound stop conditions,
typed stop records with continuation tokens, bounded read-only
inspection, resume/step/terminate, deterministic value identities,
transition digests, and the `mncs-vm` binary (`run` one-shot plus
the `debug` JSONL driver over stdio/socket). `mncs-debug` drives
live sessions through it. Remaining: live watch stops,
branch-condition stops, and compiler-owned SSA→source correspondence
for VM operations (pressure with the compiler owner).

## Phase 8 — Fabric/runtime packaging

Make the VM deployable as a clean execution component.

Define:

- runtime package identity;
- artifact/config/provider bundle inputs;
- target/host requirements;
- invocation and result envelope;
- environment observations required for evidence;
- lifecycle/cleanup semantics.

Fabric owns where the VM runs. The VM owns what its execution means.

Exit condition: Fabric can place and invoke the VM as a generic runtime without importing compiler internals.

## Phase 9 — Migration from language-owned research runtime

Once the dedicated runtime is proven:

- route the language VM backend to the canonical `mncs-vm` artifact contract;
- migrate bounded execution and corpus tests;
- migrate identity/call/session consumers;
- compare the old and new paths over frozen corpora;
- remove or reduce obsolete research-bytecode execution machinery from `mncs-language`;
- update Commons pressures and architecture docs to reflect final ownership.

Exit condition: there is one canonical MNCS-native bytecode runtime path.

## Phase 10 — Optimized engines behind the same semantics

Only after the reference engine is trustworthy, investigate performance paths such as:

- threaded/decoded interpreters;
- cached validation/decoding;
- specialization;
- baseline or optimizing JIT;
- ahead-of-time VM artifact translation;
- vectorized primitives;
- accelerator/provider dispatch;
- snapshot/preinitialization strategies.

Optimization must preserve the same observable VM semantics and evidence obligations. Differential conformance against the reference engine should remain available.

## Continuous pressures

These are not separate product versions. They apply throughout development:

### Security

Separate capability enforcement from stronger isolation claims. Add process/memory/provider hardening only with explicit threat models and evidence.

### Performance

Keep a fast local conformance lane. Measure before optimizing. Do not let family-wide verification become the only feedback loop.

### Portability

Keep host assumptions explicit. The reference runtime should avoid unnecessary coupling to one ISA, OS, allocator, or process model.

### Machine-native representation

Prefer typed, structured runtime records and stable identities over strings, implicit host state, and ad hoc JSON when native MNCS representations become available.

### Debt removal

When the VM replaces runtime machinery elsewhere, migrate and delete the duplicate path. Do not preserve obsolete implementations simply because they once worked.

## Near-term first campaign

The first implementation campaign should answer these concrete questions before broadening scope:

1. What exact artifact should `mncs-language` emit for `mncs-vm`?
2. Which parts of research bytecode are genuine VM semantics versus compiler experiment scaffolding?
3. What is the smallest useful standalone loader/executor?
4. How are callable identity, capabilities, resource bounds, and structured failures represented at runtime?
5. What current APIs need to move, remain upstream, or become neutral integration contracts?
6. Which frozen corpora can prove the new VM path agrees with the current bounded execution path where agreement is expected?

Answer those through code and evidence before expanding into speculative subsystems.
