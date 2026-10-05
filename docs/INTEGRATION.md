# MNCS VM Integration Boundaries

`mncs-vm` sits between compiler-produced native MNCS artifacts and the systems that place, authorize, observe, persist, test, or debug execution.

This document describes ownership boundaries. It is not a declaration that every named integration already exists.

## `mncs-language`

### Language owns

- source syntax and semantics;
- types and contracts;
- effect/capability declarations as language meaning;
- HIR/SSA/selected SSA;
- compiler legality;
- lowering plans;
- backend selection;
- translation evidence;
- source/compiler diagnostics.

### VM owns

- the native VM executable artifact contract to the extent it is target/runtime-specific;
- loading/admission of that artifact;
- execution semantics;
- runtime-local memory;
- runtime capability enforcement;
- runtime resource enforcement;
- structured execution observations.

### Integration direction

`mncs-language` should eventually expose `mncs-vm` through its ordinary backend-neutral realization boundary. The compiler should emit an identity-bound VM artifact and associated translation/evidence records without importing VM interpreter internals.

The VM must be executable independently from the compiler.

## Current research-bytecode path

The existing `mncs-research-bytecode` backend and bounded SSA interpreter are a precursor, not the permanent architectural boundary.

Campaign 1 established the concrete migration shape:

- Current state (2026-10-05): `src/migrate.rs` is deleted. All VM harness/corpus/differential consumers use the compiler-owned direct emitter (`mncs-compiler/tools/vm_emit.rs`); no legacy conversion consumer remains. The historical research boundary described below remains upstream-owned;
- the research interpreter is a read-only differential oracle (`mncs_model::execute_ssa` over the same compilation), never modified, never linked as an execution path;
- `tests/differential.rs` pins value and status agreement over the corpus;
- compiler legality, translation evidence, integer meaning, and type shapes stay upstream; frames, metering, capabilities, outcomes, and evidence are VM-owned.

Do not maintain both paths indefinitely as artificial compatibility generations. The desired end state is not two MNCS bytecode runtimes: once the compiler emits the canonical artifact directly, the adapter deletes and the research interpreter remains only as upstream's own tooling, not as a competing runtime.

## Per-system status (campaign 1)

- `mncs-test`: VM conformance lives here (`tests/`); family matrices are not run per edit — `cargo test` in this repo is the fast loop.
- `mncs-debug`: consumes `ExecutionRecord` JSON, structured outcomes, and the live-debug contract (`Session::start_debug`, `mncs.vm.debug/1`): identity-bound stops, typed inspection, resume/step/terminate, and shared-shape observation streams. The `mncs-vm debug` JSONL driver (stdio or socket) is the process boundary.
- `mncs-fabric`: can bind artifact id + envelope + capability names around `Session::call`, but no Fabric driver exists yet.
- `mncs-store`: no persistence contract yet; `VmArtifact` JSON and `ExecutionRecord` JSON are the shapes to persist when defined.
- `mncs-actions`: providers implement the foreign `Provider` trait; no domain logic in the VM.
- `mncs-memory` / `mncs-automation`: untouched by design (semantic memory and wall-clock scheduling are not VM concerns).

## `MNCS-Commons`

Commons is the cross-repository pressure/evidence coordination layer.

VM development should:

- consume relevant pressures before inventing local workarounds;
- publish missing language/runtime/provider primitives as explicit pressure;
- preserve pressure identities where required by the family workflow;
- avoid making Commons the runtime implementation itself.

## `mncs-fabric`

Fabric owns **where and under what environment** execution is placed.

The VM owns **what native VM execution means once admitted**.

A Fabric-to-VM invocation may eventually bind:

- VM/runtime package identity;
- VM artifact identity;
- target/host facts;
- resource envelope;
- capability/provider bindings;
- input values;
- observation policy.

The result should retain enough environment identity for Fabric to report scoped execution evidence without redefining VM semantics.

