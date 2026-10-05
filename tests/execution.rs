//! Session execution over the corpus: exact values, structured
//! outcomes, determinism, and evidence integrity.

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

fn envelope() -> ResourceEnvelope {
    ResourceEnvelope {
        limits: vec![
            ResourceLimit {
                dimension: "steps".to_owned(),
                limit: 10_000,
            },
            ResourceLimit {
                dimension: "call_depth".to_owned(),
                limit: 64,
            },
            ResourceLimit {
                dimension: "memory_cells".to_owned(),
                limit: 100_000,
            },
            ResourceLimit {
                dimension: "effects".to_owned(),
                limit: 64,
            },
        ],
    }
}

fn run(
    corpus: &str,
    module: &str,
    function: &str,
    args: Vec<mncs_model::ExecutionValue>,
) -> (Outcome, mncs_vm::evidence::ExecutionRecord) {
    let admitted = compile_corpus(corpus);
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    session.call(
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
    )
}

fn int_of(record: &mncs_vm::evidence::ExecutionRecord) -> i128 {
    assert_eq!(record.returned.len(), 1);
    match &record.returned[0] {
        mncs_vm::value::Value::Integer { value, .. } => *value,
        other => panic!("expected integer, got {other:?}"),
    }
}

#[test]
fn arith_exact_values() {
    let arith = "mncs.vmcorpus.arith.v1";
    let (outcome, record) = run("arith.mncs", arith, "add2", vec![i64_arg(3), i64_arg(4)]);
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(int_of(&record), 7);

    let (outcome, record) = run(
        "arith.mncs",
        arith,
        "muladd",
        vec![i64_arg(2), i64_arg(3), i64_arg(4)],
    );
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(int_of(&record), 10);

    let (outcome, record) = run("arith.mncs", arith, "absval", vec![i64_arg(-5)]);
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(int_of(&record), 5);

    let (outcome, record) = run("arith.mncs", arith, "max2", vec![i64_arg(3), i64_arg(9)]);
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(int_of(&record), 9);

    let (outcome, record) = run("arith.mncs", arith, "add3", vec![i64_arg(10)]);
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(int_of(&record), 13);

    let (outcome, record) = run(
        "arith.mncs",
        arith,
        "sum8",
        vec![seq_arg(&[1, 2, 3, 4, 5, 6, 7, 8])],
    );
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(int_of(&record), 36);
}

#[test]
fn shapes_exact_values() {
    let shapes = "mncs.vmcorpus.shapes.v1";
    let (outcome, record) = run(
        "shapes.mncs",
        shapes,
        "sum_pair",
        vec![i64_arg(20), i64_arg(22)],
    );
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(int_of(&record), 42);

    let (outcome, record) = run(
        "shapes.mncs",
        shapes,
        "just_or_default",
        vec![i64_arg(0), i64_arg(99), i64_arg(7)],
    );
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(int_of(&record), 7);

    let (outcome, record) = run(
        "shapes.mncs",
        shapes,
        "just_or_default",
        vec![i64_arg(1), i64_arg(99), i64_arg(7)],
    );
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(int_of(&record), 106);

    let (outcome, record) = run(
        "shapes.mncs",
        shapes,
        "mkpair",
        vec![i64_arg(3), i64_arg(4)],
    );
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(record.returned.len(), 1);
    match &record.returned[0] {
        mncs_vm::value::Value::Record { name, fields, .. } => {
            assert_eq!(name, "Pair");
            let names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
            assert_eq!(names, vec!["x", "y"]);
        }
        other => panic!("expected record, got {other:?}"),
    }
}

#[test]
fn failure_paths_are_structured() {
    let fail = "mncs.vmcorpus.neg.v1";
    let (outcome, record) = run(
        "fail.mncs",
        fail,
        "div_or_fail",
        vec![i64_arg(7), i64_arg(2)],
    );
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(int_of(&record), 3);

    let (outcome, _) = run(
        "fail.mncs",
        fail,
        "div_or_fail",
        vec![i64_arg(1), i64_arg(0)],
    );
    assert!(
        matches!(outcome, Outcome::ProgramFailure { .. }),
        "{outcome:?}"
    );

    let (outcome, _) = run(
        "fail.mncs",
        fail,
        "overflow_add",
        vec![i64_arg(i64::MAX), i64_arg(1)],
    );
    assert!(
        matches!(outcome, Outcome::ProgramFailure { .. }),
        "{outcome:?}"
    );
}

#[test]
fn invalid_requests_fail_closed() {
    let arith = "mncs.vmcorpus.arith.v1";
    let (outcome, _) = run("arith.mncs", arith, "no_such_fn", vec![]);
    assert!(matches!(outcome, Outcome::InvalidRequest { .. }));

    let (outcome, _) = run("arith.mncs", arith, "add2", vec![i64_arg(1)]);
    assert!(matches!(outcome, Outcome::InvalidRequest { .. }));
}

#[test]
fn execution_is_deterministic() {
    let arith = "mncs.vmcorpus.arith.v1";
    let first = run(
        "arith.mncs",
        arith,
        "muladd",
        vec![i64_arg(6), i64_arg(7), i64_arg(8)],
    );
    let second = run(
        "arith.mncs",
        arith,
        "muladd",
        vec![i64_arg(6), i64_arg(7), i64_arg(8)],
    );
    assert_eq!(first.0, second.0);
    assert_eq!(first.1.return_digest, second.1.return_digest);
    assert_eq!(first.1.effects_digest, second.1.effects_digest);
    assert_eq!(int_of(&first.1), 50);
}

#[test]
fn evidence_is_complete() {
    let arith = "mncs.vmcorpus.arith.v1";
    let (outcome, record) = run("arith.mncs", arith, "add2", vec![i64_arg(3), i64_arg(4)]);
    assert_eq!(outcome, Outcome::Completed);
    assert!(!record.artifact_id.is_empty());
    assert!(record.artifact_id.starts_with("sha256:"));
    assert_eq!(
        record.return_digest,
        mncs_vm::evidence::ExecutionRecord::digest_of(
            &record
                .returned
                .iter()
                .map(mncs_vm::value::to_wire)
                .collect::<Vec<_>>()
        )
    );
    assert!(record.usage.steps > 0);
    assert_eq!(record.effects.len(), 0);
}
