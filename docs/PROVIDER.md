# Selected runtime provider

`vm-runtime/1`, `vm-admission/1` and `vm-call/1` supplement the existing
`vm-artifact/0.1.0` contract. The manifest declares artifact, selected SSA,
request, session and debug revisions. `mncs-vm describe` reports the actual
executable's supported contracts; declarations alone do not prove admission.

`mncs-vm admit --artifact FILE` performs standalone admission and emits a
structured admitted/refused result. `mncs-vm serve --artifact FILE` is a bounded
stdio session: decode, admit and immutable index construction occur once, then
isolated calls reuse that admitted artifact. It has no listener or host capability
bindings. Effectful one-shot/debug execution retains its explicit provider model.

The readiness line and each request/response use `mncs.vm.session/1`. Requests
carry correlation `id`, the existing `ExecutionRequest`, an optional explicit
`ResourceEnvelope`, and an optional sealed first-class Test callable reference.
Malformed frames and mismatched Test references refuse without executing a call.
Frames are limited to 16 MiB. Missing request budgets retain finite defaults;
explicit overrides use the existing `CallSpec::from_request` contract. Counters,
live values and mutable call state reset between requests. VM steps remain
instruction accounting, not universal CPU cost.

The selected-checkout client in `python/mncs_vm_client` observes exact executable
and artifact SHA-256, validates supplied compiler build receipts, checks file
replacement across admission/calls and bounds responses/timeouts. It terminates
only its own child. Execution provenance uses
`mncs.provider-execution-provenance/1`; it does not manufacture Test or assurance
PASS verdicts. Runtime build origin is **unknown** unless separately established:
an executable hash and repository HEAD are not a compiler build certificate.

Compiler/VM source and operation identities remain compiler-owned. Runtime debug
continues through `debug --serve`; no source mapping logic moved into VM. The
migration converter remains deleted with zero callers. Corpus harnesses use the
single compiler-owned direct emitter; frozen admission needs no compiler session.

`cargo test --offline` includes retained-session malformed-frame, nested-call,
fuel exhaustion/reset, reference-refusal and isolation checks, in addition to
existing generic, live-memory and iteration-metering tests.