Fabric should not need compiler internals to execute a frozen VM artifact.

## `mncs-actions` and provider systems

Actions/providers own concrete external operations.

The VM may own:

- provider handle/binding representation;
- capability checks before dispatch;
- structured request/response transition at the VM boundary;
- accounting and observation of the call.

The VM must not own domain-specific provider logic.

A provider must not gain unrestricted host authority merely because it is callable from the VM process.

## `mncs-automation`

Automation owns wall-clock and event-driven workflow scheduling.

VM scheduling is limited to execution-local semantics such as tasks, yields, waits, joins, and cancellation required by an admitted program.

Do not put cron, reminders, background workflow policy, or distributed job queues into the VM merely because both concepts use the word "schedule."

## `mncs-memory`

`mncs-memory` owns semantic memory and retrieval behavior.

VM-owned memory is runtime-local machinery:

- stacks;
- frames;
- regions;
- bounded aggregates;
- heaps/arenas where needed;
- handles/references;
- transient snapshots.

A program may call `mncs-memory` through an explicit capability/provider contract. The VM must not reimplement semantic memory as a heap subsystem.

## `mncs-store`

Store owns durability.

The VM may consume or emit:

- executable artifacts;
- runtime packages;
- snapshots/checkpoints;
- traces;
- results;
- cached decoded artifacts;
- evidence records.

Their durable lifecycle, indexing, and content storage belong to Store.

The VM should be able to operate over content supplied by Store without becoming coupled to one persistence backend.

## `mncs-test`

Test should be able to invoke the VM through a stable execution contract and compare structured outcomes.

Useful integration surfaces include:

- artifact admission tests;
- expected result/status tests;
- capability denial tests;
- budget exhaustion tests;
- deterministic replay tests;
- transition digest comparisons;
- differential execution against other backends.

Test should not need to scrape runtime logs to determine semantic outcomes.

## `mncs-debug`

Debug consumes runtime introspection.

Shipped VM surfaces (`mncs.vm.debug/1`, `src/debug.rs`):

- execution/call identities (`mncs:vm:execution:<digest>`);
- current frame/control position (safe points with stated invariants);
- structured stack/frame state (bounded typed views, read-only);
- bounded trace/transition history (shared observation stream);
- resource counters (per stop and terminal);
- capability/provider transitions (effect log + invoke/result events);
- traps/failures (terminal safe points; inspectable, not resumable);
- step/resume/terminate interfaces with single-owner tokens;
- transition digests and deterministic value identities for replay
  comparison (forward re-execution; no reverse execution).

Debug policy and diagnosis belong to `mncs-debug`. The VM exposes factual runtime state/events.

## `mncs-doctor`

Doctor may inspect runtime health, conformance evidence, compatibility, configuration, or packaging.

The VM should expose machine-readable facts such as:

- runtime build identity;
- supported artifact/features envelope;
- host requirements;
- provider compatibility;
- validation status;
- self/conformance test evidence where available.

Doctor decides what those facts mean for system health.

## `mncs-forge`

Forge may use the VM as an execution/evidence target while developing or validating MNCS artifacts.

Forge may orchestrate:

- compilation;
- VM execution;
- corpus runs;
- backend comparisons;
- performance experiments;
- verification campaigns.

Forge does not become the VM runtime or semantic authority.

## `mncs-models`

Models owns model construction/topology.

The VM may execute compiled model graphs or invoke model-specific runtime providers/accelerators when those interfaces are explicit.

Do not embed model architecture definitions into VM instructions unless genuine broad runtime pressure shows a reusable primitive is required.

Prefer generic tensor/vector/parallel/runtime primitives or provider dispatch over architecture-name opcodes.

## `mncs-learn`

Learn owns learning/adaptation semantics and evidence-governed updates.

The VM may execute learning workloads, expose runtime measurements, and mediate access to parameter/state providers. It does not decide whether a model update should be accepted, rejected, or promoted.

