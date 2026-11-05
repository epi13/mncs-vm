//! Runtime session: admission context plus one execution at a time.
//!
//! A `Session` owns an admitted artifact. Each `call` binds the
//! callable identity, input values, capability environment, and
//! resource envelope explicitly, runs the engine, and returns the
//! terminal outcome with its evidence record. No ambient authority,
//! no hidden defaults: everything a run may observe is an argument.

use crate::admit::Admitted;
use crate::call_resolution_policy;
use crate::capability::CapabilityEnv;
use crate::engine::{CallTarget, Engine, EngineResult};
use crate::evidence::ExecutionRecord;
use crate::outcome::Outcome;
use crate::resource::{ResourceEnvelope, ResourceLimit, ResourceUsage};
use crate::value::{from_wire, to_wire, Value};
use std::sync::OnceLock;

const REQUEST_POLICY_ARTIFACT: &[u8] =
    include_bytes!("../native/mncs/vm/request_policy.artifact.json");
pub(crate) const REQUEST_POLICY_MODULE: &str = "mncs.vm.request_policy.v1";
const REQUEST_POLICY_FUNCTION: &str = "resolve_request_policy";
const MAX_NATIVE_RESOURCE_OVERRIDES: usize = 5;
static REQUEST_POLICY_ADMITTED: OnceLock<Result<Admitted, String>> = OnceLock::new();

pub(crate) fn native_policy_artifact() -> Result<&'static Admitted, String> {
    match REQUEST_POLICY_ADMITTED.get_or_init(|| {
        crate::admit::admit(REQUEST_POLICY_ARTIFACT)
            .map_err(|error| format!("cannot admit native request policy: {error}"))
    }) {
        Ok(admitted) => Ok(admitted),
        Err(error) => Err(error.clone()),
    }
}

struct NativePolicyOutput {
    status: u64,
    envelope: ResourceEnvelope,
    usage: ResourceUsage,
}

/// What one call binds.
pub struct CallSpec {
    pub target: CallTarget,
    /// Upstream wire arguments, marshalled at the boundary.
    pub arguments: Vec<mncs_model::ExecutionValue>,
    /// Explicit generic/type arguments selecting one compiled
    /// instantiation. Empty for concrete callables. The VM resolves
    /// these through the artifact's compiler-determined entrypoint
    /// table only; it never infers an instantiation.
    pub type_arguments: Vec<mncs_model::ExecutionTypeArgument>,
    /// Caller-supplied envelope; the tighter limit wins per dimension
    /// against artifact-declared bounds.
    pub envelope: ResourceEnvelope,
}

impl CallSpec {
    /// Bind the canonical compiler execution request to VM fuel. MNCS owns
    /// schema, authority, dimension, and min-merge policy; this method only
    /// marshals the request and projects its typed result. Artifact-declared
    /// bounds are still merged at execution. VM steps count VM
    /// instructions/edges, never universal CPU cost.
    pub fn from_request(
        request: mncs_model::ExecutionRequest,
        override_envelope: Option<&ResourceEnvelope>,
    ) -> Result<Self, String> {
        let policy = execute_native_request_policy(&request, override_envelope)?;
        if policy.usage.effects != 0 {
            return Err("native request policy attempted an effect".into());
        }
        let envelope = match policy.status {
            0 => policy.envelope,
            1 => return Err("unsupported execution request schema".into()),
            2 => {
                return Err(
                    "request host grants/policy require explicit VM capability bindings".into(),
                )
            }
            3 => return Err("unknown or duplicate resource dimension".into()),
            status => {
                return Err(format!(
                    "native request policy returned unknown status {status}"
                ))
            }
        };
        Ok(Self {
            target: CallTarget::ByName {
                module: request.target.module,
                name: request.target.function,
            },
            arguments: request.arguments,
            type_arguments: request.type_arguments,
            envelope,
        })
    }
}

