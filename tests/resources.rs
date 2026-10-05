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

fn request(budget: u64, depth: Option<u64>) -> mncs_model::ExecutionRequest {
    mncs_model::ExecutionRequest {
        schema_version: "0.1".into(),
        target: mncs_model::ExecutionTarget {
            module: "mncs.vmcorpus.arith.v1".into(),
            function: "add3".into(),
        },
        arguments: vec![i64_arg(4)],
        type_arguments: vec![],
        step_budget: budget,
        policy: Default::default(),
        host_grants: vec![],
        call_depth_budget: depth,
    }
}
#[test]
fn canonical_request_propagates_and_overrides_only_tighten() {
    let spec = CallSpec::from_request(request(8_000_000, Some(7)), None).unwrap();
    assert_eq!(spec.envelope.limit_of("steps"), Some(8_000_000));
    assert_eq!(spec.envelope.limit_of("call_depth"), Some(7));
    for dimension in mncs_vm::admit::ENFORCED_BOUNDS {
        assert!(spec.envelope.limit_of(dimension).is_some());
    }
    let extra = ResourceEnvelope {
        limits: vec![
            ResourceLimit {
                dimension: "steps".into(),
                limit: 9_000_000,
            },
            ResourceLimit {
                dimension: "call_depth".into(),
                limit: 1,
            },
        ],
    };
    let spec = CallSpec::from_request(request(8_000_000, Some(7)), Some(&extra)).unwrap();
    assert_eq!(spec.envelope.limit_of("steps"), Some(8_000_000));
    assert_eq!(spec.envelope.limit_of("call_depth"), Some(1));
    let duplicate = ResourceEnvelope {
        limits: vec![
            ResourceLimit {
                dimension: "steps".into(),
                limit: 2
            };
            2
        ],
    };
    assert!(CallSpec::from_request(request(3, None), Some(&duplicate)).is_err());
    let unknown = ResourceEnvelope {
        limits: vec![ResourceLimit {
            dimension: "unknown".into(),
            limit: 2,
        }],
    };
    assert!(CallSpec::from_request(request(3, None), Some(&unknown)).is_err());
    assert!(serde_json::from_value::<mncs_model::ExecutionRequest>(serde_json::json!({"schema_version":"0.1","target":{"module":"m","function":"f"},"arguments":[]})).is_err());
}
#[test]
fn canonical_request_nested_exhaustion_is_repeatable_and_isolated() {
    let admitted = compile_corpus("arith.mncs");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    for (fuel, depth, dimension) in [
        (0, None, "steps"),
        (2, None, "steps"),
        (100, Some(1), "call_depth"),
    ] {
        let (first, record) = session.call(
            &caps,
            CallSpec::from_request(request(fuel, depth), None).unwrap(),
        );
        let (again, second) = session.call(
            &caps,
            CallSpec::from_request(request(fuel, depth), None).unwrap(),
        );
        assert_eq!(
            first,
            Outcome::BudgetExhausted {
                dimension: dimension.into()
            }
        );
        assert_eq!(again, first);
        assert_eq!(record.usage, second.usage);
        if dimension == "steps" {
            assert_eq!(record.usage.steps, fuel + 1);
        }
    }
    let (outcome, record) = session.call(
        &caps,
        CallSpec::from_request(request(100, None), None).unwrap(),
    );
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(record.usage.max_call_depth, 2);
    assert_eq!(
        record
            .returned
            .iter()
            .map(mncs_vm::value::to_wire)
            .collect::<Vec<_>>(),
        vec![i64_arg(7)]
    );
}
#[test]
fn declared_bounds_still_tighten_request_and_are_recorded() {
    let mut artifact = compile_corpus("arith.mncs").artifact;
    artifact
        .requirements
        .bounds
        .push(mncs_vm::artifact::BoundDecl {
            dimension: "steps".into(),
            limit: 1,
        });
    let admitted = mncs_vm::admit::admit_artifact(artifact.seal()).unwrap();
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let (outcome, record) = session.call(
        &caps,
        CallSpec::from_request(request(8_000_000, None), None).unwrap(),
    );
    assert_eq!(
        outcome,
        Outcome::BudgetExhausted {
            dimension: "steps".into()
        }
    );
    assert!(record
        .resource_limits
        .iter()
        .any(|l| l.dimension == "steps" && l.limit == 1));
}