## `mncs-rights-provenance`

Rights/provenance owns policy and lineage.

The VM should preserve relevant artifact/provider/input identities and emit observations needed to connect an execution to provenance records.

The VM must not invent rights conclusions from the presence of an artifact.

## `mncs-crypto`

Crypto may provide canonical hashing, signatures, authenticated structures, or cryptographic primitives used by VM artifacts/evidence.

The VM should depend on explicit crypto contracts rather than growing an unrelated crypto stack internally.

## `mncs-data`

Data systems may define machine-native data representation/transport contracts used by executable workloads.

The VM owns only the runtime ABI/representation necessary to execute those values. It should not absorb dataset, schema-governance, or data-pipeline ownership.

## Cross-backend relationship

The native VM must coexist cleanly with non-VM backends.

A single selected SSA may be realized as:

```text
selected SSA
   |-- MNCS VM artifact -> mncs-vm
   |-- WASM artifact    -> WASM runtime
   |-- LLVM/native      -> host executable
   |-- Cranelift        -> JIT/AOT path
   |-- eBPF/RISC-V/PTX  -> target environment
   `-- future backend
```

Agreement across these backends can be valuable evidence over bounded corpora. It does not make the VM the semantic authority for the language, nor does it prove universal equivalence.

## Integration anti-patterns

Avoid these designs:

### Compiler embedded in the VM

A runtime should not need to reconstruct source semantics or rerun the compiler merely to invoke a frozen artifact.

### VM embedded in Fabric

Fabric placement code should not become the only implementation of VM execution semantics.

### Provider logic embedded in bytecode instructions

Keep external actions behind general capability/provider boundaries rather than adding one VM opcode per service.

### Semantic memory confused with heap memory

`mncs-memory` and runtime allocation solve different problems.

### Logs as API

If Test/Debug/Forge need an event, expose it structurally.

### Permanent duplicate runtime

Once `mncs-vm` is proven, migrate and remove the old language-owned duplicate rather than carrying it indefinitely.

## Integration completion criteria

A mature integration boundary should permit:

1. `mncs-language` to compile without importing the VM execution engine;
2. `mncs-vm` to execute a frozen artifact without importing compiler internals;
3. Fabric to place/invoke the VM without redefining its semantics;
4. Actions/providers to receive only explicitly authorized requests;
5. Test and Debug to consume structured execution data;
6. Store to persist artifacts/results without defining runtime behavior;
7. Memory, Models, Learn, and other systems to participate through explicit interfaces rather than repository ownership leaks;
8. the obsolete research runtime to be removed once migration is complete.


## Canonical request envelopes (2026-10-05)

`mncs-vm batch` reads `[{"id": "case", "request": ExecutionRequest}]`.
`CallSpec::from_request` validates schema `0.1`, binds the target and generic
arguments, and carries `step_budget` and optional `call_depth_budget` into
the VM envelope. Remaining dimensions use finite VM defaults. An explicit
`--envelope` only tightens request/default limits; artifact-declared bounds
also tighten limits. Duplicate/unknown override dimensions are refused.
Host grants and non-default execution policies require explicit VM capability
bindings and are refused by this batch adapter. No authority is inferred.

Compiler/language iteration bounds stay in frozen SSA; request fuel stays
in the call contract; VM consumption stays in `ExecutionRecord.usage`.
Records report effective merged limits, including artifact bounds. Nested
and generic calls use the same execution envelope. A numerical request fuel
limit applies to this executor's steps (instructions and terminator edges),
not universal CPU cost or equal work across backends. Zero is a finite fuel
limit and deterministically exhausts. Missing request fuel cannot decode.
The low-level explicit `CallSpec` API remains available to embedded callers.

See [SSA-ENCODING.md](SSA-ENCODING.md) for the single shared encoding and
measurement workflow. VM development compilation requires the sibling
compiler emitter source; frozen artifact execution requires no live compiler.
