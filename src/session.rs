//! Runtime session: admission context plus one execution at a time.
//!
//! A `Session` owns an admitted artifact. Each `call` binds the
//! callable identity, input values, capability environment, and
//! resource envelope explicitly, runs the engine, and returns the
//! terminal outcome with its evidence record. No ambient authority,
//! no hidden defaults: everything a run may observe is an argument.

use crate::admit::Admitted;
use crate::capability::CapabilityEnv;
use crate::engine::{CallTarget, Engine, EngineResult};
use crate::evidence::ExecutionRecord;
use crate::outcome::Outcome;
use crate::resource::{ResourceEnvelope, ResourceLimit, ResourceUsage};
use crate::value::{from_wire, to_wire, Value};

/// What one call binds.
pub struct CallSpec {
    pub target: CallTarget,
    /// Upstream wire arguments, marshalled at the boundary.
    pub arguments: Vec<mncs_model::ExecutionValue>,
    /// Caller-supplied envelope; the tighter limit wins per dimension
    /// against artifact-declared bounds.
    pub envelope: ResourceEnvelope,
}

/// A live runtime bound to one admitted artifact.
pub struct Session<'a> {
    admitted: &'a Admitted,
    runtime_id: String,
}

impl<'a> Session<'a> {
    pub fn open(admitted: &'a Admitted) -> Self {
        Self {
            admitted,
            runtime_id: format!("mncs-vm-runtime:{}", admitted.artifact_id()),
        }
    }

    pub fn artifact_id(&self) -> &str {
        self.admitted.artifact_id()
    }

    /// Execute one call. Deterministic in the admitted inputs: the
    /// same artifact, callable, arguments, capabilities, providers,
    /// and envelope produce the same outcome and evidence.
    pub fn call(&mut self, caps: &CapabilityEnv, spec: CallSpec) -> (Outcome, ExecutionRecord) {
        let resolved = match &spec.target {
            CallTarget::ByName { module, name } => {
                self.admitted.callable_by_name(module, name)
            }
            CallTarget::ByFunction { function } => self.admitted.callable_by_function(function),
        };
        let Some(callable_index) = resolved else {
            let outcome = Outcome::InvalidRequest {
                reason: "unknown callable".to_owned(),
            };
            let record = self.record(
                caps,
                &spec,
                String::new(),
                String::new(),
                Vec::new(),
                outcome.clone(),
                Vec::new(),
                ResourceUsage::default(),
                Vec::new(),
            );
            return (outcome, record);
        };
        let entry = &self.admitted.artifact.callables[callable_index];
        let callable_function = entry.function.clone();
        let callable_name = format!("{}::{}", entry.module, entry.name);
        let declared: Vec<ResourceLimit> = self
            .admitted
            .artifact
            .requirements
            .bounds
            .iter()
            .map(|bound| ResourceLimit {
                dimension: bound.dimension.clone(),
                limit: bound.limit,
            })
            .collect();
        let envelope = ResourceEnvelope::merged(&declared, &spec.envelope.limits);
        let arguments: Vec<Value> = spec.arguments.iter().map(from_wire).collect();
        let engine = match Engine::new(self.admitted, envelope, caps) {
            Some(engine) => engine,
            None => {
                let outcome = Outcome::InvalidRequest {
                    reason: "admitted artifact has no executable code".to_owned(),
                };
                let record = self.record(
                    caps,
                    &spec,
                    callable_function,
                    callable_name,
                    arguments,
                    outcome.clone(),
                    Vec::new(),
                    ResourceUsage::default(),
                    Vec::new(),
                );
                return (outcome, record);
            }
        };
        let EngineResult {
            outcome,
            returned,
            usage,
            effects,
        } = engine.run(
            CallTarget::ByFunction {
                function: callable_function.clone(),
            },
            arguments.clone(),
        );
        let record = self.record(
            caps,
            &spec,
            callable_function,
            callable_name,
            arguments,
            outcome.clone(),
            returned,
            usage,
            effects,
        );
        (outcome, record)
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        caps: &CapabilityEnv,
        spec: &CallSpec,
        callable_function: String,
        callable_name: String,
        arguments: Vec<Value>,
        outcome: Outcome,
        returned: Vec<Value>,
        usage: ResourceUsage,
        effects: Vec<crate::evidence::EffectObservation>,
    ) -> ExecutionRecord {
        let return_digest =
            ExecutionRecord::digest_of(&returned.iter().map(to_wire).collect::<Vec<_>>());
        let effects_digest = ExecutionRecord::digest_of(&effects);
        ExecutionRecord {
            schema_version: "mncs.vm.execution-record/1".to_owned(),
            runtime_id: self.runtime_id.clone(),
            artifact_id: self.admitted.artifact_id().to_owned(),
            callable_function,
            callable_name,
            arguments,
            admitted_capabilities: caps.admitted_capabilities(),
            resource_limits: spec.envelope.limits.clone(),
            outcome,
            returned,
            usage,
            effects,
            return_digest,
            effects_digest,
        }
    }
}
