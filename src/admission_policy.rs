//! Host transport for MNCS-owned frozen-artifact compatibility decisions.
//!
//! Rust preserves the historic refusal order, bounds each policy packet,
//! and projects typed policy results into the public admission errors. The
//! supported revisions and resource dimensions are decided by the embedded
//! MNCS policy artifact.

use std::sync::Arc;

use mncs_model::{ExecutionValue, IntegerType};

use crate::admit::AdmissionRefusal;
use crate::artifact::BoundDecl;
use crate::capability::CapabilityEnv;
use crate::engine::{CallTarget, Engine};
use crate::outcome::Outcome;
use crate::resource::{ResourceEnvelope, ResourceLimit, ResourceUsage};
use crate::session::{native_policy_artifact, REQUEST_POLICY_MODULE};
use crate::value::{from_wire, Value};

const BOUND_DIMENSION_CHUNK: usize = 16;
const POLICY_STEPS: u64 = 250_000;
const POLICY_CALL_DEPTH: u64 = 8;
const POLICY_MEMORY_CELLS: u64 = 100_000;
const POLICY_ITERATIONS: u64 = 100_000;

struct PolicyExecution {
    value: Value,
    usage: ResourceUsage,
}

fn policy_failure(detail: impl Into<String>) -> AdmissionRefusal {
    AdmissionRefusal::PolicyFailure {
        detail: detail.into(),
    }
}

fn policy_envelope() -> ResourceEnvelope {
    ResourceEnvelope {
        limits: vec![
            ResourceLimit {
                dimension: "steps".into(),
                limit: POLICY_STEPS,
            },
            ResourceLimit {
                dimension: "call_depth".into(),
                limit: POLICY_CALL_DEPTH,
            },
            ResourceLimit {
                dimension: "memory_cells".into(),
                limit: POLICY_MEMORY_CELLS,
            },
            ResourceLimit {
                dimension: "effects".into(),
                limit: 0,
            },
            ResourceLimit {
                dimension: "iterations".into(),
                limit: POLICY_ITERATIONS,
            },
        ],
    }
}

fn wire_u64(value: u64) -> ExecutionValue {
    ExecutionValue::Integer {
        value: i128::from(value),
        ty: IntegerType {
            bits: 64,
            signed: false,
        },
    }
}

fn wire_bytes(value: &str) -> ExecutionValue {
    ExecutionValue::Sequence {
        values: Arc::new(
            value
                .as_bytes()
                .iter()
                .take(64)
                .map(|byte| ExecutionValue::Byte {
                    value: i128::from(*byte),
                })
                .collect(),
        ),
    }
}

fn wire_sequence(values: Vec<ExecutionValue>) -> ExecutionValue {
    ExecutionValue::Sequence {
        values: Arc::new(values),
    }
}

fn execute(
    function: &str,
    arguments: Vec<ExecutionValue>,
) -> Result<PolicyExecution, AdmissionRefusal> {
    let admitted = native_policy_artifact()
        .map_err(|error| policy_failure(format!("embedded VM policy is unavailable: {error}")))?;
    let capabilities = CapabilityEnv::empty();
    let engine = Engine::new(admitted, policy_envelope(), &capabilities)
        .ok_or_else(|| policy_failure("embedded VM policy has no executable code"))?;
    let result = engine.run(
        CallTarget::ByName {
            module: REQUEST_POLICY_MODULE.to_owned(),
            name: function.to_owned(),
        },
        arguments.iter().map(from_wire).collect(),
    );
    if !matches!(result.outcome, Outcome::Completed) || result.returned.len() != 1 {
        return Err(policy_failure(format!(
            "{function} did not return one completed policy result: {:?}",
            result.outcome
        )));
    }
    let execution = PolicyExecution {
        value: result
            .returned
            .into_iter()
            .next()
            .expect("one result checked"),
        usage: result.usage,
    };
    if execution.usage.effects != 0 {
        return Err(policy_failure(format!(
            "{function} attempted {} effects",
            execution.usage.effects
        )));
    }
    Ok(execution)
}

fn returned_u64(value: Value, function: &str) -> Result<u64, AdmissionRefusal> {
    match value {
        Value::Integer { value, .. } => u64::try_from(value)
            .map_err(|_| policy_failure(format!("{function} returned a non-u64 status"))),
        _ => Err(policy_failure(format!(
            "{function} returned a non-integer status"
        ))),
    }
}

