//! Execution evidence.
//!
//! A run emits one `ExecutionRecord`: identities, admitted inputs,
//! terminal outcome, resource observations, effect observations, and
//! digests. Execution is evidence of a bounded observation, never a
//! proof of semantic correctness.

use serde::{Deserialize, Serialize};

use crate::capability::{EffectRequest, EffectResponse};
use crate::outcome::Outcome;
use crate::resource::ResourceUsage;
use crate::value::Value;

/// One observed effect attempt and its result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectObservation {
    pub sequence: u64,
    pub request: EffectRequest,
    pub outcome_tag: String,
    pub response: Option<EffectResponse>,
}

/// Everything a run binds and observes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub schema_version: String,
    pub runtime_id: String,
    pub artifact_id: String,
    pub callable_function: String,
    pub callable_name: String,
    pub arguments: Vec<Value>,
    pub admitted_capabilities: Vec<String>,
    pub resource_limits: Vec<crate::resource::ResourceLimit>,
    pub outcome: Outcome,
    pub returned: Vec<Value>,
    pub usage: ResourceUsage,
    pub effects: Vec<EffectObservation>,
    /// sha256 over the canonical JSON of `returned`.
    pub return_digest: String,
    /// sha256 over the canonical JSON of the effect observations.
    pub effects_digest: String,
    /// Retained observation stream for debugger-driven runs (`None`
    /// for unobserved one-shot runs, whose shape is unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<mncs_model::ExecutionObservationStream>,
}

impl ExecutionRecord {
    pub fn digest_of<T: Serialize>(value: &T) -> String {
        let bytes = serde_json::to_vec(value).expect("evidence is serializable");
        crate::artifact::artifact_id_of(&bytes)
    }
}
