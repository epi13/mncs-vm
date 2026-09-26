//! Capability environment and effect mediation.
//!
//! Authority is checked where it is exercised: an executing
//! instruction that names a capability dispatches through the
//! admitted environment. Absent authority fails closed with
//! `CapabilityDenied`; provider logic itself lives outside the VM
//! behind the [`Provider`] boundary.
//!
//! This is capability enforcement, not a sandbox claim. Process
//! isolation, memory safety against hostile code, and provider
//! isolation are separate claims requiring separate evidence.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::outcome::Outcome;
use crate::value::Value;

/// What an executing operation asks the outside world for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectRequest {
    pub capability: String,
    pub operation: String,
    pub inputs: Vec<Value>,
}

/// What came back across the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectResponse {
    pub provider: String,
    pub outputs: Vec<Value>,
    pub note: String,
}

/// External behavior behind one capability name. Implementations are
/// test fixtures, recorders, or family providers — never VM logic.
pub trait Provider: Send + Sync {
    fn identity(&self) -> &str;
    fn invoke(&self, request: &EffectRequest) -> Result<EffectResponse, Outcome>;
}

/// The admitted authority for one execution.
#[derive(Default)]
pub struct CapabilityEnv {
    bindings: BTreeMap<String, Box<dyn Provider>>,
}

impl CapabilityEnv {
    pub fn empty() -> Self {
        Self {
            bindings: BTreeMap::new(),
        }
    }

    pub fn bind<P: Provider + 'static>(mut self, capability: &str, provider: P) -> Self {
        self.bindings
            .insert(capability.to_owned(), Box::new(provider));
        self
    }

    pub fn admits(&self, capability: &str) -> bool {
        self.bindings.contains_key(capability)
    }

    pub fn admitted_capabilities(&self) -> Vec<String> {
        self.bindings.keys().cloned().collect()
    }

    /// Mediate one effect. Fails closed on missing authority; provider
    /// failures stay distinct from denials.
    pub fn dispatch(&self, request: &EffectRequest) -> Result<EffectResponse, Outcome> {
        match self.bindings.get(&request.capability) {
            Some(provider) => provider.invoke(request).map_err(|outcome| match outcome {
                Outcome::ProviderFailure { .. } | Outcome::Unsupported { .. } => outcome,
                other => Outcome::ProviderFailure {
                    provider: provider.identity().to_owned(),
                    detail: format!("provider exited abnormally: {}", other.tag()),
                },
            }),
            None => Err(Outcome::CapabilityDenied {
                capability: request.capability.clone(),
                operation: request.operation.clone(),
            }),
        }
    }
}

/// A provider that answers with fixed values. For deterministic
/// conformance fixtures: the same admitted input always observes
/// the same output.
pub struct ConstProvider {
    pub identity: String,
    pub outputs: Vec<Value>,
}

impl Provider for ConstProvider {
    fn identity(&self) -> &str {
        &self.identity
    }

    fn invoke(&self, _request: &EffectRequest) -> Result<EffectResponse, Outcome> {
        Ok(EffectResponse {
            provider: self.identity.clone(),
            outputs: self.outputs.clone(),
            note: "const".to_owned(),
        })
    }
}

/// A provider that always fails. Proves the denial/failure split:
/// admitted authority with a failing provider is `ProviderFailure`,
/// never `CapabilityDenied`.
pub struct FailingProvider {
    pub identity: String,
    pub detail: String,
}

impl Provider for FailingProvider {
    fn identity(&self) -> &str {
        &self.identity
    }

    fn invoke(&self, _request: &EffectRequest) -> Result<EffectResponse, Outcome> {
        Err(Outcome::ProviderFailure {
            provider: self.identity.clone(),
            detail: self.detail.clone(),
        })
    }
}
