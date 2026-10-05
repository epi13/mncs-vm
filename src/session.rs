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
    /// Bind the canonical compiler execution request to VM fuel. Request
    /// budgets override finite VM defaults; an explicit envelope may only
    /// tighten them. Artifact-declared bounds are still merged at execution.
    /// VM steps count VM instructions/edges, never universal CPU cost.
    pub fn from_request(
        request: mncs_model::ExecutionRequest,
        override_envelope: Option<&ResourceEnvelope>,
    ) -> Result<Self, String> {
        if request.schema_version != "0.1" {
            return Err("unsupported execution request schema".into());
        }
        if !request.host_grants.is_empty()
            || request.policy != mncs_model::ExecutionPolicy::default()
        {
            return Err(
                "request host grants/policy require explicit VM capability bindings".into(),
            );
        }
        let mut envelope = ResourceEnvelope::default();
        for limit in &mut envelope.limits {
            if limit.dimension == "steps" {
                limit.limit = request.step_budget;
            }
            if limit.dimension == "call_depth" {
                if let Some(depth) = request.call_depth_budget {
                    limit.limit = depth;
                }
            }
        }
        if let Some(extra) = override_envelope {
            let mut seen = std::collections::BTreeSet::new();
            for limit in &extra.limits {
                if !crate::admit::ENFORCED_BOUNDS.contains(&limit.dimension.as_str())
                    || !seen.insert(&limit.dimension)
                {
                    return Err("unknown or duplicate resource dimension".into());
                }
            }
            envelope = ResourceEnvelope::merged(&envelope.limits, &extra.limits);
        }
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

/// A resolved call: the frozen function identity to execute plus the
/// display name recorded in evidence.
struct ResolvedCall {
    function: String,
    name: String,
}

/// Resolve a call target plus explicit type arguments to one frozen
/// function identity. Pure table lookup over admitted content:
///
/// - concrete targets with no arguments bind the callable table;
/// - generic targets with arguments bind the entrypoint row whose
///   normalized spellings match exactly;
/// - everything else is an explicit refusal reason, never a guess.
///
/// Normalization reuses the upstream
/// `mncs_model::ExecutionTypeArgument::normalized_spelling` so VM
/// resolution agrees byte-for-byte with backend-session resolution.
fn resolve_call(
    admitted: &Admitted,
    target: &CallTarget,
    type_arguments: &[mncs_model::ExecutionTypeArgument],
) -> Result<ResolvedCall, String> {
    match target {
        CallTarget::ByFunction { function } => {
            if !type_arguments.is_empty() {
                return Err(format!(
                    "type arguments supplied for concrete function target {function:?}"
                ));
            }
            match admitted.callable_by_function(function) {
                Some(index) => {
                    let entry = &admitted.artifact.callables[index];
                    Ok(ResolvedCall {
                        function: entry.function.clone(),
                        name: format!("{}::{}", entry.module, entry.name),
                    })
                }
                None => Err("unknown callable".to_owned()),
            }
        }
        CallTarget::ByName { module, name } => {
            if type_arguments.is_empty() {
                return match admitted.callable_by_name(module, name) {
                    Some(index) => {
                        let entry = &admitted.artifact.callables[index];
                        Ok(ResolvedCall {
                            function: entry.function.clone(),
                            name: format!("{}::{}", entry.module, entry.name),
                        })
                    }
                    None if admitted.has_generic(module, name) => Err(format!(
                        "generic function {module}::{name} requires explicit type_arguments selecting a compiled instantiation"
                    )),
                    None => Err("unknown callable".to_owned()),
                };
            }
            if admitted.callable_by_name(module, name).is_some() {
                return Err(format!(
                    "generic arguments supplied for non-generic function {module}::{name}"
                ));
            }
            let spellings: Vec<String> = type_arguments
                .iter()
                .map(|argument| argument.normalized_spelling())
                .collect();
            match admitted.generic_entry(module, name, &spellings) {
                Some(index) => {
                    let entry = &admitted.artifact.generic_entrypoints[index];
                    Ok(ResolvedCall {
                        function: entry.function.clone(),
                        name: format!(
                            "{}::{}<{}>",
                            entry.generic_module, entry.generic_function, entry.canonical_args
                        ),
                    })
                }
                None => {
                    let available = admitted.generic_spellings(module, name);
                    if available.is_empty() {
                        Err(format!(
                            "no compiled generic instantiation of {module}::{name} matches this artifact: name the instantiation in the corpus so elaboration compiles it in"
                        ))
                    } else {
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
                }
            }
        }
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
