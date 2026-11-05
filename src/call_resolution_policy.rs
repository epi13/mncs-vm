//! MNCS-native call target and generic instantiation selection.
//!
//! Language normalizes explicit type arguments. Rust supplies indexed
//! artifact rows and marshals them into a bounded packet; the admitted
//! request-policy artifact chooses the concrete/generic route and exact
//! specialization. Rust projects the selected artifact row into the VM.

use std::sync::Arc;

use crate::admit::Admitted;
use crate::capability::CapabilityEnv;
use crate::engine::{CallTarget, Engine};
use crate::outcome::Outcome;
#[cfg(test)]
use crate::resource::ResourceUsage;
use crate::resource::{ResourceEnvelope, ResourceLimit};
use crate::session::{native_policy_artifact, REQUEST_POLICY_MODULE};
use crate::value::Value;

pub(crate) const TARGET_FUNCTION: u64 = 0;
pub(crate) const TARGET_NAME: u64 = 1;

pub(crate) const SELECT_CONCRETE: u64 = 0;
pub(crate) const SELECT_GENERIC: u64 = 1;
pub(crate) const REFUSE_FUNCTION_TYPE_ARGUMENTS: u64 = 2;
pub(crate) const REFUSE_MISSING_GENERIC_ARGUMENTS: u64 = 3;
pub(crate) const REFUSE_UNKNOWN_CALLABLE: u64 = 4;
pub(crate) const REFUSE_TYPE_ARGUMENTS_FOR_CONCRETE: u64 = 5;
pub(crate) const REFUSE_NO_GENERIC_INSTANTIATION: u64 = 6;
pub(crate) const REFUSE_NO_MATCHING_SPECIALIZATION: u64 = 7;
pub(crate) const REFUSE_INVALID_INPUT: u64 = 8;

const CALL_POLICY_FUNCTION: &str = "resolve_call_policy";
const MAX_PACKET_BYTES: usize = 1024;
const MAX_GENERIC_CANDIDATES: usize = 1024;
const CALL_POLICY_STEPS: u64 = 5_000_000;
const CALL_POLICY_CALL_DEPTH: u64 = 8;
const CALL_POLICY_MEMORY_CELLS: u64 = 2_000_000;
const CALL_POLICY_ITERATIONS: u64 = 2_000_000;

pub(crate) struct CallDecision {
    pub(crate) status: u64,
    pub(crate) index: usize,
    #[cfg(test)]
    pub(crate) usage: ResourceUsage,
}

