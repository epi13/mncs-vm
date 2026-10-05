//! Differential tests against the read-only research interpreter.
//!
//! The same compilation feeds both engines: `execute_ssa` (upstream
//! oracle, unmodified) and the VM reference engine. Agreement on
//! returned values and on the status mapping is bounded evidence the
//! VM preserves source semantics; it is not a proof, and the corpus
//! stays inside the v1 executable subset on purpose.

use mncs_vm::capability::CapabilityEnv;
use mncs_vm::engine::CallTarget;
use mncs_vm::harness;
use mncs_vm::outcome::Outcome;
use mncs_vm::resource::{ResourceEnvelope, ResourceLimit};
use mncs_vm::session::{CallSpec, Session};
use mncs_vm::value::to_wire;

fn i64_arg(value: i64) -> mncs_model::ExecutionValue {
    mncs_model::ExecutionValue::Integer {
        value: value as i128,
        ty: mncs_model::IntegerType {
            bits: 64,
            signed: true,
        },
    }
}

fn seq_arg(values: &[i64]) -> mncs_model::ExecutionValue {
    mncs_model::ExecutionValue::Sequence {
        values: values
            .iter()
            .map(|value| i64_arg(*value))
            .collect::<Vec<_>>()
            .into(),
    }
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

fn oracle_status(status: &mncs_model::ExecutionStatus) -> &'static str {
    match status {
        mncs_model::ExecutionStatus::Returned => "completed",
        mncs_model::ExecutionStatus::RuntimeFailure => "program_failure",
        mncs_model::ExecutionStatus::Unsupported => "unsupported",
        mncs_model::ExecutionStatus::BudgetExhausted => "budget_exhausted",
        mncs_model::ExecutionStatus::InvalidRequest => "invalid_request",
    }
}

fn compare(corpus: &str, module: &str, function: &str, args: Vec<mncs_model::ExecutionValue>) {
    let source = std::fs::read_to_string(format!("tests/corpus/{corpus}")).unwrap();
    let (program, admitted) = harness::compile_direct(&source, corpus).unwrap();

    let request = mncs_model::ExecutionRequest {
        schema_version: "0.1".to_owned(),
        target: mncs_model::ExecutionTarget {
            module: module.to_owned(),
            function: function.to_owned(),
        },
        arguments: args.clone(),
        type_arguments: Vec::new(),
        step_budget: 50_000,
        policy: mncs_model::ExecutionPolicy::default(),
        host_grants: Vec::new(),
        call_depth_budget: None,
    };
    let oracle = mncs_model::execute_ssa(&program, &request);

    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let (outcome, record) = session.call(
        &caps,
        CallSpec {
            target: CallTarget::ByName {
                module: module.to_owned(),
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
        "{corpus}::{function}: vm={outcome:?} oracle={:?}",
        oracle.status
    );
    if outcome == Outcome::Completed {
        let vm_values: Vec<mncs_model::ExecutionValue> =
            record.returned.iter().map(to_wire).collect();
        assert_eq!(
            vm_values, oracle.returned,
            "{corpus}::{function}: value divergence"
        );
    }
}

#[test]
fn differential_arith() {
    let arith = "mncs.vmcorpus.arith.v1";
    compare("arith.mncs", arith, "add2", vec![i64_arg(11), i64_arg(22)]);
    compare(
        "arith.mncs",
        arith,
        "muladd",
        vec![i64_arg(3), i64_arg(5), i64_arg(7)],
    );
    compare("arith.mncs", arith, "absval", vec![i64_arg(-13)]);
    compare("arith.mncs", arith, "max2", vec![i64_arg(8), i64_arg(8)]);
    compare("arith.mncs", arith, "add3", vec![i64_arg(100)]);
    compare(
        "arith.mncs",
        arith,
        "sum8",
        vec![seq_arg(&[5, 4, 3, 2, 1, 0, -1, -2])],
    );
}

#[test]
fn differential_shapes() {
    let shapes = "mncs.vmcorpus.shapes.v1";
    compare(
        "shapes.mncs",
        shapes,
        "mkpair",
        vec![i64_arg(9), i64_arg(-3)],
    );
    compare(
        "shapes.mncs",
        shapes,
        "sum_pair",
        vec![i64_arg(15), i64_arg(27)],
    );
    compare(
        "shapes.mncs",
        shapes,
        "just_or_default",
        vec![i64_arg(1), i64_arg(41), i64_arg(1)],
    );
    compare(
        "shapes.mncs",
        shapes,
        "just_or_default",
        vec![i64_arg(0), i64_arg(41), i64_arg(1)],
    );
    compare(
        "shapes.mncs",
        shapes,
        "is_just_of",
        vec![i64_arg(1), i64_arg(0)],
    );
}

#[test]
fn differential_failure_mapping() {
    let fail = "mncs.vmcorpus.neg.v1";
    compare(
        "fail.mncs",
        fail,
        "div_or_fail",
        vec![i64_arg(9), i64_arg(3)],
    );
    compare(
        "fail.mncs",
        fail,
        "div_or_fail",
        vec![i64_arg(1), i64_arg(0)],
    );
    compare(
        "fail.mncs",
        fail,
        "overflow_add",
        vec![i64_arg(i64::MAX), i64_arg(1)],
    );
}

#[test]
fn differential_invalid_requests() {
    let arith = "mncs.vmcorpus.arith.v1";
    compare("arith.mncs", arith, "add2", vec![i64_arg(1)]);
}
