//! Resource enforcement: every declared dimension exhausts with a
//! structured outcome naming the dimension. A budget is semantic
//! accounting, never a wall-clock timeout.

use mncs_vm::capability::CapabilityEnv;
use mncs_vm::engine::CallTarget;
use mncs_vm::harness::tests_only::compile_corpus;
use mncs_vm::outcome::Outcome;
use mncs_vm::resource::{ResourceEnvelope, ResourceLimit};
use mncs_vm::session::{CallSpec, Session};

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

fn run_with(
    function: &str,
    args: Vec<mncs_model::ExecutionValue>,
    limits: Vec<ResourceLimit>,
) -> Outcome {
    let admitted = compile_corpus("arith.mncs");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let (outcome, _) = session.call(
        &caps,
        CallSpec {
            target: CallTarget::ByName {
                module: "mncs.vmcorpus.arith.v1".to_owned(),
                name: function.to_owned(),
            },
            arguments: args,
            type_arguments: Vec::new(),
            envelope: ResourceEnvelope { limits },
        },
    );
    outcome
}

fn generous() -> Vec<ResourceLimit> {
    vec![
        ResourceLimit {
            dimension: "steps".to_owned(),
            limit: 100_000,
        },
        ResourceLimit {
            dimension: "call_depth".to_owned(),
            limit: 128,
        },
        ResourceLimit {
            dimension: "memory_cells".to_owned(),
            limit: 1_000_000,
        },
        ResourceLimit {
            dimension: "effects".to_owned(),
            limit: 128,
        },
    ]
}

#[test]
fn tiny_step_budget_exhausts() {
    let outcome = run_with(
        "add3",
        vec![i64_arg(1)],
        vec![ResourceLimit {
            dimension: "steps".to_owned(),
            limit: 2,
        }],
    );
    assert!(
        matches!(outcome, Outcome::BudgetExhausted { ref dimension } if dimension == "steps"),
        "{outcome:?}"
    );
}

#[test]
fn call_depth_bound_exhausts() {
    let outcome = run_with(
        "add3",
        vec![i64_arg(1)],
        vec![ResourceLimit {
            dimension: "call_depth".to_owned(),
            limit: 1,
        }],
    );
    assert!(
        matches!(outcome, Outcome::BudgetExhausted { ref dimension } if dimension == "call_depth"),
        "{outcome:?}"
    );
}

#[test]
fn generous_envelope_completes() {
    let outcome = run_with("add3", vec![i64_arg(1)], generous());
    assert_eq!(outcome, Outcome::Completed);
}

#[test]
fn memory_budget_exhausts_on_sequences() {
    let outcome = run_with(
        "sum8",
        vec![seq_arg(&[1, 2, 3, 4, 5, 6, 7, 8])],
        vec![ResourceLimit {
            dimension: "memory_cells".to_owned(),
            limit: 1,
        }],
    );
    assert!(
        matches!(outcome, Outcome::BudgetExhausted { ref dimension } if dimension == "memory_cells"),
        "{outcome:?}"
    );
}

#[test]
fn iteration_envelope_caps_loops() {
    let admitted = compile_corpus("arith.mncs");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let (outcome, _) = session.call(
        &caps,
        CallSpec {
            target: CallTarget::ByName {
                module: "mncs.vmcorpus.arith.v1".to_owned(),
                name: "sum8".to_owned(),
            },
            arguments: vec![seq_arg(&[1, 2, 3, 4, 5, 6, 7, 8])],
            type_arguments: Vec::new(),
            envelope: ResourceEnvelope {
                limits: vec![
                    ResourceLimit {
                        dimension: "steps".to_owned(),
                        limit: 100_000,
                    },
                    ResourceLimit {
                        dimension: "iterations".to_owned(),
                        limit: 2,
                    },
                ],
            },
        },
    );
    assert!(
        matches!(outcome, Outcome::BudgetExhausted { ref dimension } if dimension == "iterations"),
        "{outcome:?}"
    );
}
