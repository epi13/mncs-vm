//! Generic/type-argument call boundary over real compiler source.
//!
//! The compiled program is `mncs-compiler/src/compiler/source.mncs`
//! (read from the sibling checkout, seeded so `byte_at<N>` and
//! `ascii<N>` instantiations are compiled in). The same compilation
//! feeds the upstream `execute_ssa` oracle and the VM engine:
//! agreement plus concrete expected values is the generic proof.
//!
//! Negative coverage pins the fail-closed contract: unmatched
//! instantiations, missing arguments, arguments for concrete
//! targets, and malformed entrypoint rows are all explicit
//! refusals, never guesses.

use mncs_vm::admit::AdmissionRefusal;
use mncs_vm::capability::CapabilityEnv;
use mncs_vm::engine::CallTarget;
use mncs_vm::harness;
use mncs_vm::outcome::Outcome;
use mncs_vm::resource::{ResourceEnvelope, ResourceLimit};
use mncs_vm::session::{CallSpec, Session};
use mncs_vm::value::to_wire;

const MODULE: &str = "mncs.compiler.source.v1";
const LOCATOR: &str = "mncs-compiler/src/compiler/source.mncs";

fn compiler_source_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../mncs-compiler/src/compiler/source.mncs")
}

fn u64_arg(value: u64) -> mncs_model::ExecutionValue {
    mncs_model::ExecutionValue::Integer {
        value: value as i128,
        ty: mncs_model::IntegerType {
            bits: 64,
            signed: false,
        },
    }
}

fn byte_seq(data: &[u8]) -> mncs_model::ExecutionValue {
    mncs_model::ExecutionValue::Sequence {
        values: data
            .iter()
            .map(|byte| mncs_model::ExecutionValue::Byte {
                value: i128::from(*byte),
            })
            .collect::<Vec<_>>()
            .into(),
    }
}

fn nat(value: u32) -> mncs_model::ExecutionTypeArgument {
    mncs_model::ExecutionTypeArgument::Nat { value }
}

fn envelope() -> ResourceEnvelope {
    ResourceEnvelope {
        limits: vec![
            ResourceLimit {
                dimension: "steps".to_owned(),
                limit: 50_000,
            },
            ResourceLimit {
                dimension: "call_depth".to_owned(),
                limit: 128,
            },
            ResourceLimit {
                dimension: "memory_cells".to_owned(),
                limit: 500_000,
            },
            ResourceLimit {
                dimension: "effects".to_owned(),
                limit: 128,
            },
        ],
    }
}

fn seed(function: &str, n: u32) -> mncs_model::HostGenericSeedRequest {
    mncs_model::HostGenericSeedRequest {
        module: MODULE.to_owned(),
        function: function.to_owned(),
        type_arguments: vec![nat(n)],
    }
}

fn seeded_artifact(
    seeds: &[mncs_model::HostGenericSeedRequest],
) -> (mncs_model::Program, mncs_vm::admit::Admitted) {
    let path = compiler_source_path();
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("sibling compiler source missing: {}", path.display()));
    let (program, backend) =
        harness::compile_to_backend_seeded(&source, LOCATOR, seeds).expect("seeded compile");
    let admitted = mncs_vm::migrate::admit_research_artifact(&backend).expect("admit");
    (program, admitted)
}

/// Execute one generic callable through the VM and the oracle; assert
/// status agreement, value agreement, concrete expectations, and that
/// the evidence names the resolved specialization.
fn compare_generic(
    program: &mncs_model::Program,
    admitted: &mncs_vm::admit::Admitted,
    function: &str,
    type_arguments: Vec<mncs_model::ExecutionTypeArgument>,
    args: Vec<mncs_model::ExecutionValue>,
    expected: Vec<mncs_model::ExecutionValue>,
) {
    let request = mncs_model::ExecutionRequest {
        schema_version: "0.1".to_owned(),
        target: mncs_model::ExecutionTarget {
            module: MODULE.to_owned(),
            function: function.to_owned(),
        },
        arguments: args.clone(),
        type_arguments: type_arguments.clone(),
        step_budget: 50_000,
        policy: mncs_model::ExecutionPolicy::default(),
        host_grants: Vec::new(),
        call_depth_budget: None,
    };
    let oracle = mncs_model::execute_ssa(program, &request);

    let caps = CapabilityEnv::empty();
    let mut session = Session::open(admitted);
    let (outcome, record) = session.call(
        &caps,
        CallSpec {
            target: CallTarget::ByName {
                module: MODULE.to_owned(),
                name: function.to_owned(),
            },
            arguments: args,
            type_arguments,
            envelope: envelope(),
        },
    );

    assert_eq!(
        outcome.tag(),
        match oracle.status {
            mncs_model::ExecutionStatus::Returned => "completed",
            mncs_model::ExecutionStatus::RuntimeFailure => "program_failure",
            mncs_model::ExecutionStatus::Unsupported => "unsupported",
            mncs_model::ExecutionStatus::BudgetExhausted => "budget_exhausted",
            mncs_model::ExecutionStatus::InvalidRequest => "invalid_request",
        },
        "generic {function}: vm={outcome:?} oracle={:?}",
        oracle.status
    );
    assert_eq!(
        outcome,
        Outcome::Completed,
        "generic {function}: {outcome:?}"
    );
    let vm_values: Vec<mncs_model::ExecutionValue> = record.returned.iter().map(to_wire).collect();
    assert_eq!(
        vm_values, oracle.returned,
        "generic {function}: value divergence"
    );
    assert_eq!(
        vm_values, expected,
        "generic {function}: concrete expectation failed"
    );
    // Evidence names the resolved specialization, not just the source
    // generic: the recorded function is the frozen entrypoint target.
    let row = admitted
        .artifact
        .generic_entrypoints
        .iter()
        .find(|row| row.generic_function == function)
        .expect("entrypoint row");
    assert_eq!(record.callable_function, row.function);
    assert_eq!(
        record.callable_name,
        format!("{MODULE}::{function}<{}>", row.canonical_args)
    );
    assert!(!row.canonical_args.trim().is_empty());
}

