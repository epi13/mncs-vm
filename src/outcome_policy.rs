//! Native classification for the VM's structured terminal outcomes.
//!
//! The input is the stable `Outcome::tag()` projection; MNCS owns the
//! completed/abnormal decision used by Engine, Debug, and the CLI. The
//! policy is an ordinary admitted VM artifact with a small, explicit
//! resource envelope and no effects.

use std::sync::Arc;

use crate::capability::CapabilityEnv;
use crate::engine::{CallTarget, Engine};
use crate::outcome::Outcome;
#[cfg(test)]
use crate::resource::ResourceUsage;
use crate::resource::{ResourceEnvelope, ResourceLimit};
use crate::session::{native_policy_artifact, REQUEST_POLICY_MODULE};
use crate::value::Value;

const OUTCOME_POLICY_FUNCTION: &str = "is_completed_outcome";
const MAX_OUTCOME_TAG_BYTES: usize = 17;
const OUTCOME_POLICY_STEPS: u64 = 384;
const OUTCOME_POLICY_CALL_DEPTH: u64 = 4;
const OUTCOME_POLICY_MEMORY_CELLS: u64 = 128;
const OUTCOME_POLICY_ITERATIONS: u64 = 16;

pub(crate) struct NativeClassification {
    pub(crate) completed: bool,
    #[cfg(test)]
    pub(crate) usage: ResourceUsage,
}

fn policy_envelope() -> ResourceEnvelope {
    ResourceEnvelope {
        limits: vec![
            ResourceLimit {
                dimension: "steps".into(),
                limit: OUTCOME_POLICY_STEPS,
            },
            ResourceLimit {
                dimension: "call_depth".into(),
                limit: OUTCOME_POLICY_CALL_DEPTH,
            },
            ResourceLimit {
                dimension: "memory_cells".into(),
                limit: OUTCOME_POLICY_MEMORY_CELLS,
            },
            ResourceLimit {
                dimension: "effects".into(),
                limit: 0,
            },
            ResourceLimit {
                dimension: "iterations".into(),
                limit: OUTCOME_POLICY_ITERATIONS,
            },
        ],
    }
}

fn byte_sequence(bytes: &[u8]) -> Value {
    Value::Sequence {
        elements: Arc::new(bytes.iter().copied().map(Value::Byte).collect()),
    }
}

fn unsigned(value: u64) -> Value {
    Value::Integer {
        value: i128::from(value),
        bits: 64,
        signed: false,
    }
}

pub(crate) fn classify_tag(tag: &str) -> Result<NativeClassification, String> {
    if tag.len() > MAX_OUTCOME_TAG_BYTES {
        return Err(format!(
            "outcome tag exceeds the {MAX_OUTCOME_TAG_BYTES}-byte native policy bound"
        ));
    }

    let admitted = native_policy_artifact()?;
    let capabilities = CapabilityEnv::empty();
    let engine = Engine::new(admitted, policy_envelope(), &capabilities)
        .ok_or_else(|| "native outcome policy artifact has no executable code".to_owned())?;
    let execution = engine.run(
        CallTarget::ByName {
            module: REQUEST_POLICY_MODULE.to_owned(),
            name: OUTCOME_POLICY_FUNCTION.to_owned(),
        },
        vec![byte_sequence(tag.as_bytes()), unsigned(tag.len() as u64)],
    );
    if !matches!(&execution.outcome, Outcome::Completed) {
        return Err(format!(
            "native outcome policy execution failed: {:?}",
            execution.outcome
        ));
    }
    if execution.returned.len() != 1 {
        return Err(format!(
            "native outcome policy returned {} values, expected one",
            execution.returned.len()
        ));
    }
    let Value::Boolean(completed) = &execution.returned[0] else {
        return Err("native outcome policy returned a non-boolean value".to_owned());
    };
    let usage = execution.usage;
    if usage.steps > OUTCOME_POLICY_STEPS
        || usage.max_call_depth > OUTCOME_POLICY_CALL_DEPTH
        || usage.max_memory_cells > OUTCOME_POLICY_MEMORY_CELLS
        || usage.effects != 0
    {
        return Err(format!(
            "native outcome policy exceeded its declared resource envelope: {usage:?}"
        ));
    }
    Ok(NativeClassification {
        completed: *completed,
        #[cfg(test)]
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_runtime_policy_artifact_exposes_outcome_classification() {
        let admitted = native_policy_artifact().expect("native runtime policy admits");
        assert!(admitted
            .callable_by_name(REQUEST_POLICY_MODULE, OUTCOME_POLICY_FUNCTION)
            .is_some());
    }

    #[test]
    fn native_policy_classifies_every_terminal_variant_with_bounded_pure_work() {
        let cases = [
            (Outcome::Completed, true),
            (
                Outcome::ProgramFailure {
                    mode: "declared".into(),
                    detail: "failure".into(),
                },
                false,
            ),
            (
                Outcome::Trap {
                    detail: "trap".into(),
                },
                false,
            ),
            (
                Outcome::CapabilityDenied {
                    capability: "fixture".into(),
                    operation: "read".into(),
                },
                false,
            ),
            (
                Outcome::Unsupported {
                    feature: "fixture".into(),
                },
                false,
            ),
            (
                Outcome::InvalidRequest {
                    reason: "fixture".into(),
                },
                false,
            ),
            (
                Outcome::BudgetExhausted {
                    dimension: "steps".into(),
                },
                false,
            ),
            (
                Outcome::ProviderFailure {
                    provider: "fixture".into(),
                    detail: "failure".into(),
                },
                false,
            ),
            (
                Outcome::HostFailure {
                    detail: "failure".into(),
                },
                false,
            ),
            (
                Outcome::Terminated {
                    detail: "stop".into(),
                },
                false,
            ),
        ];

        for (outcome, expected) in cases {
            let classification = classify_tag(outcome.tag()).expect("native classification");
            assert_eq!(classification.completed, expected, "{outcome:?}");
            assert_eq!(outcome.is_completed().unwrap(), expected, "{outcome:?}");
            assert!(classification.usage.steps <= OUTCOME_POLICY_STEPS);
            assert!(classification.usage.max_call_depth <= OUTCOME_POLICY_CALL_DEPTH);
            assert!(classification.usage.max_memory_cells <= OUTCOME_POLICY_MEMORY_CELLS);
            assert_eq!(classification.usage.effects, 0);
        }

        assert!(
            !classify_tag("future_outcome")
                .expect("unknown tags classify conservatively as abnormal")
                .completed
        );
        assert!(classify_tag("future_outcome_tag_that_is_too_long").is_err());
    }
}
