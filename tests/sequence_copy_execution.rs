//! Differential SequenceCopy coverage against Language's independent corpus.
//!
//! The cases pin bounded-copy results and failure behavior for exact and
//! viewed sources, overlapping inputs, and scalar element representations.

use std::path::{Path, PathBuf};

use mncs_vm::capability::CapabilityEnv;
use mncs_vm::harness;
use mncs_vm::outcome::Outcome;
use mncs_vm::resource::{ResourceEnvelope, ResourceLimit};
use mncs_vm::session::{CallSpec, Session};
use mncs_vm::value::to_wire;
use serde::Deserialize;

#[path = "../build_support.rs"]
mod build_support;

#[derive(Debug, Deserialize)]
struct CopyCorpus {
    cases: Vec<CopyCase>,
}

#[derive(Debug, Deserialize)]
struct CopyCase {
    id: String,
    request: mncs_model::ExecutionRequest,
    expected_status: Option<String>,
    expected: Option<Vec<mncs_model::ExecutionValue>>,
}

fn language_example(relative: &str) -> PathBuf {
    let vm_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    build_support::path_dependency(vm_root, "mncs-model")
        .parent()
        .and_then(Path::parent)
        .expect("mncs-model must be in a crates directory")
        .join(relative)
}

fn oracle_status(status: &mncs_model::ExecutionStatus) -> &'static str {
    match status {
        mncs_model::ExecutionStatus::Returned => "returned",
        mncs_model::ExecutionStatus::RuntimeFailure => "runtime_failure",
        mncs_model::ExecutionStatus::Unsupported => "unsupported",
        mncs_model::ExecutionStatus::BudgetExhausted => "budget_exhausted",
        mncs_model::ExecutionStatus::InvalidRequest => "invalid_request",
    }
}

fn vm_status(expected: &str) -> &'static str {
    match expected {
        "returned" => "completed",
        "runtime_failure" => "program_failure",
        other => panic!("unexpected SequenceCopy corpus status {other:?}"),
    }
}

fn independent_status(case: &CopyCase) -> &str {
    if let Some(status) = case.expected_status.as_deref() {
        return status;
    }
    // The four negative cases in this corpus omit an explicit expected
    // outcome; their trap_* identities and source comments specify that
    // they exercise out-of-range or wrapping windows.
    match case.id.as_str() {
        "trap_oob_dst" | "trap_oob_src" | "trap_wrap_max" | "trap_wrap_edge" => "runtime_failure",
        other => panic!("missing independent status for SequenceCopy case {other:?}"),
    }
}

#[test]
fn language_sequence_copy_corpus_matches_independent_expectations() {
    let source_path = language_example("examples/source/pressure-span-copy.mncs");
    let source = std::fs::read_to_string(&source_path).expect("read SequenceCopy source");
    let (program, admitted) = harness::compile_direct(
        &source,
        source_path
            .to_str()
            .expect("UTF-8 SequenceCopy source path"),
    )
    .expect("compile SequenceCopy source through Language");

    let corpus_path = language_example("examples/execution/pressure-span-copy-corpus.json");
    let corpus: CopyCorpus = serde_json::from_slice(
        &std::fs::read(&corpus_path).expect("read independent SequenceCopy expectations"),
    )
    .expect("parse SequenceCopy corpus");
    assert_eq!(corpus.cases.len(), 17, "keep all copy cases active");

    let capabilities = CapabilityEnv::empty();
    for case in corpus.cases {
        let expected_status = independent_status(&case).to_owned();
        let independent = case.expected.clone().unwrap_or_default();
        let oracle = mncs_model::execute_ssa(&program, &case.request);
        assert_eq!(
            oracle_status(&oracle.status),
            expected_status,
            "{}: Language oracle status diverged from independent expectation",
            case.id,
        );
        assert_eq!(
            oracle.returned, independent,
            "{}: Language oracle value diverged from independent expectation",
            case.id,
        );

        let step_budget = case.request.step_budget;
        let spec = CallSpec::from_request(case.request, None)
            .unwrap_or_else(|error| panic!("{}: invalid VM request: {error}", case.id));
        let mut session = Session::open(&admitted);
        let (outcome, record) = session.call(&capabilities, spec);
        assert_eq!(
            outcome.tag(),
            vm_status(&expected_status),
            "{}: canonical VM status diverged: {outcome:?}",
            case.id,
        );
        if outcome == Outcome::Completed {
            let returned: Vec<_> = record.returned.iter().map(to_wire).collect();
            assert_eq!(returned, independent, "{}: VM value diverged", case.id);
        } else {
            assert!(
                independent.is_empty(),
                "{}: failure returned values",
                case.id
            );
        }
        assert!(
            record.usage.steps <= step_budget,
            "{}: exceeded request step budget",
            case.id
        );
    }
}

fn u64_value(value: u64) -> mncs_model::ExecutionValue {
    mncs_model::ExecutionValue::Integer {
        value: i128::from(value),
        ty: mncs_model::IntegerType {
            bits: 64,
            signed: false,
        },
    }
}

fn u64_sequence(values: &[u64]) -> mncs_model::ExecutionValue {
    mncs_model::ExecutionValue::Sequence {
        values: values
            .iter()
            .copied()
            .map(u64_value)
            .collect::<Vec<_>>()
            .into(),
    }
}

fn copy_call(
    admitted: &mncs_vm::admit::Admitted,
    memory_limit: u64,
) -> (Outcome, mncs_vm::evidence::ExecutionRecord) {
    let mut session = Session::open(admitted);
    session.call(
        &CapabilityEnv::empty(),
        CallSpec {
            target: mncs_vm::engine::CallTarget::ByName {
                module: "pressure.span_copy_memory".to_owned(),
                name: "copy_all".to_owned(),
            },
            arguments: vec![
                u64_sequence(&[1; 8]),
                u64_value(0),
                u64_sequence(&[2; 8]),
                u64_value(0),
                u64_value(8),
            ],
            type_arguments: Vec::new(),
            envelope: ResourceEnvelope {
                limits: vec![ResourceLimit {
                    dimension: "memory_cells".to_owned(),
                    limit: memory_limit,
                }],
            },
        },
    )
}

#[test]
fn copied_sequence_cells_are_metered_before_return() {
    let source = r#"
        mncs 0.14;
        module pressure.span_copy_memory;
        fn copy_all(
            dst: [u64; 8],
            dst_at: u64,
            src: [u64; 8],
            src_at: u64,
            len: u64
        ) -> (result: [u64; 8]) {
            return copy_span(dst, dst_at, src, src_at, len);
        }
    "#;
    let (_, admitted) = harness::compile_direct(source, "sequence-copy-memory.mncs")
        .expect("compile bounded copy memory workload");

    // Two eight-element sequences (9 cells apiece) and three scalar
    // arguments occupy 21 cells. SequenceCopy's new eight-element result
    // adds another 9, so 30 completes and 29 exhausts on the copied value.
    let (outcome, record) = copy_call(&admitted, 30);
    assert_eq!(outcome, Outcome::Completed);
    assert_eq!(record.usage.max_memory_cells, 30);

    let (outcome, record) = copy_call(&admitted, 29);
    assert!(
        matches!(outcome, Outcome::BudgetExhausted { ref dimension } if dimension == "memory_cells"),
        "{outcome:?}"
    );
    assert_eq!(record.usage.max_memory_cells, 30);
}
