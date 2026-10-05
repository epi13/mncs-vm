//! Differential execution of real new-compiler source through the VM.
//!
//! The compiled program is `mncs-compiler/src/compiler/source.mncs`
//! (the compiler's own source layer, read from the sibling checkout
//! the same way the path dependencies are resolved): no fixtures,
//! no copies, no drift. The same compilation feeds the upstream
//! `execute_ssa` oracle and the VM engine; agreement plus concrete
//! expected values is the Phase-3 consumer proof.
//!
//! Only non-generic callables are exercised here; generic helpers
//! (`byte_at<N>`, `ascii<N>`) are covered by `tests/generics.rs`
//! through the type-argument call boundary.

use mncs_vm::capability::CapabilityEnv;
use mncs_vm::engine::CallTarget;
use mncs_vm::harness;
use mncs_vm::outcome::Outcome;
use mncs_vm::resource::{ResourceEnvelope, ResourceLimit};
use mncs_vm::session::{CallSpec, Session};
use mncs_vm::value::to_wire;

const MODULE: &str = "mncs.compiler.source.v1";

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

/// Execute one callable through the VM and the oracle; assert status
/// agreement, value agreement, and the concrete expected values.
fn compare(
    program: &mncs_model::Program,
    admitted: &mncs_vm::admit::Admitted,
    function: &str,
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
        "source::{function}: vm={outcome:?} oracle={:?}",
        oracle.status
    );
    assert_eq!(outcome, Outcome::Completed, "source::{function}: {outcome:?}");
    let vm_values: Vec<mncs_model::ExecutionValue> =
        record.returned.iter().map(to_wire).collect();
    assert_eq!(
        vm_values, oracle.returned,
        "source::{function}: value divergence"
    );
    assert_eq!(
        vm_values, expected,
        "source::{function}: concrete expectation failed"
    );
}

fn bool_value(value: bool) -> mncs_model::ExecutionValue {
    mncs_model::ExecutionValue::Boolean { value }
}

#[test]
fn compiler_source_layer_executes_through_vm() {
    let path = compiler_source_path();
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("sibling compiler source missing: {}", path.display()));
    assert!(
        source.contains("module mncs.compiler.source.v1;"),
        "unexpected source at {}",
        path.display()
    );
    let (program, admitted) =
        harness::compile_direct(&source, "mncs-compiler/src/compiler/source.mncs").unwrap();
    // Generic exports without a seeded instance are recorded, not
    // silently dropped and not fatal to the artifact.
    assert!(
        admitted
            .artifact
            .unsupported
            .iter()
            .any(|entry| entry.ends_with(": no ssa instance")),
        "uninstantiated exports recorded: {:?}",
        admitted.artifact.unsupported
    );
    assert!(
        admitted
            .artifact
            .callables
            .iter()
            .any(|entry| entry.name == "page_count_for"),
        "non-generic callables admitted"
    );

    // page_count_for: ceil(total / stride), 0 for empty/zero-stride.
    compare(
        &program,
        &admitted,
        "page_count_for",
        vec![u64_arg(64), u64_arg(137)],
        vec![u64_arg(3)],
    );
    compare(
        &program,
        &admitted,
        "page_count_for",
        vec![u64_arg(0), u64_arg(137)],
        vec![u64_arg(0)],
    );
    compare(
        &program,
        &admitted,
        "page_count_for",
        vec![u64_arg(64), u64_arg(0)],
        vec![u64_arg(0)],
    );
    // Global span predicate: start <= end <= total.
    compare(
        &program,
        &admitted,
        "span_valid_global",
        vec![u64_arg(100), u64_arg(10), u64_arg(20)],
        vec![bool_value(true)],
    );
    compare(
        &program,
        &admitted,
        "span_valid_global",
        vec![u64_arg(100), u64_arg(30), u64_arg(20)],
        vec![bool_value(false)],
    );
    // FNV basis constant from the fingerprint path.
    compare(
        &program,
        &admitted,
        "fnv_basis",
        vec![],
        vec![u64_arg(14695981039346656037)],
    );
}
