//! Differential binary64 coverage against the Language Stage C1 contract.
//!
//! The checked-in Language corpus carries independent expected values from
//! its Python IEEE-754 oracle. This test asks the reference executor and the
//! canonical VM to satisfy those expectations without changing the corpus.

use std::path::{Path, PathBuf};

use mncs_vm::capability::CapabilityEnv;
use mncs_vm::harness;
use mncs_vm::outcome::Outcome;
use mncs_vm::session::{CallSpec, Session};
use mncs_vm::value::to_wire;
use serde::Deserialize;

#[path = "../build_support.rs"]
mod build_support;

#[derive(Debug, Deserialize)]
struct FloatCorpus {
    cases: Vec<FloatCase>,
}

#[derive(Debug, Deserialize)]
struct FloatCase {
    id: String,
    request: mncs_model::ExecutionRequest,
    expected_status: String,
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
        other => panic!("unexpected Stage C1 status {other:?}"),
    }
}

#[test]
fn stage_c1_binary64_corpus_matches_independent_vectors_and_language_oracle() {
    let source_path = language_example("examples/source/stage-c1-float-arithmetic.mncs");
    let source = std::fs::read_to_string(&source_path).expect("read Stage C1 MNCS source");
    let (program, admitted) = harness::compile_direct(
        &source,
        source_path.to_str().expect("UTF-8 Stage C1 source path"),
    )
    .expect("compile Stage C1 source through the Language compiler");

    let corpus_path = language_example("examples/execution/stage-c1-float-corpus.json");
    let corpus: FloatCorpus = serde_json::from_slice(
        &std::fs::read(&corpus_path).expect("read independent Stage C1 vectors"),
    )
    .expect("parse Stage C1 corpus");
    assert_eq!(
        corpus.cases.len(),
        69,
        "keep the full independent corpus active"
    );

    let capabilities = CapabilityEnv::empty();
    for case in corpus.cases {
        let independent = case.expected.clone().unwrap_or_default();
        let oracle = mncs_model::execute_ssa(&program, &case.request);
        assert_eq!(
            oracle_status(&oracle.status),
            case.expected_status,
            "{}: Language oracle status diverged from independent vector",
            case.id,
        );
        assert_eq!(
            oracle.returned, independent,
            "{}: Language oracle value diverged from independent vector",
            case.id,
        );

        let step_budget = case.request.step_budget;
        let spec = CallSpec::from_request(case.request, None)
            .unwrap_or_else(|error| panic!("{}: invalid VM call request: {error}", case.id));
        let mut session = Session::open(&admitted);
        let (outcome, record) = session.call(&capabilities, spec);
        assert_eq!(
            outcome.tag(),
            vm_status(&case.expected_status),
            "{}: canonical VM status diverged: {outcome:?}",
            case.id,
        );
        if outcome == Outcome::Completed {
            let returned: Vec<_> = record.returned.iter().map(to_wire).collect();
            assert_eq!(returned, independent, "{}: VM value diverged", case.id);
        } else {
            assert!(
                independent.is_empty(),
                "{}: failure must not return values",
                case.id
            );
        }
        assert!(
            record.usage.steps <= step_budget,
            "{}: exceeded request step budget",
            case.id
        );
        assert!(
            record.usage.max_memory_cells <= 64,
            "{}: scalar workload grew unexpectedly",
            case.id
        );
    }
}