#[test]
fn seeded_generic_helpers_execute_through_vm() {
    let (program, admitted) = seeded_artifact(&[seed("byte_at", 8), seed("ascii", 8)]);
    let rows: Vec<_> = admitted
        .artifact
        .generic_entrypoints
        .iter()
        .map(|row| {
            (
                row.generic_function.clone(),
                row.args_spellings.clone(),
                row.canonical_args.clone(),
                row.function.clone(),
            )
        })
        .collect();
    assert!(
        rows.iter()
            .any(|(function, spellings, _, _)| function == "byte_at" && spellings == &["8"]),
        "byte_at<8> entrypoint carried: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|(function, spellings, _, _)| function == "ascii" && spellings == &["8"]),
        "ascii<8> entrypoint carried: {rows:?}"
    );
    // Distinct helpers resolve to distinct frozen targets.
    let byte_at = rows.iter().find(|row| row.0 == "byte_at").unwrap();
    let ascii = rows.iter().find(|row| row.0 == "ascii").unwrap();
    assert_ne!(byte_at.3, ascii.3);

    // byte_at: in-range byte, last byte, out-of-range sentinel 256.
    compare_generic(
        &program,
        &admitted,
        "byte_at",
        vec![nat(8)],
        vec![byte_seq(b"ABCDEFGH"), u64_arg(0)],
        vec![u64_arg(65)],
    );
    compare_generic(
        &program,
        &admitted,
        "byte_at",
        vec![nat(8)],
        vec![byte_seq(b"ABCDEFGH"), u64_arg(7)],
        vec![u64_arg(72)],
    );
    compare_generic(
        &program,
        &admitted,
        "byte_at",
        vec![nat(8)],
        vec![byte_seq(b"ABCDEFGH"), u64_arg(8)],
        vec![u64_arg(256)],
    );
    // ascii: pure-ASCII view passes, high byte fails.
    compare_generic(
        &program,
        &admitted,
        "ascii",
        vec![nat(8)],
        vec![byte_seq(b"ABCDEFGH")],
        vec![mncs_model::ExecutionValue::Boolean { value: true }],
    );
    compare_generic(
        &program,
        &admitted,
        "ascii",
        vec![nat(8)],
        vec![byte_seq(b"ABCDEFG\xff")],
        vec![mncs_model::ExecutionValue::Boolean { value: false }],
    );
}

#[test]
fn distinct_instantiations_stay_distinct() {
    let (_, admitted) = seeded_artifact(&[seed("byte_at", 8), seed("byte_at", 16)]);
    let rows: Vec<_> = admitted
        .artifact
        .generic_entrypoints
        .iter()
        .filter(|row| row.generic_function == "byte_at")
        .collect();
    assert_eq!(rows.len(), 2, "both instantiations carried");
    assert_ne!(rows[0].canonical_args, rows[1].canonical_args);
    assert_ne!(rows[0].function, rows[1].function);
    let wide = rows
        .iter()
        .find(|row| row.args_spellings == ["16"])
        .expect("N=16 row");
    // Both instantiations execute: byte 15 exists only under N=16.
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let data16: Vec<u8> = (0..16u8).collect();
    let (outcome, record) = session.call(
        &caps,
        CallSpec {
            target: CallTarget::ByName {
                module: MODULE.to_owned(),
                name: "byte_at".to_owned(),
            },
            arguments: vec![byte_seq(&data16), u64_arg(15)],
            type_arguments: vec![nat(16)],
            envelope: envelope(),
        },
    );
    assert_eq!(outcome, Outcome::Completed, "{outcome:?}");
    let values: Vec<_> = record.returned.iter().map(to_wire).collect();
    assert_eq!(values, vec![u64_arg(15)]);
    assert!(record.callable_name.contains(&wide.canonical_args));
}

