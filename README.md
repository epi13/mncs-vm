# mncs-vm

<!-- MNCS:generated:begin -->
<!-- MNCS:generated:end -->

Canonical machine-native virtual machine and execution runtime for MNCS bytecode.

> **Core thesis:** MNCS needs a native execution target whose runtime behavior preserves the language's explicit contracts, effects, capabilities, bounds, identity, and evidence instead of flattening them into an opaque host process.

`mncs-vm` is the execution layer between compiler-produced MNCS VM artifacts and the wider MNCS runtime environment. It is intended to provide a deterministic core execution model, explicit nondeterminism/effect boundaries, capability enforcement, bounded resource accounting, runtime memory semantics, call/task scheduling, structured failure, and identity-preserving observations that other MNCS systems can inspect and verify.

This repository began documentation-first; campaign 1 built the first executable foundation (see "Implemented state" below). The research-bytecode interpreter inside `mncs-language` was not copied: it serves as a read-only differential oracle while the VM executes through its own engine under VM-owned runtime rules.

## Implemented state (campaign 1)

One Rust crate (`cargo test` is the fast loop, ~30 tests):

- `mncs.vm.artifact/1` canonical artifact with content identity, identity-bound callables, declared bounds/capabilities, and an SSA code section;
- loader/admission with typed refusals (malformed, unsupported, unresolved, incompatible, identity-mismatch);
- reference engine over the admitted SSA subset (integers via upstream evaluation, calls, branches, records, finite values, sequences, bounded iteration, effects, provider-dispatched host calls);
- enforced resource envelopes (steps, call depth, memory cells, effects, iterations);
- capability allow/deny with fail-closed dispatch and provider boundary;
- structured outcomes plus `ExecutionRecord` evidence with digests;
- differential agreement with the unmodified research interpreter over `tests/corpus/`;
- six upstream pressure records in `pressures/` (the compiler/language agent's intake).

```bash
cargo test --offline        # full suite: admission, execution, differential, resources, capabilities
python3 scripts/run_tests.py  # same suite plus evidence validation
cargo run --offline --example dump -- <corpus.mncs>  # inspect admitted SSA
```

Read-only upstream constraint honored: `mncs-language` and `mncs-compiler` were inspected, never modified. Everything the VM could not do without an upstream change is a pressure record, not a workaround.

## What this repository owns

`mncs-vm` owns the **execution semantics of the MNCS-native virtual machine**.

That includes, as the implementation develops:

- the canonical MNCS VM artifact/container contract and virtual instruction set needed to execute compiler-lowered programs;
- artifact loading, structural validation, compatibility checks, and runtime admission;
- the machine value model used at the execution boundary;
- frames, calls, returns, control transfer, traps, and structured completion;
- runtime-local memory semantics such as stacks, regions, heaps, linear memory, handles, and transient execution state where required by the VM design;
- capability checks and effect mediation at the point an executing program attempts to cross an authority boundary;
- deterministic execution rules for the pure/runtime core and explicit representation of nondeterministic or external observations;
- bounded task/call scheduling and execution ordering where concurrency or cooperative work is part of the VM contract;
- resource metering and budget enforcement for instructions, calls, memory, effects, or other bounded resources represented by MNCS contracts;
- runtime identities, traces, transition digests, failure records, and other execution evidence;
- host/provider interfaces through which external capabilities are made available without becoming VM-owned application logic;
- portable interpreter/reference execution and, later, optimized execution strategies that preserve the same VM semantics;
- debugging, stepping, inspection, snapshot, replay, and observability hooks needed by the wider MNCS verification/tooling stack.

The VM should eventually be able to answer questions such as:

- Exactly which frozen artifact is executing?
- Which callable identity was invoked?
- Which capabilities and resource budgets were admitted?
- What runtime state may this instruction or call observe or mutate?
- Which effects were attempted, authorized, refused, or completed?
- Why did execution complete, trap, suspend, or fail?
- What nondeterministic inputs or external observations influenced the result?
- Can this execution be replayed or compared under the same bounded inputs?
- Which observations can be retained as evidence without claiming more than was actually measured?

## What this repository does not own

Keep the boundary sharp.

- **`mncs-language`** owns source-language meaning, types, contracts, effects/capabilities as language semantics, HIR/SSA, legality, compiler transformations, and lowering decisions. The VM executes a frozen target artifact; it does not redefine source semantics to fit the runtime.
- **Other `mncs-language` backends** such as WASM, LLVM, C11, Cranelift, eBPF, RISC-V, or PTX remain valid realization paths. `mncs-vm` is the native MNCS execution target, not a requirement that every program execute through this VM.
- **`mncs-memory`** owns semantic/agent/model memory, persistence and retrieval behavior, and memory-specific reasoning contracts. VM stacks, heaps, regions, and transient runtime state are execution memory, not `mncs-memory`.
- **`mncs-store`** owns durable storage of artifacts, state, checkpoints, traces, and other persisted content. The VM may load or emit such artifacts through contracts but should not become a storage engine.
- **`mncs-fabric`** owns placement, worker/device selection, heterogeneous hosts, distributed execution, and run-environment orchestration. Fabric may place and invoke a VM instance; the VM owns the semantics of what happens inside that execution boundary.
- **`mncs-actions`** and capability/provider systems own concrete external actions and provider behavior. The VM validates and mediates authority to call them; it does not absorb their domain logic.
- **`mncs-automation`** owns user/workflow scheduling over wall-clock time and external triggers. VM scheduling is only execution-local scheduling required to run an admitted program.
- **`mncs-test`, `mncs-debug`, `mncs-doctor`, and `mncs-forge`** own testing, diagnosis, health assessment, experimentation, and orchestration. They may consume VM traces and control interfaces without becoming part of VM semantics.
- **`mncs-models` and `mncs-learn`** own model structure and learning semantics. The VM may execute compiled model workloads or dispatch to approved accelerators/providers without defining model architecture or learning policy.
- **`mncs-rights-provenance`** owns rights/provenance policy and lineage. The VM preserves and reports relevant identities rather than inventing provenance authority.

## Relationship to the current research-bytecode backend

`mncs-language` currently has a research-bytecode backend with a bounded SSA interpreter. It proves that a native interpreter-shaped execution path is useful, but it is still language-repository research infrastructure.

The intended direction is not to maintain two permanent MNCS bytecode runtimes. Instead:

1. inspect the current research-bytecode artifact, interpreter, execution corpus, callable identity, effect/capability, and evidence contracts;
2. determine which concepts belong to the stable VM boundary and which are experimental compiler scaffolding;
3. define the MNCS VM artifact/runtime contract here;
4. teach `mncs-language` to lower to that contract through its normal backend interface;
5. migrate current consumers and verification paths;
6. remove or reduce obsolete duplicate runtime machinery once the dedicated VM path is proven.

The existing path is migration input, not a compatibility mandate.

## Design principles

### One evolving implementation

MNCS currently controls the relevant consumers, so there is no reason to freeze half-finished generations of the VM.

**Build one canonical implementation and migrate the ecosystem forward with it.** Do not create `v1`, `v2`, `legacy`, `next`, or parallel runtimes merely to avoid updating current repositories. Versioned serialized artifacts may need explicit revision identities, but those revisions are not permission to maintain competing product architectures.

### Language meaning remains upstream

The VM is allowed to reject an artifact it cannot safely or correctly execute. It is not allowed to reinterpret language semantics to make execution convenient.

Compiler legality, type meaning, effect declarations, capability requirements, bounds, and translation evidence remain upstream responsibilities. Runtime validation should confirm the artifact is structurally admissible and that required runtime facts/authority are present.

### Deterministic core, explicit nondeterminism

Do not claim that every useful program is deterministic. External I/O, clocks, random sources, devices, networks, users, models, and remote providers can introduce nondeterminism.

The VM should instead make the deterministic portion of execution reproducible and force nondeterministic observations through explicit interfaces that can be identified, recorded, bounded, replayed where possible, or honestly marked non-replayable.

### Authority is checked where it is exercised

Capabilities are not decorative metadata. A VM-level effect boundary should fail closed when an instruction/call lacks the admitted authority required to cross it.

This does **not** automatically make the VM a hardened security sandbox. Security claims must remain scoped to what has actually been isolated, tested, and verified.

### Bounded execution is a first-class runtime concept

Where MNCS artifacts carry instruction, iteration, recursion, call, memory, or effect budgets, the VM should preserve and enforce those bounds rather than treating them as comments for external tooling.

Resource accounting must be deterministic enough to be useful as evidence and explicit about what it does not measure.

### Machine-native observability

Runtime events should be structured and identity-bearing rather than primarily log strings. Human-readable output can be derived from the same underlying execution records used by agents, tests, debuggers, and verifiers.

## Provisional execution shape

The vocabulary will evolve with implementation pressure, but a likely execution path is:

```text
MNCS source
    |
    v
mncs-language compiler
    |
    v
selected SSA + lowering/evidence
    |
    v
MNCS VM artifact
    |
    v
artifact loader / verifier
    |
    v
runtime admission
  artifact identity
  capabilities
  resource envelope
  host/provider bindings
    |
    v
execution engine
  values / frames / control
  runtime memory
  task scheduling
  effect mediation
  metering
    |
    +----> external capability/provider boundary
    |
    v
structured completion
  result / failure
  effects
  resource use
  transition identities
  execution evidence
```

## Native VM versus host backends

The VM should be especially useful for workloads where MNCS wants:

- portable semantics independent of a particular host ISA;
- strong identity and evidence continuity from compiler artifact to runtime observation;
- fine-grained capability/effect mediation;
- deterministic metering and bounded execution;
- inspectable execution for test/debug/verification;
- rapid evolution of machine-native semantics before mapping them efficiently to every native backend.

It should not become an excuse to prevent direct compilation to WASM, native code, accelerators, GPUs, or other appropriate targets.

## Repository map

```text
AGENTS.md
    contributor/agent invariants and entry procedure

README.md
    project purpose, ownership boundary, and execution thesis

ROADMAP.md
    ordered path from runtime survey to canonical MNCS VM

mncs-boundary.json
    machine-readable ownership and integration boundary

rfcs/0001-machine-native-virtual-machine.md
    initial normative architecture decision

docs/ARCHITECTURE.md
    provisional VM execution architecture and invariants

docs/INTEGRATION.md
    contracts and migration boundaries with the wider MNCS family
```

Implementation status: the artifact contract, admission, reference engine, capabilities, resources, sessions, evidence, and the live-debug contract (`mncs.vm.debug/1` with the `mncs-vm run` / `mncs-vm debug` binary) are implemented in `src/` with conformance suites in `tests/`. `python3 scripts/run_tests.py` is the fast verification loop. New work should extend the implemented contracts and their tests rather than re-surveying from scratch; pressures live in `pressures/`.

## Definition of a healthy foundation

The project is moving in the intended direction when:

- one canonical MNCS VM artifact can be emitted by the compiler and executed independently of compiler internals;
- source-language semantics do not leak into ad hoc runtime reinterpretation;
- runtime admission binds the exact artifact, capabilities, resource envelope, and provider environment being executed;
- pure execution is reproducible and external nondeterminism is explicit;
- capability checks occur at effect boundaries;
- bounded resources are actually enforced and their accounting is inspectable;
- failures/traps/suspensions are structured outcomes rather than process accidents;
- runtime observations retain stable identities suitable for test, debug, replay, and evidence workflows;
- Fabric can place the VM without owning VM semantics;
- the existing research-bytecode interpreter can eventually be retired or reduced rather than preserved as permanent parallel debt;
- optimized engines can be added behind the same semantics without creating incompatible VM generations.

## Start here

Agents and contributors should read, in order:

1. [`AGENTS.md`](AGENTS.md)
2. [`rfcs/0001-machine-native-virtual-machine.md`](rfcs/0001-machine-native-virtual-machine.md)
3. [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
4. [`docs/INTEGRATION.md`](docs/INTEGRATION.md)
5. [`ROADMAP.md`](ROADMAP.md)

Then inspect the current heads and relevant RFCs/specifications of `mncs-language`, `MNCS-Commons`, and the runtime consumers before implementing code.

## License

Apache-2.0.
