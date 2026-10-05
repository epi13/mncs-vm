//! Capability enforcement: authority is checked where it is
//! exercised. The environment unit tests pin allow/deny/failure;
//! the engine path pins fail-closed dispatch on real HostCall code.

use mncs_vm::capability::{CapabilityEnv, ConstProvider, EffectRequest, FailingProvider};
use mncs_vm::engine::CallTarget;
use mncs_vm::harness::tests_only::compile_corpus;
use mncs_vm::outcome::Outcome;
use mncs_vm::resource::{ResourceEnvelope, ResourceLimit};
use mncs_vm::session::{CallSpec, Session};
use mncs_vm::value::Value;

fn request(capability: &str) -> EffectRequest {
    EffectRequest {
        capability: capability.to_owned(),
        operation: "probe".to_owned(),
        inputs: Vec::new(),
    }
}

#[test]
fn bound_provider_answers() {
    let env = CapabilityEnv::empty().bind(
        "sensor",
        ConstProvider {
            identity: "sensor-stub".to_owned(),
            outputs: vec![Value::Integer {
                value: 42,
                bits: 64,
                signed: true,
            }],
        },
    );
    let response = env.dispatch(&request("sensor")).expect("admitted");
    assert_eq!(response.provider, "sensor-stub");
    assert_eq!(
        response.outputs,
        vec![Value::Integer {
            value: 42,
            bits: 64,
            signed: true
        }]
    );
}

#[test]
fn missing_authority_denies_closed() {
    let env = CapabilityEnv::empty();
    let outcome = env.dispatch(&request("sensor")).unwrap_err();
    assert!(
        matches!(outcome, Outcome::CapabilityDenied { ref capability, .. } if capability == "sensor"),
        "{outcome:?}"
    );
}

#[test]
fn failing_provider_is_not_a_denial() {
    let env = CapabilityEnv::empty().bind(
        "flaky",
        FailingProvider {
            identity: "flaky-stub".to_owned(),
            detail: "boom".to_owned(),
        },
    );
    let outcome = env.dispatch(&request("flaky")).unwrap_err();
    assert!(
        matches!(outcome, Outcome::ProviderFailure { ref provider, .. } if provider == "flaky-stub"),
        "{outcome:?}"
    );
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

/// Drive the real `clock_read` HostCall through the engine twice:
/// once with no authority (denied, fail-closed) and once with a
/// bound stub provider (answered, recorded).
#[test]
fn host_call_mediation() {
    let admitted = match mncs_vm::harness::compile_file(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("corpus")
            .join("clock.mncs"),
    ) {
        Ok(admitted) => admitted,
        Err(error) => {
            panic!("clock corpus must compile for capability tests: {error}");
        }
    };
    let _ = compile_corpus("arith.mncs");
    let target = CallTarget::ByName {
        module: "mncs.vmcorpus.clock.v1".to_owned(),
        name: "read_clock".to_owned(),
    };
    let spec = || CallSpec {
        target: target.clone(),
        arguments: Vec::new(),
        type_arguments: Vec::new(),
        envelope: envelope(),
    };
    let mut denied_session = Session::open(&admitted);
    let (outcome, record) = denied_session.call(&CapabilityEnv::empty(), spec());
    assert!(
        matches!(outcome, Outcome::CapabilityDenied { .. }),
        "{outcome:?}"
    );
    assert_eq!(record.effects.len(), 1);
    assert_eq!(record.effects[0].outcome_tag, "capability_denied");

    let env = CapabilityEnv::empty().bind(
        "clock_capability",
        ConstProvider {
            identity: "clock-stub".to_owned(),
            outputs: vec![Value::Integer {
                value: 1_700_000_000,
                bits: 64,
                signed: false,
            }],
        },
    );
    let mut allowed_session = Session::open(&admitted);
    let (outcome, record) = allowed_session.call(&env, spec());
    assert_eq!(outcome, Outcome::Completed, "{outcome:?}");
    assert_eq!(record.effects.len(), 1);
    assert_eq!(record.effects[0].outcome_tag, "completed");
    match &record.returned[0] {
        Value::Integer { value, .. } => assert_eq!(*value, 1_700_000_000),
        other => panic!("expected stub clock value, got {other:?}"),
    }
}
