# VM pressure records

Implementation-grounded pressures against read-only upstream
repositories, produced by building `mncs-vm`. Each file states the
owner, the observed behavior, the runtime consequence, the smallest
general missing capability, the required upstream change, evidence,
blocked VM work, temporary behavior, and the removal condition.

Index:

- `P-VM-COMPILER-001.md` — no direct canonical artifact emission
- `P-VM-COMPILER-002.md` — frozen interchange integer fidelity
- `P-VM-COMPILER-003.md` — no CLI surface emits frozen artifact bytes
- `P-VM-LANG-001.md` — requests are name-bound, not identity-bound
- `P-VM-LANG-002.md` — executable section embeds compiler internals
- `P-VM-ARTIFACT-007.md` — VM-owned bignum-exact encoding (sibling of 002)

`mncs-language` and `mncs-compiler` were not modified during this
campaign. These records are written so the agent working there can
act without rediscovering the problem.