/// Execute the policy artifact independently from the guest call. Request
/// metadata is bounded to the ABI's fixed sequence widths before entering
/// MNCS; lengths retain the original sizes so truncation cannot turn an
/// invalid value into a valid one.
fn execute_native_request_policy(
    request: &mncs_model::ExecutionRequest,
    override_envelope: Option<&ResourceEnvelope>,
) -> Result<NativePolicyOutput, String> {
    use mncs_model::{EffectExecutionPolicy, ExecutionValue, IntegerType};
    use std::sync::Arc;

    fn wire_u64(value: u64) -> ExecutionValue {
        ExecutionValue::Integer {
            value: i128::from(value),
            ty: IntegerType {
                bits: 64,
                signed: false,
            },
        }
    }

    fn wire_bool(value: bool) -> ExecutionValue {
        ExecutionValue::Boolean { value }
    }

    fn wire_bytes(value: &[u8]) -> ExecutionValue {
        ExecutionValue::Sequence {
            values: Arc::new(
                value
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

    let admitted = native_policy_artifact()?;

    let effect_policy = match &request.policy.effects {
        EffectExecutionPolicy::Unsupported => 0,
        EffectExecutionPolicy::Record => 1,
        EffectExecutionPolicy::Realize => 2,
    };
    let extra = override_envelope
        .map(|envelope| envelope.limits.as_slice())
        .unwrap_or(&[]);
    let bounded_extra = &extra[..extra.len().min(MAX_NATIVE_RESOURCE_OVERRIDES)];
    let dimensions = wire_sequence(
        bounded_extra
            .iter()
            .map(|limit| wire_bytes(limit.dimension.as_bytes()))
            .collect(),
    );
    let dimension_lengths = wire_sequence(
        bounded_extra
            .iter()
            .map(|limit| wire_u64(limit.dimension.len() as u64))
            .collect(),
    );
    let dimension_values = wire_sequence(
        bounded_extra
            .iter()
            .map(|limit| wire_u64(limit.limit))
            .collect(),
    );
    let arguments = vec![
        wire_bytes(request.schema_version.as_bytes()),
        wire_u64(request.schema_version.len() as u64),
        wire_bool(!request.host_grants.is_empty()),
        wire_u64(effect_policy),
        wire_u64(request.step_budget),
        wire_bool(request.call_depth_budget.is_some()),
        wire_u64(request.call_depth_budget.unwrap_or_default()),
        wire_u64(extra.len() as u64),
        dimensions,
        dimension_lengths,
        dimension_values,
    ];
    let capabilities = CapabilityEnv::empty();
    let engine = Engine::new(
        admitted,
        ResourceEnvelope {
            limits: vec![
                ResourceLimit {
                    dimension: "steps".into(),
                    limit: 100_000,
                },
                ResourceLimit {
                    dimension: "call_depth".into(),
                    limit: 8,
                },
                ResourceLimit {
                    dimension: "memory_cells".into(),
                    limit: 100_000,
                },
                ResourceLimit {
                    dimension: "effects".into(),
                    limit: 0,
                },
                ResourceLimit {
                    dimension: "iterations".into(),
                    limit: 10_000,
                },
            ],
        },
        &capabilities,
    )
    .ok_or_else(|| "native request policy artifact has no executable code".to_owned())?;
    let executed = engine.run(
        CallTarget::ByName {
            module: REQUEST_POLICY_MODULE.to_owned(),
            name: REQUEST_POLICY_FUNCTION.to_owned(),
        },
        arguments
            .into_iter()
            .map(|value| from_wire(&value))
            .collect(),
    );
    if !matches!(executed.outcome, Outcome::Completed) || executed.returned.len() != 1 {
        return Err(format!(
            "native request policy execution failed: {:?}",
            executed.outcome
        ));
    }
    let result = &executed.returned[0];
    let field = |name: &str| {
        result_field_u64(result, name)
            .ok_or_else(|| format!("native request policy omitted {name}"))
    };
    let status = field("status")?;
    let mut limits = vec![
        ResourceLimit {
            dimension: "steps".into(),
            limit: field("steps")?,
        },
        ResourceLimit {
            dimension: "call_depth".into(),
            limit: field("call_depth")?,
        },
        ResourceLimit {
            dimension: "memory_cells".into(),
            limit: field("memory_cells")?,
        },
        ResourceLimit {
            dimension: "effects".into(),
            limit: field("effects")?,
        },
        ResourceLimit {
            dimension: "iterations".into(),
            limit: field("iterations")?,
        },
    ];
    // Preserve the preexisting envelope iteration order as an API detail.
    // An explicit override previously passed through ResourceEnvelope::merged,
    // which sorts dimensions for a stable projected view.
    if override_envelope.is_some() {
        limits.sort_by(|left, right| left.dimension.cmp(&right.dimension));
    }
    Ok(NativePolicyOutput {
        status,
        envelope: ResourceEnvelope { limits },
        usage: executed.usage,
    })
}

#[cfg(test)]
mod request_policy_tests {
    use super::*;

    fn request() -> mncs_model::ExecutionRequest {
        mncs_model::ExecutionRequest {
            schema_version: "0.1".into(),
            target: mncs_model::ExecutionTarget {
                module: "policy.test.v1".into(),
                function: "unused".into(),
            },
            arguments: Vec::new(),
            type_arguments: Vec::new(),
            step_budget: 41,
            policy: Default::default(),
            host_grants: Vec::new(),
            call_depth_budget: None,
        }
    }

    #[test]
    fn native_policy_execution_stays_within_its_bounded_workload() {
        let execution = execute_native_request_policy(&request(), None).unwrap();
        assert_eq!(execution.status, 0);
        assert_eq!(execution.envelope.limit_of("steps"), Some(41));
        assert_eq!(execution.envelope.limit_of("call_depth"), Some(1024));
        assert!(execution.usage.steps < 100_000, "{:?}", execution.usage);
        assert_eq!(execution.usage.effects, 0);
        assert!(execution.usage.max_call_depth <= 8, "{:?}", execution.usage);
    }
}

/// A resolved call: the frozen function identity to execute plus the
/// display name recorded in evidence.
struct ResolvedCall {
    function: String,
    name: String,
}

/// Bind the native policy's selection to one frozen artifact row. Language
/// owns type spelling normalization, the admitted indexes provide candidate
/// rows, and MNCS decides the concrete/generic route and exact specialization.
fn resolve_call(
    admitted: &Admitted,
    target: &CallTarget,
    type_arguments: &[mncs_model::ExecutionTypeArgument],
) -> Result<ResolvedCall, String> {
    let (target_kind, concrete_index, candidate_indices, module_name) = match target {
        CallTarget::ByFunction { function } => (
            call_resolution_policy::TARGET_FUNCTION,
            admitted.callable_by_function(function),
            Vec::new(),
            None,
        ),
        CallTarget::ByName { module, name } => (
            call_resolution_policy::TARGET_NAME,
            admitted.callable_by_name(module, name),
            admitted.generic_entry_indices(module, name).to_vec(),
            Some((module.as_str(), name.as_str())),
        ),
    };
    let spellings: Vec<String> = if target_kind == call_resolution_policy::TARGET_NAME {
        type_arguments
            .iter()
            .map(mncs_model::ExecutionTypeArgument::normalized_spelling)
            .collect()
    } else {
        Vec::new()
    };
    let candidate_spellings =
        call_resolution_policy::candidate_spellings(admitted, &candidate_indices);
    let decision = call_resolution_policy::select(
        target_kind,
        !type_arguments.is_empty(),
        concrete_index,
        &spellings,
        &candidate_spellings,
    )?;

    match decision.status {
        call_resolution_policy::SELECT_CONCRETE => {
            let index = concrete_index.ok_or_else(|| {
                "native call-resolution policy selected an absent concrete callable".to_owned()
            })?;
            let entry = admitted
                .artifact
                .callables
                .get(index)
                .ok_or_else(|| "native call-resolution concrete index is out of range".to_owned())?;
            Ok(ResolvedCall {
                function: entry.function.clone(),
                name: format!("{}::{}", entry.module, entry.name),
            })
        }
        call_resolution_policy::SELECT_GENERIC => {
            let artifact_index = *candidate_indices.get(decision.index).ok_or_else(|| {
                "native call-resolution generic index is out of range".to_owned()
            })?;
            let entry = admitted
                .artifact
                .generic_entrypoints
                .get(artifact_index)
                .ok_or_else(|| "native call-resolution artifact row is out of range".to_owned())?;
            Ok(ResolvedCall {
                function: entry.function.clone(),
                name: format!(
                    "{}::{}<{}>",
                    entry.generic_module, entry.generic_function, entry.canonical_args
                ),
            })
        }
        call_resolution_policy::REFUSE_FUNCTION_TYPE_ARGUMENTS => match target {
            CallTarget::ByFunction { function } => Err(format!(
                "type arguments supplied for concrete function target {function:?}"
            )),
            CallTarget::ByName { .. } => Err(
                "native call-resolution policy returned a function-target refusal for a named target".to_owned(),
            ),
        },
        call_resolution_policy::REFUSE_MISSING_GENERIC_ARGUMENTS => match module_name {
            Some((module, name)) => Err(format!(
                "generic function {module}::{name} requires explicit type_arguments selecting a compiled instantiation"
            )),
            None => Err(
                "native call-resolution policy returned a generic-argument refusal without a named target".to_owned(),
            ),
        },
        call_resolution_policy::REFUSE_UNKNOWN_CALLABLE => Err("unknown callable".to_owned()),
        call_resolution_policy::REFUSE_TYPE_ARGUMENTS_FOR_CONCRETE => match module_name {
            Some((module, name)) => Err(format!(
                "generic arguments supplied for non-generic function {module}::{name}"
            )),
            None => Err(
                "native call-resolution policy returned a named-target refusal for a function identity".to_owned(),
            ),
        },
        call_resolution_policy::REFUSE_NO_GENERIC_INSTANTIATION => match module_name {
            Some((module, name)) => Err(format!(
                "no compiled generic instantiation of {module}::{name} matches this artifact: name the instantiation in the corpus so elaboration compiles it in"
            )),
            None => Err(
                "native call-resolution policy returned an instantiation refusal without a named target".to_owned(),
            ),
        },
        call_resolution_policy::REFUSE_NO_MATCHING_SPECIALIZATION => match module_name {
            Some((module, name)) => {
                let available = admitted.generic_spellings(module, name);
                let rendered: Vec<String> = available
                    .iter()
                    .map(|row| format!("({})", row.join(", ")))
                    .collect();
                Err(format!(
                    "no compiled specialization of generic function {module}::{name} for arguments ({}); compiled instantiations: [{}]",
                    spellings.join(", "),
                    rendered.join(", ")
                ))
            }
            None => Err(
                "native call-resolution policy returned a specialization refusal without a named target".to_owned(),
            ),
        },
        call_resolution_policy::REFUSE_INVALID_INPUT => Err(
            "native call-resolution policy rejected inconsistent bounded artifact/request data".to_owned(),
        ),
        status => Err(format!(
            "native call-resolution policy returned unknown status {status}"
        )),
    }
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

    /// Start one debugged call. Runs to the first bound stop,
    /// terminal boundary, or finish. The returned live execution (if
    /// stopped) holds the same run state that one-shot `call` would
    /// drive to completion: stopping changes nothing about how the
    /// program executes.
    pub fn start_debug<'e>(
        &'e mut self,
        caps: &'e CapabilityEnv,
        spec: CallSpec,
        config: crate::debug::DebugConfig,
    ) -> crate::debug::DebugStart<'e> {
        use crate::debug::{DebugStart, FinishRecord, LiveExecution};
        let resolved = resolve_call(self.admitted, &spec.target, &spec.type_arguments);
        let invalid = |session: &Self, reason: String| {
            let outcome = Outcome::InvalidRequest { reason };
            let stream = crate::debug::empty_stream();
            let mut record = session.record(
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
            record.observation = Some(stream.clone());
            DebugStart::Finished(Box::new(FinishRecord {
                outcome,
                record,
                stream,
            }))
        };
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(reason) => return invalid(self, reason),
        };
        let callable_function = resolved.function;
        let callable_name = resolved.name;
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
        let engine = match Engine::new(self.admitted, envelope.clone(), caps) {
            Some(engine) => engine,
            None => return invalid(self, "admitted artifact has no executable code".to_owned()),
        };
        let Some(function_index) = engine_function_index(&engine, &callable_function) else {
            return invalid(self, "callable function not in code section".to_owned());
        };
        let execution_id = crate::debug::execution_identity(
            self.admitted.artifact_id(),
            &callable_function,
            &spec.arguments,
            &caps.admitted_capabilities(),
            &envelope,
        );
        let (parts, begun) = engine.begin(function_index, arguments.clone());
        let mut live = LiveExecution::new(
            engine,
            parts,
            crate::debug::DebugDriver::new(
                self.admitted.ssa_module().expect("engine admitted SSA"),
                config,
                execution_id.clone(),
            ),
            execution_id,
            self.admitted.artifact_id().to_owned(),
            callable_function,
            callable_name,
            arguments,
            caps.admitted_capabilities(),
            envelope.limits.clone(),
        );
        if let Err(outcome) = begun {
            // Entry failed before any transition: finish directly with
            // the same outcome one-shot entry would produce.
            return DebugStart::Finished(Box::new(live.finish_invalid(outcome)));
        }
        match live.drive_first() {
            crate::debug::LiveEvent::Stopped(record) => DebugStart::Stopped(Box::new(live), record),
            crate::debug::LiveEvent::Finished(finished) => DebugStart::Finished(finished),
        }
    }

    /// Execute one call. Deterministic in the admitted inputs: the
    /// same artifact, callable, arguments, capabilities, providers,
    /// and envelope produce the same outcome and evidence.
    pub fn call(&mut self, caps: &CapabilityEnv, spec: CallSpec) -> (Outcome, ExecutionRecord) {
        let resolved = resolve_call(self.admitted, &spec.target, &spec.type_arguments);
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(reason) => {
                let outcome = Outcome::InvalidRequest { reason };
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
            }
        };
        let callable_function = resolved.function;
        let callable_name = resolved.name;
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
            resource_limits: ResourceEnvelope::merged(
                &self
                    .admitted
                    .artifact
                    .requirements
                    .bounds
                    .iter()
                    .map(|b| ResourceLimit {
                        dimension: b.dimension.clone(),
                        limit: b.limit,
                    })
                    .collect::<Vec<_>>(),
                &spec.envelope.limits,
            )
            .limits,
            outcome,
            returned,
            usage,
            effects,
            return_digest,
            effects_digest,
            observation: None,
        }
    }
}

/// Function index for a callable identity, mirroring [`Engine::run`].
fn engine_function_index(engine: &Engine, callable_function: &str) -> Option<usize> {
    engine.function_index(callable_function)
}