fn invalid_reason(
    admitted: &mncs_vm::admit::Admitted,
    target: CallTarget,
    type_arguments: Vec<mncs_model::ExecutionTypeArgument>,
    args: Vec<mncs_model::ExecutionValue>,
) -> String {
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(admitted);
    let (outcome, _) = session.call(
        &caps,
        CallSpec {
            target,
            arguments: args,
            type_arguments,
            envelope: envelope(),
        },
    );
    match outcome {
        Outcome::InvalidRequest { reason } => reason,
        other => panic!("expected InvalidRequest, got {other:?}"),
    }
}

#[test]
fn generic_call_boundary_fails_closed() {
    let (_, admitted) = seeded_artifact(&[seed("byte_at", 8)]);
    let by_name = |function: &str| CallTarget::ByName {
        module: MODULE.to_owned(),
        name: function.to_owned(),
    };
    // Generic without arguments names the missing selection.
    let reason = invalid_reason(
        &admitted,
        by_name("byte_at"),
        Vec::new(),
        vec![byte_seq(b"ABCDEFGH"), u64_arg(0)],
    );
    assert!(
        reason.contains("requires explicit type_arguments"),
        "unexpected: {reason}"
    );
    // Uncompiled instantiation lists what is compiled.
    let reason = invalid_reason(
        &admitted,
        by_name("byte_at"),
        vec![nat(7)],
        vec![byte_seq(b"ABCDEFG"), u64_arg(0)],
    );
    assert!(
        reason.contains("no compiled specialization") && reason.contains("(8)"),
        "unexpected: {reason}"
    );
    // Arguments for a concrete callable are rejected.
    let reason = invalid_reason(
        &admitted,
        by_name("page_count_for"),
        vec![nat(8)],
        vec![u64_arg(64), u64_arg(137)],
    );
    assert!(
        reason.contains("non-generic function"),
        "unexpected: {reason}"
    );
    // Concrete function targets never take arguments.
    let function = admitted
        .artifact
        .callables
        .iter()
        .find(|entry| entry.name == "page_count_for")
        .expect("concrete callable")
        .function
        .clone();
    let reason = invalid_reason(
        &admitted,
        CallTarget::ByFunction { function },
        vec![nat(8)],
        vec![u64_arg(64), u64_arg(137)],
    );
    assert!(
        reason.contains("concrete function target"),
        "unexpected: {reason}"
    );
    // Unknown names stay unknown.
    let reason = invalid_reason(
        &admitted,
        by_name("missing_helper"),
        vec![nat(8)],
        Vec::new(),
    );
    assert_eq!(reason, "no compiled generic instantiation of mncs.compiler.source.v1::missing_helper matches this artifact: name the instantiation in the corpus so elaboration compiles it in");
}

#[test]
fn unseeded_artifact_refuses_generic_calls() {
    let (_, admitted) = seeded_artifact(&[]);
    assert!(
        admitted.artifact.generic_entrypoints.is_empty(),
        "no seeds means no entrypoints"
    );
    let reason = invalid_reason(
        &admitted,
        CallTarget::ByName {
            module: MODULE.to_owned(),
            name: "byte_at".to_owned(),
        },
        vec![nat(8)],
        vec![byte_seq(b"ABCDEFGH"), u64_arg(0)],
    );
    assert!(
        reason.contains("no compiled generic instantiation"),
        "unexpected: {reason}"
    );
}

#[test]
fn malformed_entrypoint_rows_are_refused_at_admission() {
    let (_, admitted) = seeded_artifact(&[seed("byte_at", 8)]);
    assert!(
        !admitted.artifact.generic_entrypoints.is_empty(),
        "seeded artifact carries rows"
    );
    // Duplicate row.
    let mut artifact = admitted.artifact.clone();
    let row = artifact.generic_entrypoints[0].clone();
    artifact.generic_entrypoints.push(row);
    let refused = mncs_vm::admit::admit_artifact(artifact.seal());
    assert!(
        matches!(refused, Err(AdmissionRefusal::Malformed { .. })),
        "duplicate row: {refused:?}"
    );
    // Row naming an unknown function.
    let mut artifact = admitted.artifact.clone();
    artifact.generic_entrypoints[0].function = "mncs:bogus:function".to_owned();
    let refused = mncs_vm::admit::admit_artifact(artifact.seal());
    assert!(
        matches!(refused, Err(AdmissionRefusal::UnresolvedFact { .. })),
        "unknown function: {refused:?}"
    );
    // Row without spellings.
    let mut artifact = admitted.artifact.clone();
    artifact.generic_entrypoints[0].args_spellings.clear();
    let refused = mncs_vm::admit::admit_artifact(artifact.seal());
    assert!(
        matches!(refused, Err(AdmissionRefusal::Malformed { .. })),
        "empty spellings: {refused:?}"
    );
    // Row without specialization identity.
    let mut artifact = admitted.artifact.clone();
    artifact.generic_entrypoints[0].canonical_args.clear();
    let refused = mncs_vm::admit::admit_artifact(artifact.seal());
    assert!(
        matches!(refused, Err(AdmissionRefusal::Malformed { .. })),
        "empty canonical args: {refused:?}"
    );
}
