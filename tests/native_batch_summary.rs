use std::sync::Arc;

use mncs_model::{ExecutionValue, IntegerType};
use mncs_vm::admit::{admit_owned, Admitted};
use mncs_vm::capability::CapabilityEnv;
use mncs_vm::engine::CallTarget;
use mncs_vm::harness;
use mncs_vm::outcome::Outcome;
use mncs_vm::resource::ResourceEnvelope;
use mncs_vm::session::{CallSpec, Session};
use mncs_vm::value::to_wire;

const SOURCE: &str = include_str!("../native/mncs/vm/batch_summary.mncs");
const ARTIFACT: &[u8] = include_bytes!("../native/mncs/vm/batch_summary.artifact.json");
const LOCATOR: &str = "native/mncs/vm/batch_summary.mncs";
const MODULE: &str = "mncs.vm.batch_summary.v1";

fn u64_arg(value: u64) -> ExecutionValue {
    ExecutionValue::Integer {
        value: i128::from(value),
        ty: IntegerType {
            bits: 64,
            signed: false,
        },
    }
}

fn byte_arg(value: u8) -> ExecutionValue {
    ExecutionValue::Byte {
        value: i128::from(value),
    }
}

fn sequence(values: Vec<ExecutionValue>) -> ExecutionValue {
    ExecutionValue::Sequence {
        values: Arc::new(values),
    }
}

fn u64_sequence(values: impl IntoIterator<Item = u64>) -> ExecutionValue {
    sequence(values.into_iter().map(u64_arg).collect())
}

fn summary_arguments(
    tags: &[&str],
    steps: &[u64],
    memory_cells: &[u64],
    effects: &[u64],
    call_depths: &[u64],
) -> Vec<ExecutionValue> {
    let mut arguments = vec![u64_arg(0); 17];
    arguments.push(ExecutionValue::Boolean { value: false });
    arguments.push(sequence(
        tags.iter()
            .map(|tag| sequence(tag.as_bytes().iter().copied().map(byte_arg).collect()))
            .collect(),
    ));
    arguments.push(u64_sequence(steps.iter().copied()));
    arguments.push(u64_sequence(memory_cells.iter().copied()));
    arguments.push(u64_sequence(effects.iter().copied()));
    arguments.push(u64_sequence(call_depths.iter().copied()));
    arguments
}

fn call_summary(admitted: &Admitted, arguments: Vec<ExecutionValue>) -> ExecutionValue {
    let mut session = Session::open(admitted);
    let (outcome, record) = session.call(
        &CapabilityEnv::empty(),
        CallSpec {
            target: CallTarget::ByName {
                module: MODULE.to_owned(),
                name: "summarize_chunk".to_owned(),
            },
            arguments,
            type_arguments: Vec::new(),
            envelope: ResourceEnvelope::default(),
        },
    );
    assert_eq!(
        outcome,
        Outcome::Completed,
        "native summary call: {outcome:?}"
    );
    assert_eq!(record.returned.len(), 1);
    to_wire(&record.returned[0])
}

fn summary_field<'a>(summary: &'a ExecutionValue, name: &str) -> &'a ExecutionValue {
    let ExecutionValue::Record { fields, .. } = summary else {
        panic!("expected BatchSummary record, got {summary:?}");
    };
    fields
        .iter()
        .find_map(|(field, value)| (field == name).then_some(value))
        .unwrap_or_else(|| panic!("missing BatchSummary field {name:?}"))
}

fn summary_number(summary: &ExecutionValue, name: &str) -> u64 {
    match summary_field(summary, name) {
        ExecutionValue::Integer { value, .. } => u64::try_from(*value).unwrap(),
        value => panic!("expected integer field {name:?}, got {value:?}"),
    }
}

fn summary_bool(summary: &ExecutionValue, name: &str) -> bool {
    match summary_field(summary, name) {
        ExecutionValue::Boolean { value } => *value,
        value => panic!("expected boolean field {name:?}, got {value:?}"),
    }
}

fn embedded_artifact() -> Admitted {
    admit_owned(ARTIFACT.to_vec()).expect("embedded batch-summary artifact admits")
}

#[test]
fn embedded_batch_summary_artifact_matches_its_native_source() {
    let compiled = harness::compile_source(SOURCE, LOCATOR)
        .expect("batch-summary MNCS source compiles and admits");
    let embedded = embedded_artifact();
    assert_eq!(compiled.artifact_id(), embedded.artifact_id());
}

#[test]
fn native_reducer_counts_outcomes_and_resource_facts_independently() {
    let tags = [
        "completed",
        "program_failure",
        "trap",
        "capability_denied",
        "unsupported",
        "invalid_request",
        "budget_exhausted",
        "provider_failure",
        "host_failure",
        "terminated",
        "future_outcome",
    ];
    let steps = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    let memory_cells = [2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22];
    let effects = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let call_depths = [1, 3, 2, 4, 1, 2, 3, 4, 5, 6, 7];

    let summary = call_summary(
        &embedded_artifact(),
        summary_arguments(&tags, &steps, &memory_cells, &effects, &call_depths),
    );

    assert_eq!(summary_number(&summary, "calls"), 11);
    assert_eq!(summary_number(&summary, "completed"), 1);
    assert_eq!(summary_number(&summary, "program_failure"), 1);
    assert_eq!(summary_number(&summary, "trap"), 1);
    assert_eq!(summary_number(&summary, "capability_denied"), 1);
    assert_eq!(summary_number(&summary, "unsupported"), 1);
    assert_eq!(summary_number(&summary, "invalid_request"), 1);
    assert_eq!(summary_number(&summary, "budget_exhausted"), 1);
    assert_eq!(summary_number(&summary, "provider_failure"), 1);
    assert_eq!(summary_number(&summary, "host_failure"), 1);
    assert_eq!(summary_number(&summary, "terminated"), 1);
    assert_eq!(summary_number(&summary, "unrecognized_outcomes"), 1);
    assert_eq!(summary_number(&summary, "steps_total"), 66);
    assert_eq!(summary_number(&summary, "steps_max"), 11);
    assert_eq!(summary_number(&summary, "memory_cells_max"), 22);
    assert_eq!(summary_number(&summary, "effects_total"), 55);
    assert_eq!(summary_number(&summary, "call_depth_max"), 7);
    assert!(summary_bool(&summary, "valid"));
    assert!(!summary_bool(&summary, "overflowed"));
}

#[test]
fn native_reducer_rejects_parallel_sequences_with_different_lengths() {
    let summary = call_summary(
        &embedded_artifact(),
        summary_arguments(&["completed"], &[], &[1], &[0], &[1]),
    );
    assert!(!summary_bool(&summary, "valid"));
}

#[test]
fn native_reducer_marks_counter_and_metric_overflow() {
    let mut arguments = summary_arguments(&["completed"], &[1], &[0], &[0], &[0]);
    arguments[0] = u64_arg(u64::MAX);
    arguments[1] = u64_arg(u64::MAX);
    arguments[11] = u64_arg(u64::MAX);
    let summary = call_summary(&embedded_artifact(), arguments);
    assert_eq!(summary_number(&summary, "calls"), 0);
    assert_eq!(summary_number(&summary, "completed"), 0);
    assert_eq!(summary_number(&summary, "steps_total"), 0);
    assert!(summary_bool(&summary, "overflowed"));
}
