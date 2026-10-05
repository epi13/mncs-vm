//! `ViewConstruct` differential: checked slices through the VM.
//!
//! Positive spans agree with the oracle on values; malformed spans
//! agree on failure classification (invalid request vs program
//! failure). The segment suite exercises views at scale; this file
//! pins the branch behavior directly.

use mncs_vm::capability::CapabilityEnv;
use mncs_vm::engine::CallTarget;
use mncs_vm::harness::tests_only::compile_corpus;
use mncs_vm::outcome::Outcome;
use mncs_vm::resource::{ResourceEnvelope, ResourceLimit};
use mncs_vm::session::{CallSpec, Session};
use mncs_vm::value::to_wire;

const MODULE: &str = "mncs.vmcorpus.views.v1";

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

fn oracle_status(status: &mncs_model::ExecutionStatus) -> &'static str {
    match status {
        mncs_model::ExecutionStatus::Returned => "completed",
        mncs_model::ExecutionStatus::RuntimeFailure => "program_failure",
        mncs_model::ExecutionStatus::Unsupported => "unsupported",
        mncs_model::ExecutionStatus::BudgetExhausted => "budget_exhausted",
        mncs_model::ExecutionStatus::InvalidRequest => "invalid_request",
    }
}

fn compare(
    program: &mncs_model::Program,
    admitted: &mncs_vm::admit::Admitted,
    function: &str,
    args: Vec<mncs_model::ExecutionValue>,
) -> (Outcome, Vec<mncs_model::ExecutionValue>) {
    let request = mncs_model::ExecutionRequest {
        schema_version: "0.1".to_owned(),
        target: mncs_model::ExecutionTarget {
            module: MODULE.to_owned(),
            function: function.to_owned(),
        },
        arguments: args.clone(),
        type_arguments: Vec::new(),
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
            type_arguments: Vec::new(),
            envelope: envelope(),
        },
    );
    assert_eq!(
        outcome.tag(),
        oracle_status(&oracle.status),
        "{function}: vm={outcome:?} oracle={:?}",
        oracle.status
    );
    let vm_values: Vec<mncs_model::ExecutionValue> = record.returned.iter().map(to_wire).collect();
    if outcome == Outcome::Completed {
        assert_eq!(vm_values, oracle.returned, "{function}: value divergence");
    }
    (outcome, vm_values)
}

fn program_and_admitted() -> (mncs_model::Program, mncs_vm::admit::Admitted) {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push("corpus");
    path.push("views.mncs");
    let source = std::fs::read_to_string(&path).unwrap();
    let (program, backend) =
        mncs_vm::harness::compile_to_backend(&source, "views.mncs").expect("corpus compiles");
    let admitted = mncs_vm::migrate::admit_research_artifact(&backend).expect("admit");
    (program, admitted)
}

#[test]
fn valid_spans_agree_with_oracle() {
    let (program, admitted) = program_and_admitted();
    let text = byte_seq(b"ABCDEFGH");
    let (outcome, values) = compare(
        &program,
        &admitted,
        "span_len",
        vec![text.clone(), u64_arg(2), u64_arg(6)],
    );
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(values, vec![u64_arg(4)]);
    let (outcome, values) = compare(
        &program,
        &admitted,
        "span_len",
        vec![text.clone(), u64_arg(0), u64_arg(8)],
    );
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(values, vec![u64_arg(8)]);
    let (outcome, values) = compare(
        &program,
        &admitted,
        "span_len",
        vec![text.clone(), u64_arg(5), u64_arg(5)],
    );
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(values, vec![u64_arg(0)]);
    let _ = compile_corpus("views.mncs");
}

#[test]
fn malformed_spans_fail_like_oracle() {
    let (program, admitted) = program_and_admitted();
    let text = byte_seq(b"ABCDEFGH");
    // End past the source length: program failure both sides.
    let (outcome, _) = compare(
        &program,
        &admitted,
        "span_len",
        vec![text.clone(), u64_arg(0), u64_arg(9)],
    );
    assert!(
        matches!(outcome, Outcome::ProgramFailure { .. }),
        "{outcome:?}"
    );
    // Inverted range: program failure both sides.
    let (outcome, _) = compare(
        &program,
        &admitted,
        "span_len",
        vec![text, u64_arg(6), u64_arg(2)],
    );
    assert!(
        matches!(outcome, Outcome::ProgramFailure { .. }),
        "{outcome:?}"
    );
}