fn record_u64(value: &Value, field: &str, function: &str) -> Result<u64, AdmissionRefusal> {
    let Value::Record { fields, .. } = value else {
        return Err(policy_failure(format!(
            "{function} returned a non-record result"
        )));
    };
    let Some((_, value)) = fields.iter().find(|(name, _)| name == field) else {
        return Err(policy_failure(format!("{function} omitted {field}")));
    };
    match value {
        Value::Integer { value, .. } => u64::try_from(*value)
            .map_err(|_| policy_failure(format!("{function} returned an invalid {field}"))),
        _ => Err(policy_failure(format!(
            "{function} returned a non-integer {field}"
        ))),
    }
}

fn classify_text(function: &str, value: &str) -> Result<u64, AdmissionRefusal> {
    let result = execute(
        function,
        vec![wire_bytes(value), wire_u64(value.len() as u64)],
    )?;
    returned_u64(result.value, function)
}

pub(crate) fn check_artifact_schema(schema: &str) -> Result<(), AdmissionRefusal> {
    match classify_text("classify_artifact_schema", schema)? {
        0 => Ok(()),
        1 => Err(AdmissionRefusal::IncompatibleContract {
            detail: format!(
                "artifact schema {schema} != {}",
                crate::artifact::ARTIFACT_SCHEMA_VERSION
            ),
        }),
        status => Err(policy_failure(format!(
            "classify_artifact_schema returned unknown status {status}"
        ))),
    }
}

pub(crate) fn check_vm_contract(contract: &str) -> Result<(), AdmissionRefusal> {
    match classify_text("classify_vm_contract", contract)? {
        0 => Ok(()),
        1 => Err(AdmissionRefusal::IncompatibleContract {
            detail: format!("vm contract {contract} != {}", crate::artifact::VM_CONTRACT),
        }),
        status => Err(policy_failure(format!(
            "classify_vm_contract returned unknown status {status}"
        ))),
    }
}

pub(crate) fn check_bounds(bounds: &[BoundDecl]) -> Result<(), AdmissionRefusal> {
    for chunk in bounds.chunks(BOUND_DIMENSION_CHUNK) {
        let dimensions = wire_sequence(
            chunk
                .iter()
                .map(|bound| wire_bytes(&bound.dimension))
                .collect(),
        );
        let lengths = wire_sequence(
            chunk
                .iter()
                .map(|bound| wire_u64(bound.dimension.len() as u64))
                .collect(),
        );
        let result = execute(
            "classify_bound_dimensions",
            vec![dimensions, lengths, wire_u64(chunk.len() as u64)],
        )?;
        let status = record_u64(&result.value, "status", "classify_bound_dimensions")?;
        match status {
            0 => {}
            1 => {
                let index = record_u64(&result.value, "index", "classify_bound_dimensions")?;
                let index = usize::try_from(index)
                    .map_err(|_| policy_failure("native bound index does not fit usize"))?;
                let Some(bound) = chunk.get(index) else {
                    return Err(policy_failure(format!(
                        "native bound index {index} exceeds its {BOUND_DIMENSION_CHUNK}-item packet"
                    )));
                };
                return Err(AdmissionRefusal::UnsupportedFeature {
                    feature: format!("bound-dimension:{}", bound.dimension),
                });
            }
            status => {
                return Err(policy_failure(format!(
                    "classify_bound_dimensions returned invalid packet/status {status}"
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn check_ssa_schema(schema: &str) -> Result<(), AdmissionRefusal> {
    match classify_text("classify_ssa_schema", schema)? {
        0 => Ok(()),
        1 => Err(AdmissionRefusal::UnsupportedFeature {
            feature: format!("ssa-schema:{schema}"),
        }),
        status => Err(policy_failure(format!(
            "classify_ssa_schema returned unknown status {status}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_bound_scan_obeys_its_resource_envelope() {
        let dimensions = [
            "steps",
            "call_depth",
            "memory_cells",
            "effects",
            "iterations",
            "steps",
            "call_depth",
            "memory_cells",
            "effects",
            "iterations",
            "steps",
            "call_depth",
            "memory_cells",
            "effects",
            "iterations",
            "steps",
        ];
        let result = execute(
            "classify_bound_dimensions",
            vec![
                wire_sequence(dimensions.iter().map(|value| wire_bytes(value)).collect()),
                wire_sequence(
                    dimensions
                        .iter()
                        .map(|value| wire_u64(value.len() as u64))
                        .collect(),
                ),
                wire_u64(dimensions.len() as u64),
            ],
        )
        .expect("bounded native policy execution");
        assert_eq!(
            record_u64(&result.value, "status", "classify_bound_dimensions").unwrap(),
            0
        );
        assert!(result.usage.steps < POLICY_STEPS);
        assert!(result.usage.memory_cells < POLICY_MEMORY_CELLS);
        assert_eq!(result.usage.effects, 0);
    }
}
