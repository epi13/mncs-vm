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
    let (program, admitted) =
        mncs_vm::harness::compile_direct(&source, "views.mncs").expect("corpus compiles");
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
    for (function, expected) in [
        ("attempt_one", 1),
        ("attempt_plain", 8),
        ("attempt_sum", 8),
        ("attempt_branch", 8),
        ("nested_sum", 32),
        ("nested_inline", 32),
    ] {
        let (outcome, values) = compare(&program, &admitted, function, Vec::new());
        assert_eq!(outcome, Outcome::Completed, "{function}");
        assert_eq!(values, vec![u64_arg(expected)], "{function}");
    }
    let _ = compile_corpus("views.mncs");
}

fn run_vm_with_iterations(
    admitted: &mncs_vm::admit::Admitted,
    function: &str,
    iterations: u64,
) -> Outcome {
    let mut env = envelope();
    env.limits.push(ResourceLimit {
        dimension: "iterations".to_owned(),
        limit: iterations,
    });
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(admitted);
    let (outcome, _) = session.call(
        &caps,
        CallSpec {
            target: CallTarget::ByName {
                module: MODULE.to_owned(),
                name: function.to_owned(),
            },
            arguments: Vec::new(),
            type_arguments: Vec::new(),
            envelope: env,
        },
    );
    outcome
}

/// Iteration accounting charges one unit per executed loop pass:
/// multi-block bodies (calls, branches) and nested loops must not
/// inflate the count, and every pass must still be charged. Each
/// case pins the exact live-count peak: the stated budget completes
/// while one less exhausts on the `iterations` dimension.
#[test]
fn iteration_accounting_counts_passes_not_block_visits() {
    let (_, admitted) = program_and_admitted();
    // nested_inline peaks at 4 (outer) + 8 (inner, reset per outer
    // pass) live in one frame; nested_sum meters per frame, so its
    // peak is the callee inner loop's 8.
    for (function, limit) in [
        ("attempt_plain", 8),
        ("attempt_sum", 8),
        ("attempt_branch", 8),
        ("nested_sum", 8),
        ("nested_inline", 12),
    ] {
        assert_eq!(
            run_vm_with_iterations(&admitted, function, limit),
            Outcome::Completed,
            "{function}: exact pass budget must complete",
        );
        assert!(
            matches!(
                run_vm_with_iterations(&admitted, function, limit - 1),
                Outcome::BudgetExhausted { dimension } if dimension == "iterations"
            ),
            "{function}: budget one below the pass count must exhaust",
        );
    }
}

fn run_vm_with_memory(
    admitted: &mncs_vm::admit::Admitted,
    function: &str,
    args: Vec<mncs_model::ExecutionValue>,
    memory_cells: u64,
) -> Outcome {
    let mut env = envelope();
    env.limits.retain(|limit| limit.dimension != "memory_cells");
    env.limits.push(ResourceLimit {
        dimension: "memory_cells".to_owned(),
        limit: memory_cells,
    });
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(admitted);
    let (outcome, _) = session.call(
        &caps,
        CallSpec {
            target: CallTarget::ByName {
                module: MODULE.to_owned(),
                name: function.to_owned(),
            },
            arguments: args,
            type_arguments: Vec::new(),
            envelope: env,
        },
    );
    outcome
}

/// Memory accounting tracks live held cells, not allocation
/// history: rebinding a carried 1025-cell value over 100 passes
/// peaks near 3K live, while cumulative history would exceed 100K.
#[test]
fn memory_accounting_tracks_live_values_not_history() {
    let (program, admitted) = program_and_admitted();
    let blob = byte_seq(&vec![7u8; 1024]);
    let (outcome, values) = compare(&program, &admitted, "carry_blob", vec![blob.clone()]);
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(values, vec![u64_arg(100)]);
    assert_eq!(
        run_vm_with_memory(&admitted, "carry_blob", vec![blob], 10_000),
        Outcome::Completed,
        "live peak must fit a 10K cell budget",
    );
    // Charging still happens: the 1025-cell argument alone
    // exhausts a 100-cell budget at entry.
    assert!(
        matches!(
            run_vm_with_memory(
                &admitted,
                "carry_blob",
                vec![byte_seq(&vec![7u8; 1024])],
                100
            ),
            Outcome::BudgetExhausted { dimension } if dimension == "memory_cells"
        ),
        "tiny budget must still exhaust at entry",
    );
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