fn policy_envelope() -> ResourceEnvelope {
    ResourceEnvelope {
        limits: vec![
            ResourceLimit {
                dimension: "steps".into(),
                limit: CALL_POLICY_STEPS,
            },
            ResourceLimit {
                dimension: "call_depth".into(),
                limit: CALL_POLICY_CALL_DEPTH,
            },
            ResourceLimit {
                dimension: "memory_cells".into(),
                limit: CALL_POLICY_MEMORY_CELLS,
            },
            ResourceLimit {
                dimension: "effects".into(),
                limit: 0,
            },
            ResourceLimit {
                dimension: "iterations".into(),
                limit: CALL_POLICY_ITERATIONS,
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

fn packet(spellings: &[String]) -> Result<Vec<u8>, String> {
    let count = u16::try_from(spellings.len())
        .map_err(|_| "native call resolution type-argument count exceeds u16".to_owned())?;
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&count.to_le_bytes());
    for spelling in spellings {
        let bytes = spelling.as_bytes();
        let length = u16::try_from(bytes.len())
            .map_err(|_| "native call resolution type-argument spelling exceeds u16".to_owned())?;
        encoded.extend_from_slice(&length.to_le_bytes());
        encoded.extend_from_slice(bytes);
    }
    if encoded.len() > MAX_PACKET_BYTES {
        return Err(format!(
            "native call resolution argument packet exceeds {MAX_PACKET_BYTES} bytes"
        ));
    }
    Ok(encoded)
}

fn result_field_u64(value: &Value, field: &str) -> Option<u64> {
    let Value::Record { fields, .. } = value else {
        return None;
    };
    let (_, value) = fields.iter().find(|(name, _)| name == field)?;
    match value {
        Value::Integer { value, .. } => u64::try_from(*value).ok(),
        _ => None,
    }
}

/// Resolve one request using MNCS policy. Candidate order is the VM's
/// admitted artifact order; duplicate spellings are rejected at admission.
pub(crate) fn select(
    target_kind: u64,
    has_type_arguments: bool,
    concrete_index: Option<usize>,
    requested_spellings: &[String],
    candidate_spellings: &[&[String]],
) -> Result<CallDecision, String> {
    if candidate_spellings.len() > MAX_GENERIC_CANDIDATES {
        return Err(format!(
            "native call resolution supports at most {MAX_GENERIC_CANDIDATES} specializations per generic function"
        ));
    }
    let requested = packet(requested_spellings)?;
    let candidate_packets: Vec<Value> = candidate_spellings
        .iter()
        .map(|spellings| packet(spellings).map(|encoded| byte_sequence(&encoded)))
        .collect::<Result<_, _>>()?;
    let candidate_lengths: Vec<Value> = candidate_packets
        .iter()
        .map(|candidate| match candidate {
            Value::Sequence { elements } => unsigned(elements.len() as u64),
            _ => unreachable!("candidate packet is a byte sequence"),
        })
        .collect();

    let admitted = native_policy_artifact()?;
    let capabilities = CapabilityEnv::empty();
    let engine = Engine::new(admitted, policy_envelope(), &capabilities).ok_or_else(|| {
        "native call-resolution policy artifact has no executable code".to_owned()
    })?;
    let execution = engine.run(
        CallTarget::ByName {
            module: REQUEST_POLICY_MODULE.to_owned(),
            name: CALL_POLICY_FUNCTION.to_owned(),
        },
        vec![
            unsigned(target_kind),
            Value::Boolean(has_type_arguments),
            Value::Boolean(concrete_index.is_some()),
            unsigned(concrete_index.unwrap_or_default() as u64),
            byte_sequence(&requested),
            unsigned(requested.len() as u64),
            Value::Sequence {
                elements: Arc::new(candidate_packets),
            },
            Value::Sequence {
                elements: Arc::new(candidate_lengths),
            },
            unsigned(candidate_spellings.len() as u64),
        ],
    );
    if !matches!(execution.outcome, Outcome::Completed) || execution.returned.len() != 1 {
        return Err(format!(
            "native call-resolution policy execution failed: {:?}",
            execution.outcome
        ));
    }
    let result = &execution.returned[0];
    let status = result_field_u64(result, "status")
        .ok_or_else(|| "native call-resolution policy omitted status".to_owned())?;
    let index = result_field_u64(result, "index")
        .ok_or_else(|| "native call-resolution policy omitted index".to_owned())?;
    let usage = execution.usage;
    if usage.steps > CALL_POLICY_STEPS
        || usage.max_call_depth > CALL_POLICY_CALL_DEPTH
        || usage.max_memory_cells > CALL_POLICY_MEMORY_CELLS
        || usage.effects != 0
    {
        return Err(format!(
            "native call-resolution policy exceeded its declared resource envelope: {usage:?}"
        ));
    }
    let index = usize::try_from(index).map_err(|_| {
        "native call-resolution policy index exceeds the host index range".to_owned()
    })?;
    Ok(CallDecision {
        status,
        index,
        #[cfg(test)]
        usage,
    })
}

pub(crate) fn candidate_spellings<'a>(
    admitted: &'a Admitted,
    candidate_indices: &[usize],
) -> Vec<&'a [String]> {
    candidate_indices
        .iter()
        .map(|index| {
            admitted.artifact.generic_entrypoints[*index]
                .args_spellings
                .as_slice()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn shared_policy_artifact_exposes_call_resolution() {
        let admitted = native_policy_artifact().expect("native policy admits");
        assert!(admitted
            .callable_by_name(REQUEST_POLICY_MODULE, CALL_POLICY_FUNCTION)
            .is_some());
    }

    #[test]
    fn native_policy_selects_or_refuses_every_call_shape() {
        let requested = row(&["[byte; 8]"]);
        let other = row(&["[byte; 4]"]);
        let candidates: [&[String]; 2] = [&other, &requested];
        let cases = [
            (
                TARGET_FUNCTION,
                false,
                Some(17),
                &[][..],
                SELECT_CONCRETE,
                17,
            ),
            (
                TARGET_FUNCTION,
                true,
                Some(17),
                &[][..],
                REFUSE_FUNCTION_TYPE_ARGUMENTS,
                0,
            ),
            (
                TARGET_FUNCTION,
                false,
                None,
                &[][..],
                REFUSE_UNKNOWN_CALLABLE,
                0,
            ),
            (
                TARGET_NAME,
                false,
                Some(9),
                &candidates[..],
                SELECT_CONCRETE,
                9,
            ),
            (
                TARGET_NAME,
                false,
                None,
                &candidates[..],
                REFUSE_MISSING_GENERIC_ARGUMENTS,
                0,
            ),
            (
                TARGET_NAME,
                false,
                None,
                &[][..],
                REFUSE_UNKNOWN_CALLABLE,
                0,
            ),
            (
                TARGET_NAME,
                true,
                Some(9),
                &candidates[..],
                REFUSE_TYPE_ARGUMENTS_FOR_CONCRETE,
                0,
            ),
            (TARGET_NAME, true, None, &candidates[..], SELECT_GENERIC, 1),
            (
                TARGET_NAME,
                true,
                None,
                &[][..],
                REFUSE_NO_GENERIC_INSTANTIATION,
                0,
            ),
            (
                TARGET_NAME,
                true,
                None,
                &[other.as_slice()][..],
                REFUSE_NO_MATCHING_SPECIALIZATION,
                0,
            ),
        ];
        for (target, has_args, concrete, candidates, expected_status, expected_index) in cases {
            let decision = select(
                target,
                has_args,
                concrete,
                if has_args { &requested } else { &[] },
                candidates,
            )
            .expect("native call policy executes");
            assert_eq!(decision.status, expected_status);
            assert_eq!(decision.index, expected_index);
            assert!(decision.usage.steps <= CALL_POLICY_STEPS);
            assert_eq!(decision.usage.effects, 0);
        }
    }
}
