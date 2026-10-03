//! Live execution debugging: observation, safe points, stops, inspection, resume.
//!
//! VM-owned contract (`mncs.vm.debug/1`). The debugger, test harness,
//! profiler, or any other consumer drives a [`LiveExecution`]; the VM
//! owns suspension truthfully because run state is plain data and the
//! drive loop is re-entrant. Nothing here simulates a stop by
//! re-executing: a stop holds the same frames, values, usage, and
//! effect log that resume continues from.
//!
//! Live observations reuse the shared language-owned stream shape
//! (`mncs.execution-observation/1`): event kinds, value captures,
//! frames, effects, policy bounds, and completeness mean exactly what
//! they mean for completed reference-runtime runs. Only suspension
//! vocabulary (safe points, stop conditions, continuation tokens,
//! inspection views) is VM-owned, because only the VM can suspend
//! itself.
//!
//! ## Safe points
//!
//! A [`SafePoint`] names a position at which inspection or stopping
//! is valid. The VM guarantees these invariants at every safe point:
//!
//! - the current frame's value map is consistent: no partially
//!   written instruction effects are visible;
//! - the frame stack above the current frame is untouched;
//! - resource usage is charged exactly for completed transitions;
//! - the pending transition (instruction, effect dispatch, or
//!   terminal outcome) has NOT executed yet.
//!
//! Safe-point kinds: `operation` (before one SSA instruction
//! executes), `effect_before` (before provider dispatch; stopping
//! here never repeats or skips the effect), `effect_after` (after
//! the provider responded), and `terminal` (an abnormal outcome is
//! pending; inspectable but not resumable).
//!
//! ## Value identity
//!
//! A runtime value instance is identified by
//! `mncs:vm:value:<digest>` over (execution, frame sequence,
//! binding, version). The identity is stable while the instance is
//! unchanged and deterministic across replay of identical admitted
//! inputs. Versions count assignments per binding; SSA bindings
//! assigned once keep version 0. This is a runtime-local instance
//! identity, distinct from the semantic origin carried alongside it.

use std::collections::{BTreeMap, HashMap, HashSet};

use mncs_model::{
    ExecutionObservationCompleteness, ExecutionObservationEvent, ExecutionObservationPolicy,
    ExecutionObservationStream, ExecutionObservedEffect, ExecutionObservedFrame,
    ExecutionObservedValue, ExecutionValueCapture, ObservationCapturePolicy,
    ObservationCompletenessStatus, SemanticId, SsaModule,
};
use serde::{Deserialize, Serialize};

use crate::engine::{Engine, LiveParts};
use crate::evidence::{EffectObservation, ExecutionRecord};
use crate::outcome::Outcome;
use crate::resource::{ResourceEnvelope, ResourceUsage};
use crate::value::{to_wire, Value};

/// VM-owned live-debug contract revision.
pub const DEBUG_SCHEMA_VERSION: &str = "mncs.vm.debug/1";

/// What one debugged call binds, beyond [`crate::session::CallSpec`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugConfig {
    /// Shared observation policy: capture mode, bounds, selection.
    /// Stops work even when capture is `None`; the policy only
    /// scopes retained evidence.
    pub policy: ExecutionObservationPolicy,
    /// Stop conditions bound before entry.
    pub stops: Vec<StopCondition>,
    /// Suspend (inspectable, not resumable) before finalizing any
    /// abnormal terminal outcome instead of finishing directly.
    pub stop_on_abnormal_terminal: bool,
}

impl Default for DebugConfig {
    fn default() -> Self {
        Self {
            policy: ExecutionObservationPolicy::default(),
            stops: Vec::new(),
            stop_on_abnormal_terminal: true,
        }
    }
}

/// One bound stop condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopCondition {
    /// Caller-supplied or minted (`stop:<n>`) identity.
    pub id: String,
    pub target: StopTarget,
}

/// What a stop condition matches. All identities are authoritative
/// compiler/VM identities, never filename/line heuristics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopTarget {
    /// Before the SSA instruction with this identity executes.
    Operation { instruction: String },
    /// At entry to the function naming this identity (function or
    /// semantic identity both match).
    Function { function: String },
    /// Around provider dispatch: `before`, `after`, or `both`.
    EffectBoundary { phase: String },
    /// At any abnormal terminal boundary (failure, trap, budget,
    /// denial, provider failure, unsupported).
    FailureOrTrap,
}

/// Why execution suspended. Several reasons may coincide at one stop
/// (for example a step landing on a bound operation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopReason {
    Breakpoint { condition: String },
    Step,
    StepOver,
    StepOut,
    FunctionEntry { condition: String },
    EffectBoundary { condition: String, phase: String },
    FailureOrTrap,
}

/// A VM position at which inspection or stopping is valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafePoint {
    /// `operation`, `effect_before`, `effect_after`, or `terminal`.
    pub kind: String,
    pub execution: String,
    pub frame: String,
    pub depth: u64,
    pub function: String,
    pub function_semantic: String,
    pub block: String,
    pub instruction: Option<String>,
    pub instruction_semantic: Option<String>,
    /// Engine steps charged so far (completed transitions only).
    pub step_index: u64,
}

/// Retained-evidence counters at a stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationCounts {
    pub events: usize,
    pub values: usize,
    pub effects: usize,
    pub dropped_events: usize,
    pub dropped_values: usize,
    pub dropped_effects: usize,
}

/// The typed record produced when execution suspends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopRecord {
    pub schema_version: String,
    pub execution: String,
    pub artifact: String,
    pub callable_function: String,
    /// Monotonic per execution; resume must present the matching token.
    pub stop_sequence: u64,
    /// Single-owner continuation token. Present it to resume, step,
    /// or terminate; any older token is stale and refused.
    pub continuation_token: String,
    pub reasons: Vec<StopReason>,
    pub safe_point: SafePoint,
    pub usage: ResourceUsage,
    /// Digest over the live frame stack (functions, blocks, ips,
    /// value bindings). Deterministic across replay.
    pub state_digest: String,
    /// Digest over (execution, steps, effects, safe point, state).
    /// Equal transitions across replay produce equal digests.
    pub transition_digest: String,
    pub observations: ObservationCounts,
    /// Some when the stop sits on a terminal boundary: the abnormal
    /// outcome that will finalize on terminate. Resume is refused.
    pub pending_terminal: Option<Outcome>,
}

/// One inspected value: instance identity plus a bounded capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueView {
    pub binding: String,
    pub instance: String,
    pub version: u64,
    pub kind: String,
    pub type_name: String,
    pub capture: ExecutionValueCapture,
}

/// One inspected frame, innermost data first within a stack view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameView {
    pub frame: String,
    pub function: String,
    pub function_semantic: String,
    pub depth: u64,
    pub block: String,
    pub ip: usize,
    pub instruction: Option<String>,
    pub instruction_semantic: Option<String>,
    pub values: Vec<ValueView>,
    /// True when a per-request bound cut the value list.
    pub truncated: bool,
}

/// Bounded typed stack view. `frames[0]` is the current frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackView {
    pub execution: String,
    pub stop_sequence: u64,
    pub frames: Vec<FrameView>,
}

/// Terminal product of a debugged execution.
#[derive(Debug, Clone, PartialEq)]
pub struct FinishRecord {
    pub outcome: Outcome,
    pub record: ExecutionRecord,
    pub stream: ExecutionObservationStream,
}

/// What a resume/step call returns.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveEvent {
    Stopped(Box<StopRecord>),
    Finished(Box<FinishRecord>),
}

/// Typed debugger failures. Stale tokens fail closed; they never
/// resume a different execution or an older stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum DebugError {
    StaleToken { expected_sequence: u64 },
    BadToken { detail: String },
    AlreadyFinished,
    ResumeRefusedTerminal { outcome: String },
    UnknownStop { id: String },
    InvalidStop { detail: String },
    InvalidRequest { reason: String },
    InspectionBound { detail: String },
}

impl std::fmt::Display for DebugError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DebugError::StaleToken { expected_sequence } => {
                write!(f, "stale continuation token; stop {expected_sequence} is current")
            }
            DebugError::BadToken { detail } => write!(f, "bad continuation token: {detail}"),
            DebugError::AlreadyFinished => write!(f, "execution already finished"),
            DebugError::ResumeRefusedTerminal { outcome } => write!(
                f,
                "terminal stop cannot resume (outcome {outcome}); terminate to finalize"
            ),
            DebugError::UnknownStop { id } => write!(f, "unknown stop condition {id}"),
            DebugError::InvalidStop { detail } => write!(f, "invalid stop condition: {detail}"),
            DebugError::InvalidRequest { reason } => write!(f, "invalid debug request: {reason}"),
            DebugError::InspectionBound { detail } => write!(f, "inspection bound: {detail}"),
        }
    }
}

impl std::error::Error for DebugError {}

/// One-shot step arming. Persistent conditions still apply; the
/// one-shot adds a reason when it fires.
#[derive(Debug, Clone)]
pub(crate) enum OneShot {
    /// Stop at the very next operation safe point (steps into calls).
    NextOperation,
    /// Stop at the next operation safe point with depth <= entry.
    Over { max_depth: usize },
    /// Stop at the next operation safe point with depth < entry.
    Out { depth: usize },
}

/// How the drive loop exits when a debugger is attached.
pub(crate) enum DriveExit {
    Finished(Outcome),
    Suspended(Box<SuspendRequest>),
}

/// Suspension payload carried out of the drive loop.
pub(crate) struct SuspendRequest {
    pub reasons: Vec<StopReason>,
    pub safe_point: SafePoint,
    /// (frame sequence, block, ip) plus covered safe-point kinds, so
    /// resume skips stops at this arrival instead of re-firing them.
    pub position: (u64, String, usize),
    pub kinds: Vec<String>,
    /// Some when the stop sits on a terminal boundary: inspectable,
    /// but resume is refused and terminate finalizes this outcome.
    pub pending_terminal: Option<Outcome>,
}

/// Hot-path indexes over bound stop conditions: every semantic
/// transition evaluates stops, so matching is O(1) lookup plus work
/// proportional to matches, never a scan over all stops. Index
/// vectors hold config positions in ascending order, so reason order
/// is identical to the unindexed scan. Rebuilt on bind/clear (cold).
#[derive(Default)]
struct StopIndexes {
    by_operation: HashMap<String, Vec<usize>>,
    by_function: HashMap<String, Vec<usize>>,
    effect_before: Vec<usize>,
    effect_after: Vec<usize>,
    failure: Vec<usize>,
}

impl StopIndexes {
    fn rebuild(stops: &[StopCondition]) -> Self {
        let mut indexes = StopIndexes::default();
        for (index, condition) in stops.iter().enumerate() {
            match &condition.target {
                StopTarget::Operation { instruction } => {
                    indexes.by_operation.entry(instruction.clone()).or_default().push(index);
                }
                StopTarget::Function { function } => {
                    indexes.by_function.entry(function.clone()).or_default().push(index);
                }
                StopTarget::EffectBoundary { phase } => {
                    if phase == "before" || phase == "both" {
                        indexes.effect_before.push(index);
                    }
                    if phase == "after" || phase == "both" {
                        indexes.effect_after.push(index);
                    }
                }
                StopTarget::FailureOrTrap => indexes.failure.push(index),
            }
        }
        indexes
    }
}

/// Per-execution debug state: observation stream builder, stop
/// evaluation, and identity minting.
pub(crate) struct DebugDriver {
    module: SsaModule,
    config: DebugConfig,
    execution_id: String,
    stream: ExecutionObservationStream,
    next_sequence: u64,
    dropped_events: usize,
    dropped_values: usize,
    dropped_effects: usize,
    /// Frame identities mirroring the engine stack (entry first).
    frame_ids: Vec<SemanticId>,
    next_stop_mint: u64,
    one_shot: Option<OneShot>,
    /// (frame_seq, binding) -> (version, instance, last value).
    versions: BTreeMap<(u64, String), (u64, SemanticId, Value)>,
    /// Value binding -> producer operation identity (origin chains).
    producers: BTreeMap<String, String>,
    /// Value binding -> declared type name.
    type_names: BTreeMap<String, String>,
    /// Last operation position, for terminal safe points.
    position_function: usize,
    position_block: String,
    position_instruction: Option<String>,
    position_semantic: Option<String>,
    /// Suspended (frame, block, ip) plus the safe-point kinds covered
    /// by that stop. Stops there do not re-fire until arrival moves.
    last_stop: Option<((u64, String, usize), Vec<String>)>,
    /// Last position that emitted `operation_enter`: resume re-fires
    /// the op hook at the stop position, but emission happens once
    /// per actual transition.
    emit_enter: Option<(u64, String, usize)>,
    /// Last position that emitted `effect_invoke`, for the same
    /// resume re-fire reason. `effect_result` and `operation_result`
    /// never re-fire (the transition advanced past them).
    emit_invoke: Option<(u64, String, usize)>,
    /// A function-entry suspension already ran block entry for the
    /// callee entry block; the resumed loop skips it once.
    block_prefetched: bool,
    /// Monotonic effect counter for deterministic effect identities.
    next_effect: u64,
    /// Hot-path stop indexes (rebuilt on bind/clear) and the selected
    /// operation set (policy is immutable after construction).
    stop_indexes: StopIndexes,
    selected_operations: HashSet<String>,
    /// `short_digest(execution_id)`, computed once: frame identities
    /// embed it on every call.
    frame_prefix: String,
}

impl DebugDriver {
    pub(crate) fn new(
        module: &SsaModule,
        config: DebugConfig,
        execution_id: String,
    ) -> Self {
        let stream = ExecutionObservationStream {
            schema_version: mncs_model::EXECUTION_OBSERVATION_SCHEMA_VERSION.to_owned(),
            identity: SemanticId(format!("mncs:vm:observation:{}", short_digest(&execution_id))),
            execution_identity: SemanticId(execution_id.clone()),
            policy: config.policy.clone(),
            completeness: ExecutionObservationCompleteness {
                status: ObservationCompletenessStatus::Disabled,
                captured_events: 0,
                dropped_events: 0,
                captured_values: 0,
                dropped_values: 0,
                captured_effects: 0,
                truncated: false,
            },
            frames: Vec::new(),
            values: Vec::new(),
            effects: Vec::new(),
            events: Vec::new(),
        };
        let mut producers = BTreeMap::new();
        let mut type_names = BTreeMap::new();
        for function in &module.functions {
            for input in &function.inputs {
                type_names.insert(input.identity.0.clone(), input.ty.semantic_name());
            }
            for block in &function.blocks {
                for parameter in &block.parameters {
                    type_names.insert(parameter.identity.0.clone(), parameter.ty.semantic_name());
                }
                for instruction in &block.instructions {
                    for output in &instruction.outputs {
                        producers.insert(output.identity.0.clone(), instruction.identity.0.clone());
                        type_names.insert(output.identity.0.clone(), output.ty.semantic_name());
                    }
                }
            }
        }
        let frame_prefix = short_digest(&execution_id);
        let stop_indexes = StopIndexes::rebuild(&config.stops);
        let selected_operations: HashSet<String> = config
            .policy
            .selected_operations
            .iter()
            .map(|id| id.0.clone())
            .collect();
        Self {
            module: module.clone(),
            config,
            execution_id,
            stream,
            next_sequence: 0,
            dropped_events: 0,
            dropped_values: 0,
            dropped_effects: 0,
            frame_ids: Vec::new(),
            next_stop_mint: 0,
            one_shot: None,
            versions: BTreeMap::new(),
            producers,
            type_names,
            position_function: 0,
            position_block: String::new(),
            position_instruction: None,
            position_semantic: None,
            last_stop: None,
            emit_enter: None,
            emit_invoke: None,
            block_prefetched: false,
            next_effect: 0,
            stop_indexes,
            selected_operations,
            frame_prefix,
        }
    }

    /// Capture mode active for retained evidence.
    fn capture(&self) -> ObservationCapturePolicy {
        self.stream.policy.capture
    }

    fn captures_events(&self) -> bool {
        !matches!(self.capture(), ObservationCapturePolicy::None)
    }

    /// Whether an operation-scoped event is retained under the policy.
    /// Frame skeleton events (execution/frame enter/exit) are always
    /// retained while any capture is enabled so retained events stay
    /// interpretable; operation/block/return/failure/effect events
    /// follow the mode. Failure mode retains only failure-class events.
    fn retains_operation(&self, operation: Option<&str>, failure_class: bool) -> bool {
        match self.capture() {
            ObservationCapturePolicy::None => false,
            ObservationCapturePolicy::Selected => {
                operation.is_some_and(|identity| self.selected_operations.contains(identity))
            }
            ObservationCapturePolicy::FailureOnly => failure_class,
            ObservationCapturePolicy::Bounded | ObservationCapturePolicy::Diagnostic => true,
        }
    }

    /// Values are captured only for retained operation events: a
    /// selected stop set retains its own value closure, never the
    /// ambient value universe.
    fn retains_values_for(&self, operation: Option<&str>, failure_class: bool) -> bool {
        self.captures_events() && self.retains_operation(operation, failure_class)
    }

    // Identity input mirrors the event struct field-for-field; a
    // builder would only re-list the same eleven parameters.
    #[allow(clippy::too_many_arguments)]
    fn mint_event_id(
        &self,
        sequence: u64,
        kind: &str,
        frame: &Option<SemanticId>,
        block: &Option<SemanticId>,
        operation: &Option<SemanticId>,
        inputs: &[SemanticId],
        outputs: &[SemanticId],
        effect: &Option<SemanticId>,
        failure: &Option<SemanticId>,
        status: &Option<String>,
    ) -> SemanticId {
        // Borrowing material: byte-identical to the previous
        // `serde_json::json!` object (keys alphabetical, as the JSON
        // map orders them) without building an intermediate Value.
        // The derivation-stability test pins the resulting identities.
        #[derive(Serialize)]
        struct EventMaterial<'a> {
            block: &'a Option<SemanticId>,
            effect: &'a Option<SemanticId>,
            execution: &'a str,
            failure: &'a Option<SemanticId>,
            frame: &'a Option<SemanticId>,
            inputs: &'a [SemanticId],
            operation: &'a Option<SemanticId>,
            outputs: &'a [SemanticId],
            status: &'a Option<String>,
        }
        let material = EventMaterial {
            block,
            effect,
            execution: &self.execution_id,
            failure,
            frame,
            inputs,
            operation,
            outputs,
            status,
        };
        let bytes =
            serde_json::to_vec(&(sequence, kind, &material)).expect("event material serializes");
        SemanticId(format!("mncs:vm:event:{}", short_digest_hex(&bytes)))
    }

    // Positional event fields mirror the shared stream struct; a
    // builder would obscure the twelve call sites, not clarify them.
    #[allow(clippy::too_many_arguments)]
    fn push_event(
        &mut self,
        kind: &str,
        frame: Option<SemanticId>,
        block: Option<SemanticId>,
        operation: Option<SemanticId>,
        inputs: Vec<SemanticId>,
        outputs: Vec<SemanticId>,
        effect: Option<SemanticId>,
        failure: Option<SemanticId>,
        status: Option<String>,
        failure_class: bool,
    ) {
        if operation.is_some() || failure_class {
            let selected = operation.as_ref().map(|id| id.0.as_str());
            if !self.retains_operation(selected, failure_class) {
                return;
            }
        } else if !self.captures_events() {
            return;
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        if self.stream.events.len() >= self.stream.policy.max_events {
            self.dropped_events = self.dropped_events.saturating_add(1);
            return;
        }
        let identity = self.mint_event_id(
            sequence, kind, &frame, &block, &operation, &inputs, &outputs, &effect, &failure,
            &status,
        );
        self.stream.events.push(ExecutionObservationEvent {
            identity,
            sequence,
            kind: kind.to_owned(),
            frame,
            block,
            operation,
            inputs,
            outputs,
            effect,
            failure,
            status,
        });
    }

    /// Capture one value observation. Returns the instance identity,
    /// reusing the existing identity while the instance is unchanged.
    /// Returns `None` when the policy retains nothing here or the
    /// value budget is exhausted (counted as dropped).
    fn capture_value(
        &mut self,
        frame_seq: u64,
        binding: &str,
        observation: &str,
        operation: Option<&SemanticId>,
        value: &Value,
    ) -> Option<SemanticId> {
        let selected = operation.map(|id| id.0.as_str());
        if !self.retains_values_for(selected, false) {
            // Still mint a stable identity for live views, but retain
            // nothing in the stream.
            return Some(self.instance_identity(frame_seq, binding, value, false));
        }
        let instance = self.instance_identity(frame_seq, binding, value, true);
        if self.stream.values.len() >= self.stream.policy.max_values {
            self.dropped_values = self.dropped_values.saturating_add(1);
            return None;
        }
        let wire = to_wire(value);
        let bytes = serde_json::to_vec(&wire).unwrap_or_default();
        let total_cap = self.stream.policy.max_value_bytes;
        let capture = if bytes.len() <= total_cap.min(4096) && !bytes.is_empty() {
            ExecutionValueCapture::Full { value: wire }
        } else if bytes.is_empty() {
            ExecutionValueCapture::Unavailable {
                reason: "value did not serialize".to_owned(),
            }
        } else {
            ExecutionValueCapture::Truncated {
                sha256: crate::artifact::artifact_id_of(&bytes),
                bytes: bytes.len(),
            }
        };
        let type_name = self
            .type_names
            .get(binding)
            .cloned()
            .unwrap_or_else(|| value.kind_tag().to_owned());
        let origin = self.producers.get(binding).map(|id| SemanticId(id.clone()));
        let frame = self.frame_ids.last().cloned().unwrap_or_else(|| SemanticId("mncs:vm:frame:unknown".to_owned()));
        let version = self.versions.get(&(frame_seq, binding.to_owned())).map(|entry| entry.0).unwrap_or(0);
        self.stream.values.push(ExecutionObservedValue {
            identity: instance.clone(),
            logical_identity: SemanticId(binding.to_owned()),
            binding: binding.to_owned(),
            version,
            type_name,
            observation: observation.to_owned(),
            frame,
            operation: operation.cloned(),
            origin,
            capture,
        });
        Some(instance)
    }

    /// Stable instance identity for (frame, binding): unchanged while
    /// the value is unchanged, versioned on reassignment.
    fn instance_identity(
        &mut self,
        frame_seq: u64,
        binding: &str,
        value: &Value,
        retain_version: bool,
    ) -> SemanticId {
        let key = (frame_seq, binding.to_owned());
        if let Some((version, instance, last)) = self.versions.get(&key) {
            if last == value {
                return instance.clone();
            }
            let version = version + 1;
            let instance = mint_value_id(&self.execution_id, frame_seq, binding, version);
            if retain_version {
                self.versions.insert(key, (version, instance.clone(), value.clone()));
            }
            return instance;
        }
        let instance = mint_value_id(&self.execution_id, frame_seq, binding, 0);
        if retain_version {
            self.versions.insert(key, (0, instance.clone(), value.clone()));
        }
        instance
    }

    /// Live-view instance lookup without retention.
    pub(crate) fn live_instance(
        &mut self,
        frame_seq: u64,
        binding: &str,
        value: &Value,
    ) -> (SemanticId, u64) {
        let instance = self.instance_identity(frame_seq, binding, value, true);
        let version = self.versions.get(&(frame_seq, binding.to_owned())).map(|entry| entry.0).unwrap_or(0);
        (instance, version)
    }

    pub(crate) fn type_name(&self, binding: &str, value: &Value) -> String {
        self.type_names
            .get(binding)
            .cloned()
            .unwrap_or_else(|| value.kind_tag().to_owned())
    }

    fn frame_id(&self, depth: usize) -> SemanticId {
        self.frame_ids.get(depth).cloned().unwrap_or_else(|| SemanticId("mncs:vm:frame:unknown".to_owned()))
    }

    /// Execution entry: mint the root frame and emit `execution_enter`
    /// plus the root `frame_enter`.
    pub(crate) fn note_execution_enter(
        &mut self,
        entry_function: usize,
        arguments: &[(String, Value)],
    ) {
        let root = SemanticId(format!("mncs:vm:frame:{}:0", self.frame_prefix));
        self.frame_ids.push(root.clone());
        let function_identity = self.module.functions[entry_function].identity.0.clone();
        if self.captures_events() {
            self.push_event(
                "execution_enter",
                Some(root.clone()),
                None,
                None,
                Vec::new(),
                Vec::new(),
                None,
                None,
                Some("running".to_owned()),
                false,
            );
        }
        let mut inputs = Vec::new();
        for (binding, value) in arguments {
            if let Some(instance) =
                self.capture_value(0, binding, "argument", None, value)
            {
                inputs.push(instance);
            }
        }
        if self.captures_events() {
            self.stream.frames.push(ExecutionObservedFrame {
                identity: root.clone(),
                function: SemanticId(function_identity),
                parent: None,
                call_operation: None,
                depth: 0,
                arguments: inputs.clone(),
            });
            self.push_event(
                "frame_enter",
                Some(root),
                None,
                None,
                inputs,
                Vec::new(),
                None,
                None,
                Some("running".to_owned()),
                false,
            );
        }
    }

    /// Block entry: `block_enter`.
    pub(crate) fn note_block_enter(&mut self, depth: usize, block: &str) {
        if !self.captures_events() {
            return;
        }
        self.push_event(
            "block_enter",
            Some(self.frame_id(depth)),
            Some(SemanticId(block.to_owned())),
            None,
            Vec::new(),
            Vec::new(),
            None,
            None,
            Some("running".to_owned()),
            false,
        );
    }

    /// Evaluate stop conditions at an operation safe point. Returns the
    /// matched reasons (possibly empty). Also records position and
    /// emits `operation_enter` with input captures (once per actual
    /// transition; resume re-fires evaluate nothing twice).
    ///
    /// On a `HostCall` instruction, armed effect-before stops merge
    /// into this evaluation so one arrival suspends once instead of
    /// stopping separately at the operation and the effect boundary.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn before_operation(
        &mut self,
        frame_seq: u64,
        depth: usize,
        ip: usize,
        function_index: usize,
        block: &str,
        instruction: &mncs_model::SsaInstruction,
        values: &BTreeMap<String, Value>,
    ) -> Vec<StopReason> {
        self.position_function = function_index;
        self.position_block = block.to_owned();
        self.position_instruction = Some(instruction.identity.0.clone());
        self.position_semantic = instruction.semantic_identity.as_ref().map(|id| id.0.clone());
        if self.emit_enter_arrived(frame_seq, block, ip) {
            // Capture input instances only when the event or its values
            // are actually retained; otherwise both the minted identities
            // and the event are discarded, and live views mint on demand.
            // (capture_value retains nothing when this is false, so the
            // versions map is untouched either way.)
            let operation = SemanticId(instruction.identity.0.clone());
            let mut inputs = Vec::new();
            if self.retains_values_for(Some(&operation.0), false) {
                for input in &instruction.inputs {
                    if let Some(value) = values.get(&input.0) {
                        if let Some(instance) =
                            self.capture_value(frame_seq, &input.0, "input", Some(&operation), value)
                        {
                            inputs.push(instance);
                        }
                    }
                }
            }
            if self.captures_events() {
                self.push_event(
                    "operation_enter",
                    Some(self.frame_id(depth)),
                    Some(SemanticId(block.to_owned())),
                    Some(operation.clone()),
                    inputs,
                    Vec::new(),
                    None,
                    None,
                    Some("running".to_owned()),
                    false,
                );
            }
        }
        if !self.stop_arrived(frame_seq, block, ip, "operation") {
            return Vec::new();
        }
        let mut reasons = self.evaluate_operation_stops(depth, instruction);
        if matches!(
            instruction.kind,
            mncs_model::SsaInstructionKind::HostCall { .. }
        ) && self.stop_arrived(frame_seq, block, ip, "effect_before")
        {
            reasons.extend(self.effect_before_reasons());
        }
        reasons
    }

    /// `operation_result` with output captures after an instruction.
    pub(crate) fn after_operation(
        &mut self,
        frame_seq: u64,
        depth: usize,
        block: &str,
        instruction: &mncs_model::SsaInstruction,
        values: &BTreeMap<String, Value>,
    ) {
        if !self.captures_events() {
            return;
        }
        let operation = SemanticId(instruction.identity.0.clone());
        let mut outputs = Vec::new();
        for output in &instruction.outputs {
            if let Some(value) = values.get(&output.identity.0) {
                if let Some(instance) =
                    self.capture_value(frame_seq, &output.identity.0, "output", Some(&operation), value)
                {
                    outputs.push(instance);
                }
            }
        }
        self.push_event(
            "operation_result",
            Some(self.frame_id(depth)),
            Some(SemanticId(block.to_owned())),
            Some(operation),
            Vec::new(),
            outputs,
            None,
            None,
            Some("running".to_owned()),
            false,
        );
    }

    /// Successful call entry: mint the callee frame, capture its
    /// arguments, emit `frame_enter`.
    pub(crate) fn note_call_enter(
        &mut self,
        call_operation: &str,
        callee_function: usize,
        callee_seq: u64,
        depth: usize,
        arguments: &[(String, Value)],
    ) {
        let callee = SemanticId(format!("mncs:vm:frame:{}:{callee_seq}", self.frame_prefix));
        self.frame_ids.push(callee.clone());
        let function_identity = self.module.functions[callee_function].identity.0.clone();
        let mut inputs = Vec::new();
        // Argument instances feed retained frames only; live views mint
        // on demand. (capture_value retains nothing when capture is off,
        // so skipping here leaves the versions map untouched.)
        if self.captures_events() {
            let operation = SemanticId(call_operation.to_owned());
            for (binding, value) in arguments {
                if let Some(instance) =
                    self.capture_value(callee_seq, binding, "argument", Some(&operation), value)
                {
                    inputs.push(instance);
                }
            }
        }
        if self.captures_events() {
            self.stream.frames.push(ExecutionObservedFrame {
                identity: callee.clone(),
                function: SemanticId(function_identity),
                parent: self.frame_ids.get(depth.saturating_sub(1)).cloned(),
                call_operation: Some(SemanticId(call_operation.to_owned())),
                depth: depth as u64,
                arguments: inputs.clone(),
            });
            self.push_event(
                "frame_enter",
                Some(callee),
                None,
                None,
                inputs,
                Vec::new(),
                None,
                None,
                Some("running".to_owned()),
                false,
            );
        }
    }

    /// Return unwound one frame: `return` plus `frame_exit`. Caller
    /// binds the output after this hook; the returned values are
    /// captured from the callee frame before it is dropped.
    pub(crate) fn note_return(
        &mut self,
        callee_seq: u64,
        returned: &[(String, Value)],
        status: &str,
    ) {
        let callee = self.frame_ids.pop().unwrap_or_else(|| SemanticId("mncs:vm:frame:unknown".to_owned()));
        if !self.captures_events() {
            return;
        }
        let mut outputs = Vec::new();
        for (binding, value) in returned {
            if let Some(instance) = self.capture_value(callee_seq, binding, "returned", None, value)
            {
                outputs.push(instance);
            }
        }
        self.push_event(
            "return",
            Some(callee.clone()),
            None,
            None,
            Vec::new(),
            outputs,
            None,
            None,
            Some(status.to_owned()),
            false,
        );
        self.push_event(
            "frame_exit",
            Some(callee),
            None,
            None,
            Vec::new(),
            Vec::new(),
            None,
            None,
            Some(status.to_owned()),
            false,
        );
    }

    /// Accumulated safe-point kinds covered when suspending at one
    /// hook: hooks fire in position order (operation, effect_before,
    /// effect_after), and a resume must skip every hook at or before
    /// the suspending one so stops at one instruction never ping-pong.
    pub(crate) fn suspend_kinds(hook: &str, reasons: &[StopReason]) -> Vec<String> {
        let mut kinds = vec!["operation".to_owned()];
        let merged_before = reasons.iter().any(|reason| {
            matches!(
                reason,
                StopReason::EffectBoundary { phase, .. } if phase == "before"
            )
        });
        if hook == "operation" && !merged_before {
            return kinds[..1].to_vec();
        }
        kinds.push("effect_before".to_owned());
        if hook == "effect_after" {
            kinds.push("effect_after".to_owned());
        }
        kinds
    }

    /// Effect invocation: `effect_invoke` before dispatch. Returns
    /// matched stop reasons for the `effect_before` safe point. This
    /// fires only when the op hook did not already suspend (normally
    /// effect-before reasons merge there): mid-run bindings and
    /// resumed positions reach this hook.
    pub(crate) fn before_effect(
        &mut self,
        frame_seq: u64,
        depth: usize,
        ip: usize,
        block: &str,
        instruction: &mncs_model::SsaInstruction,
        request: &crate::capability::EffectRequest,
    ) -> Vec<StopReason> {
        let operation = SemanticId(instruction.identity.0.clone());
        if self.emit_invoke_arrived(frame_seq, block, ip) {
            let effect_sequence = self.next_effect;
            self.next_effect = self.next_effect.saturating_add(1);
            if self.captures_events() {
                let identity = SemanticId(format!(
                    "mncs:vm:effect:{}:{effect_sequence}",
                    self.frame_prefix
                ));
                self.stream.effects.push(ExecutionObservedEffect {
                    identity: identity.clone(),
                    operation: operation.clone(),
                    frame: self.frame_id(depth),
                    kind: request.operation.clone(),
                    target: request.operation.clone(),
                    capability: request.capability.clone(),
                    input_values: Vec::new(),
                    result_value: None,
                    provenance: None,
                    replayability: "provider-bound".to_owned(),
                    status: "invoked".to_owned(),
                });
                self.push_event(
                    "effect_invoke",
                    Some(self.frame_id(depth)),
                    Some(SemanticId(block.to_owned())),
                    Some(operation),
                    Vec::new(),
                    Vec::new(),
                    Some(identity),
                    None,
                    Some("invoked".to_owned()),
                    false,
                );
            }
        }
        if !self.stop_arrived(frame_seq, block, ip, "effect_before") {
            return Vec::new();
        }
        self.effect_before_reasons()
    }

    /// `effect_result` after dispatch, plus `effect_after` stops.
    /// Never re-fires: suspending here advances past the instruction
    /// first, so resume cannot dispatch the effect twice.
    pub(crate) fn after_effect(
        &mut self,
        depth: usize,
        block: &str,
        instruction: &mncs_model::SsaInstruction,
        status: &str,
    ) -> Vec<StopReason> {
        if self.captures_events() {
            let operation = SemanticId(instruction.identity.0.clone());
            self.push_event(
                "effect_result",
                Some(self.frame_id(depth)),
                Some(SemanticId(block.to_owned())),
                Some(operation),
                Vec::new(),
                Vec::new(),
                None,
                None,
                Some(status.to_owned()),
                false,
            );
        }
        self.effect_after_reasons().into_iter()
            .collect()
    }

    /// Record-only effect observation (no external boundary crossed):
    /// paired invoke/result with `recorded` status. Never suspends.
    pub(crate) fn note_recorded_effect(
        &mut self,
        depth: usize,
        block: &str,
        instruction: &mncs_model::SsaInstruction,
        operation_name: &str,
    ) {
        if !self.captures_events() {
            return;
        }
        let operation = SemanticId(instruction.identity.0.clone());
        let effect_sequence = self.next_effect;
        self.next_effect = self.next_effect.saturating_add(1);
        let identity = SemanticId(format!(
            "mncs:vm:effect:{}:{effect_sequence}",
            self.frame_prefix
        ));
        self.stream.effects.push(ExecutionObservedEffect {
            identity: identity.clone(),
            operation: operation.clone(),
            frame: self.frame_id(depth),
            kind: operation_name.to_owned(),
            target: operation_name.to_owned(),
            capability: String::new(),
            input_values: Vec::new(),
            result_value: None,
            provenance: None,
            replayability: "replayable".to_owned(),
            status: "recorded".to_owned(),
        });
        self.push_event(
            "effect_invoke",
            Some(self.frame_id(depth)),
            Some(SemanticId(block.to_owned())),
            Some(operation.clone()),
            Vec::new(),
            Vec::new(),
            Some(identity),
            None,
            Some("recorded".to_owned()),
            false,
        );
        self.push_event(
            "effect_result",
            Some(self.frame_id(depth)),
            Some(SemanticId(block.to_owned())),
            Some(operation),
            Vec::new(),
            Vec::new(),
            None,
            None,
            Some("recorded".to_owned()),
            false,
        );
    }

    /// Evaluate persistent + one-shot stops at an operation safe point.
    fn evaluate_operation_stops(
        &mut self,
        depth: usize,
        instruction: &mncs_model::SsaInstruction,
    ) -> Vec<StopReason> {
        let mut reasons = Vec::new();
        // Indexed: only stops bound to this instruction are visited;
        // positions ascend, so reason order matches the flat scan.
        if let Some(positions) = self.stop_indexes.by_operation.get(&instruction.identity.0) {
            for position in positions {
                reasons.push(StopReason::Breakpoint {
                    condition: self.config.stops[*position].id.clone(),
                });
            }
        }
        if let Some(one_shot) = self.one_shot.take() {
            match one_shot {
                OneShot::NextOperation => reasons.push(StopReason::Step),
                OneShot::Over { max_depth } if depth <= max_depth => {
                    reasons.push(StopReason::StepOver);
                }
                OneShot::Over { max_depth } => {
                    self.one_shot = Some(OneShot::Over { max_depth });
                }
                OneShot::Out { depth: entry } if depth < entry => {
                    reasons.push(StopReason::StepOut);
                }
                one_shot => {
                    self.one_shot = Some(one_shot);
                }
            }
        }
        reasons
    }

    /// Evaluate function-entry stops at a call hook.
    pub(crate) fn evaluate_function_entry(&self, function_index: usize) -> Vec<StopReason> {
        let function = &self.module.functions[function_index];
        // A target matches either identity; merge both hit lists in
        // ascending position order (deduped) to match the flat scan.
        let mut positions: Vec<usize> = Vec::new();
        if let Some(hits) = self.stop_indexes.by_function.get(&function.identity.0) {
            positions.extend(hits.iter().copied());
        }
        if function.semantic_identity.0 != function.identity.0 {
            if let Some(hits) = self.stop_indexes.by_function.get(&function.semantic_identity.0) {
                positions.extend(hits.iter().copied());
            }
        }
        positions.sort_unstable();
        positions.dedup();
        positions
            .iter()
            .map(|position| StopReason::FunctionEntry {
                condition: self.config.stops[*position].id.clone(),
            })
            .collect()
    }

    /// Terminal boundary: emit `failure`/`return`, unwind remaining
    /// frames with `frame_exit`, then `execution_exit`. Returns
    /// whether an abnormal terminal should suspend before finalizing.
    pub(crate) fn note_terminal(&mut self, outcome: &Outcome) -> bool {
        let failure_class = !outcome.is_completed();
        if self.captures_events() {
            let frame = self.frame_ids.last().cloned();
            match outcome {
                Outcome::Completed => {
                    self.push_event(
                        "return",
                        frame.clone(),
                        None,
                        None,
                        Vec::new(),
                        Vec::new(),
                        None,
                        None,
                        Some("completed".to_owned()),
                        false,
                    );
                }
                _ => {
                    self.push_event(
                        "failure",
                        frame.clone(),
                        None,
                        None,
                        Vec::new(),
                        Vec::new(),
                        None,
                        None,
                        Some(outcome.tag().to_owned()),
                        true,
                    );
                }
            }
            while let Some(exiting) = self.frame_ids.pop() {
                self.push_event(
                    "frame_exit",
                    Some(exiting),
                    None,
                    None,
                    Vec::new(),
                    Vec::new(),
                    None,
                    None,
                    Some(outcome.tag().to_owned()),
                    false,
                );
            }
            self.push_event(
                "execution_exit",
                frame,
                None,
                None,
                Vec::new(),
                Vec::new(),
                None,
                None,
                Some(outcome.tag().to_owned()),
                false,
            );
        } else {
            self.frame_ids.clear();
        }
        failure_class && self.config.stop_on_abnormal_terminal
    }

    pub(crate) fn bind_stop(&mut self, target: StopTarget, id: Option<String>) -> StopCondition {
        let id = id.unwrap_or_else(|| {
            let minted = format!("stop:{}", self.next_stop_mint);
            self.next_stop_mint += 1;
            minted
        });
        let condition = StopCondition { id, target };
        self.config.stops.push(condition.clone());
        self.stop_indexes = StopIndexes::rebuild(&self.config.stops);
        condition
    }

    pub(crate) fn clear_stop(&mut self, id: &str) -> bool {
        let before = self.config.stops.len();
        self.config.stops.retain(|condition| condition.id != id);
        let changed = self.config.stops.len() != before;
        if changed {
            self.stop_indexes = StopIndexes::rebuild(&self.config.stops);
        }
        changed
    }

    pub(crate) fn arm_one_shot(&mut self, one_shot: OneShot) {
        self.one_shot = Some(one_shot);
    }

    pub(crate) fn counts(&self) -> ObservationCounts {
        ObservationCounts {
            events: self.stream.events.len(),
            values: self.stream.values.len(),
            effects: self.stream.effects.len(),
            dropped_events: self.dropped_events,
            dropped_values: self.dropped_values,
            dropped_effects: self.dropped_effects,
        }
    }

    /// Whether this safe point is a fresh arrival. A resume leaves
    /// the stop position behind: stops at the suspended
    /// (position, kind) do not re-fire until execution arrives
    /// somewhere new. Without this, resuming from a bound operation
    /// would suspend again immediately instead of executing it.
    pub(crate) fn stop_arrived(&self, frame_seq: u64, block: &str, ip: usize, kind: &str) -> bool {
        match &self.last_stop {
            None => true,
            Some((position, kinds)) => {
                position != &(frame_seq, block.to_owned(), ip) || !kinds.iter().any(|name| name == kind)
            }
        }
    }

    /// Record the suspended (position, kinds) after a stop. Kinds
    /// accumulate along the hook order at one position
    /// (operation, effect_before, effect_after) so a resume never
    /// ping-pongs between stops at the same instruction.
    pub(crate) fn note_suspended(&mut self, frame_seq: u64, block: String, ip: usize, kinds: Vec<String>) {
        self.last_stop = Some(((frame_seq, block, ip), kinds));
    }

    /// Whether to emit `operation_enter` for this position. Resume
    /// re-fires the op hook at the stop position; emission happens
    /// once per actual transition.
    fn emit_enter_arrived(&mut self, frame_seq: u64, block: &str, ip: usize) -> bool {
        let key = (frame_seq, block.to_owned(), ip);
        if self.emit_enter.as_ref() == Some(&key) {
            return false;
        }
        self.emit_enter = Some(key);
        true
    }

    /// Whether to emit `effect_invoke` for this position (same resume
    /// re-fire reason as [`DebugDriver::emit_enter_arrived`]).
    fn emit_invoke_arrived(&mut self, frame_seq: u64, block: &str, ip: usize) -> bool {
        let key = (frame_seq, block.to_owned(), ip);
        if self.emit_invoke.as_ref() == Some(&key) {
            return false;
        }
        self.emit_invoke = Some(key);
        true
    }

    /// Evaluate operation + one-shot stops for one instruction.
    /// Public within the crate so call-entry suspension can merge the
    /// callee's first-operation reasons into one stop.
    pub(crate) fn evaluate_stops_for_instruction(
        &mut self,
        depth: usize,
        instruction: &mncs_model::SsaInstruction,
    ) -> Vec<StopReason> {
        self.evaluate_operation_stops(depth, instruction)
    }

    /// Persistent stops matching an abnormal terminal boundary.
    pub(crate) fn failure_stop_reasons(&self) -> Vec<StopReason> {
        self.stop_indexes
            .failure
            .iter()
            .map(|position| StopReason::Breakpoint {
                condition: self.config.stops[*position].id.clone(),
            })
            .collect()
    }

    /// Persistent effect-before stops, for merging into an operation
    /// stop on a HostCall instruction so one arrival suspends once.
    pub(crate) fn effect_before_reasons(&self) -> Vec<StopReason> {
        self.stop_indexes
            .effect_before
            .iter()
            .map(|position| StopReason::EffectBoundary {
                condition: self.config.stops[*position].id.clone(),
                phase: "before".to_owned(),
            })
            .collect()
    }

    pub(crate) fn effect_after_reasons(&self) -> Vec<StopReason> {
        self.stop_indexes
            .effect_after
            .iter()
            .map(|position| StopReason::EffectBoundary {
                condition: self.config.stops[*position].id.clone(),
                phase: "after".to_owned(),
            })
            .collect()
    }

    /// Position the driver at callee entry (used when suspending on a
    /// function-entry stop before the first operation hook runs).
    pub(crate) fn note_function_entry_position(
        &mut self,
        callee_function: usize,
        entry_block: &str,
        first: Option<(String, Option<String>)>,
    ) {
        self.position_function = callee_function;
        self.position_block = entry_block.to_owned();
        self.position_instruction = first.as_ref().map(|pair| pair.0.clone());
        self.position_semantic = first.and_then(|pair| pair.1);
        // A fresh frame means a fresh arrival even when the caller's
        // stop shares the shape: entry stops always fire.
        self.last_stop = None;
    }

    pub(crate) fn set_block_prefetched(&mut self) {
        self.block_prefetched = true;
    }

    pub(crate) fn take_block_prefetched(&mut self) -> bool {
        std::mem::replace(&mut self.block_prefetched, false)
    }

    /// Build an operation safe point for the recorded position.
    pub(crate) fn operation_safe_point(&self, frame_seq: u64, depth: usize, steps: u64) -> SafePoint {
        let function = &self.module.functions[self.position_function];
        SafePoint {
            kind: "operation".to_owned(),
            execution: self.execution_id.clone(),
            frame: format!("mncs:vm:frame:{}:{frame_seq}", self.frame_prefix),
            depth: depth as u64,
            function: function.identity.0.clone(),
            function_semantic: function.semantic_identity.0.clone(),
            block: self.position_block.clone(),
            instruction: self.position_instruction.clone(),
            instruction_semantic: self.position_semantic.clone(),
            step_index: steps,
        }
    }

    /// Build an effect safe point for the recorded position.
    pub(crate) fn effect_safe_point(&self, frame_seq: u64, depth: usize, steps: u64, phase: &str) -> SafePoint {
        let mut point = self.operation_safe_point(frame_seq, depth, steps);
        point.kind = if phase == "before" {
            "effect_before".to_owned()
        } else {
            "effect_after".to_owned()
        };
        point
    }

    /// Build a terminal safe point for the recorded position.
    pub(crate) fn terminal_safe_point(&self, frame_seq: u64, depth: usize, steps: u64) -> SafePoint {
        let mut point = self.operation_safe_point(frame_seq, depth, steps);
        point.kind = "terminal".to_owned();
        point
    }

    /// Snapshot the stream without consuming the driver (bounded).
    pub(crate) fn stream_snapshot(&self) -> ExecutionObservationStream {
        let mut stream = self.stream.clone();
        let truncated = self.dropped_events > 0 || self.dropped_values > 0 || self.dropped_effects > 0;
        stream.completeness = ExecutionObservationCompleteness {
            status: if matches!(self.capture(), ObservationCapturePolicy::None) {
                ObservationCompletenessStatus::Disabled
            } else if truncated {
                ObservationCompletenessStatus::Truncated
            } else {
                ObservationCompletenessStatus::Complete
            },
            captured_events: stream.events.len(),
            dropped_events: self.dropped_events,
            captured_values: stream.values.len(),
            dropped_values: self.dropped_values,
            captured_effects: stream.effects.len(),
            truncated,
        };
        stream
    }
}

/// How a debugged call starts: stopped at the first bound condition
/// (or terminal boundary), or already finished.
pub enum DebugStart<'a> {
    Stopped(Box<LiveExecution<'a>>, Box<StopRecord>),
    Finished(Box<FinishRecord>),
}

/// A live debugged execution: the engine plus its run state plus the
/// debug driver. Single-owner (`&mut self` everywhere): one holder
/// resumes, inspects, and terminates.
pub struct LiveExecution<'a> {
    engine: Engine<'a>,
    parts: LiveParts,
    driver: DebugDriver,
    execution_id: String,
    artifact_id: String,
    callable_function: String,
    callable_name: String,
    arguments: Vec<Value>,
    admitted_capabilities: Vec<String>,
    resource_limits: Vec<crate::resource::ResourceLimit>,
    stop_sequence: u64,
    current_token: String,
    token_salt: u64,
    pending_terminal: Option<Outcome>,
    finished: bool,
    finished_outcome: Option<Outcome>,
    entered: bool,
}

impl<'a> LiveExecution<'a> {
    // Constructor assembles independently owned parts; each argument
    // is used exactly once.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        engine: Engine<'a>,
        parts: LiveParts,
        driver: DebugDriver,
        execution_id: String,
        artifact_id: String,
        callable_function: String,
        callable_name: String,
        arguments: Vec<Value>,
        admitted_capabilities: Vec<String>,
        resource_limits: Vec<crate::resource::ResourceLimit>,
    ) -> Self {
        Self {
            engine,
            parts,
            driver,
            execution_id,
            artifact_id,
            callable_function,
            callable_name,
            arguments,
            admitted_capabilities,
            resource_limits,
            stop_sequence: 0,
            current_token: String::new(),
            token_salt: token_salt(),
            pending_terminal: None,
            finished: false,
            finished_outcome: None,
            entered: false,
        }
    }

    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }

    pub fn stop_sequence(&self) -> u64 {
        self.stop_sequence
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Resume until the next bound stop, terminal boundary, or finish.
    /// Consumes the current continuation token.
    pub fn resume(&mut self, token: &str) -> Result<LiveEvent, DebugError> {
        self.check_token(token)?;
        self.drive()
    }

    /// Alias naming the intent: run until another stop or finish.
    pub fn continue_execution(&mut self, token: &str) -> Result<LiveEvent, DebugError> {
        self.resume(token)
    }

    /// Step to the next observable semantic transition (steps into calls).
    pub fn step_in(&mut self, token: &str) -> Result<LiveEvent, DebugError> {
        self.check_token(token)?;
        self.driver.arm_one_shot(OneShot::NextOperation);
        self.drive()
    }

    /// Step beyond the current operation without stopping in nested callees.
    pub fn step_over(&mut self, token: &str) -> Result<LiveEvent, DebugError> {
        self.check_token(token)?;
        let depth = self.parts.stack.len();
        self.driver.arm_one_shot(OneShot::Over { max_depth: depth });
        self.drive()
    }

    /// Run until the current frame returns (or the program finishes).
    pub fn step_out(&mut self, token: &str) -> Result<LiveEvent, DebugError> {
        self.check_token(token)?;
        let depth = self.parts.stack.len();
        self.driver.arm_one_shot(OneShot::Out { depth });
        self.drive()
    }

    /// Terminate the execution. Finalizes the pending terminal outcome
    /// when stopped on a terminal boundary, otherwise records an
    /// explicit `Terminated` outcome. Inspectable state is retained.
    pub fn terminate(&mut self, token: &str) -> Result<FinishRecord, DebugError> {
        self.check_token(token)?;
        if self.finished {
            return Err(DebugError::AlreadyFinished);
        }
        let outcome = self.pending_terminal.clone().unwrap_or(Outcome::Terminated {
            detail: format!("terminated by debugger at stop {}", self.stop_sequence),
        });
        // A mid-run terminate still closes the observation stream
        // honestly: the terminal hook runs once for event closure.
        if self.pending_terminal.is_none() {
            self.driver.note_terminal(&outcome);
        }
        Ok(self.finish(outcome))
    }

    /// Bind a stop condition before or during execution.
    pub fn bind_stop(&mut self, target: StopTarget, id: Option<String>) -> Result<StopCondition, DebugError> {
        if self.finished {
            return Err(DebugError::AlreadyFinished);
        }
        validate_stop_target(&target)?;
        Ok(self.driver.bind_stop(target, id))
    }

    pub fn clear_stop(&mut self, id: &str) -> Result<bool, DebugError> {
        if self.finished {
            return Err(DebugError::AlreadyFinished);
        }
        Ok(self.driver.clear_stop(id))
    }

    pub fn list_stops(&self) -> Vec<StopCondition> {
        self.driver.config.stops.clone()
    }

    /// Bounded typed stack view. Read-only: repeated inspection of
    /// unchanged stopped state is cheap (no re-execution, no stream
    /// retention).
    pub fn inspect_stack(&mut self, max_frames: usize, max_values: usize, max_value_bytes: usize) -> Result<StackView, DebugError> {
        if max_frames == 0 {
            return Err(DebugError::InspectionBound {
                detail: "max_frames must be positive".to_owned(),
            });
        }
        // Disjoint field borrows: values are read from `parts`
        // while instance identities mint through `driver`.
        let driver = &mut self.driver;
        let parts = &self.parts;
        let execution_id = &self.execution_id;
        let mut frames = Vec::new();
        // Current frame first, then callers outward.
        frames.push(frame_view(
            driver,
            execution_id,
            &parts.frame,
            parts.stack.len(),
            max_values,
            max_value_bytes,
        ));
        let depth = parts.stack.len();
        for (index, caller) in parts.stack.iter().rev().take(max_frames.saturating_sub(1)).enumerate() {
            frames.push(frame_view(
                driver,
                execution_id,
                &caller.frame,
                depth - 1 - index,
                max_values,
                max_value_bytes,
            ));
        }
        Ok(StackView {
            execution: self.execution_id.clone(),
            stop_sequence: self.stop_sequence,
            frames,
        })
    }

    /// Current observation stream snapshot (retained evidence so far).
    pub fn inspect_observation(&self) -> ExecutionObservationStream {
        self.driver.stream_snapshot()
    }

    /// Retained-effect log so far (effect attempts and results).
    pub fn inspect_effects(&self) -> Vec<EffectObservation> {
        self.parts.state.effects.clone()
    }

    /// Read-only token check for session daemons that gate
    /// concurrent clients. The live handle stays the token authority;
    /// mutating operations check tokens themselves.
    pub fn check_token_public(&self, token: &str) -> bool {
        self.check_token(token).is_ok()
    }

    fn check_token(&self, token: &str) -> Result<(), DebugError> {
        if self.finished {
            return Err(DebugError::AlreadyFinished);
        }
        if token != self.current_token {
            // Distinguish stale (well-formed, older sequence) from bad.
            if let Some(sequence) = parse_token_sequence(token) {
                if sequence < self.stop_sequence && token_stem(token) == token_stem(&self.current_token) {
                    return Err(DebugError::StaleToken {
                        expected_sequence: self.stop_sequence,
                    });
                }
                let _ = sequence;
            }
            // Cross-execution substitution fails closed here: the stem
            // binds the execution identity.
            if token_stem(token) != token_stem(&self.current_token) {
                return Err(DebugError::BadToken {
                    detail: "token names a different execution".to_owned(),
                });
            }
            return Err(DebugError::StaleToken {
                expected_sequence: self.stop_sequence,
            });
        }
        Ok(())
    }

    fn mint_token(&mut self) -> String {
        self.stop_sequence += 1;
        let material = serde_json::json!({
            "execution": self.execution_id,
            "stop": self.stop_sequence,
            "salt": self.token_salt,
        });
        let bytes = serde_json::to_vec(&material).expect("token material serializes");
        let token = format!(
            "dbg:{}:{}:{}",
            self.driver.frame_prefix,
            self.stop_sequence,
            short_digest_hex(&bytes)
        );
        self.current_token = token.clone();
        token
    }

    /// First drive: emits execution entry, then runs to the first
    /// stop or finish. Later drives continue the same run state.
    pub(crate) fn drive_first(&mut self) -> LiveEvent {
        if !self.entered {
            self.entered = true;
            let entry_function = self.parts.frame.function;
            let arguments: Vec<(String, Value)> = self
                .parts
                .frame
                .values
                .iter()
                .map(|(binding, value)| (binding.clone(), value.clone()))
                .collect();
            self.driver.note_execution_enter(entry_function, &arguments);
        }
        self.drive_internal()
    }

    fn drive(&mut self) -> Result<LiveEvent, DebugError> {
        if self.finished {
            return Err(DebugError::AlreadyFinished);
        }
        if let Some(outcome) = &self.pending_terminal {
            return Err(DebugError::ResumeRefusedTerminal {
                outcome: outcome.tag().to_owned(),
            });
        }
        Ok(self.drive_internal())
    }

    fn drive_internal(&mut self) -> LiveEvent {
        match self.engine.drive(&mut self.parts, Some(&mut self.driver)) {
            DriveExit::Finished(outcome) => LiveEvent::Finished(Box::new(self.finish(outcome))),
            DriveExit::Suspended(request) => {
                self.driver.note_suspended(
                    request.position.0,
                    request.position.1.clone(),
                    request.position.2,
                    request.kinds.clone(),
                );
                let token = self.mint_token();
                let state_digest = state_digest(&self.execution_id, &self.parts);
                let transition_digest = transition_digest(
                    &self.execution_id,
                    &self.parts.state,
                    &request.safe_point,
                    &state_digest,
                );
                if request.pending_terminal.is_some() {
                    self.pending_terminal = request.pending_terminal.clone();
                }
                LiveEvent::Stopped(Box::new(StopRecord {
                    schema_version: DEBUG_SCHEMA_VERSION.to_owned(),
                    execution: self.execution_id.clone(),
                    artifact: self.artifact_id.clone(),
                    callable_function: self.callable_function.clone(),
                    stop_sequence: self.stop_sequence,
                    continuation_token: token,
                    reasons: request.reasons,
                    safe_point: request.safe_point,
                    usage: self.parts.state.usage.clone(),
                    state_digest,
                    transition_digest,
                    observations: self.driver.counts(),
                    pending_terminal: request.pending_terminal.clone(),
                }))
            }
        }
    }

    /// Finish an entry that failed before any transition. Entry
    /// already unwound its own call accounting; nothing executes.
    pub(crate) fn finish_invalid(&mut self, outcome: Outcome) -> FinishRecord {
        self.finish_internal(outcome, false)
    }

    fn finish(&mut self, outcome: Outcome) -> FinishRecord {
        // Mirror one-shot accounting: the entry call exits exactly once.
        self.finish_internal(outcome, true)
    }

    fn finish_internal(&mut self, outcome: Outcome, exit_call: bool) -> FinishRecord {
        self.finished = true;
        self.finished_outcome = Some(outcome.clone());
        if exit_call {
            self.parts.state.usage.exit_call();
        }
        // Snapshot (not consume): post-finish inspection keeps stable
        // value identities from the live version map.
        let stream = self.driver.stream_snapshot();
        let returned = self.parts.state.completed.clone();
        let return_digest =
            ExecutionRecord::digest_of(&returned.iter().map(to_wire).collect::<Vec<_>>());
        let effects_digest = ExecutionRecord::digest_of(&self.parts.state.effects);
        let record = ExecutionRecord {
            schema_version: "mncs.vm.execution-record/1".to_owned(),
            runtime_id: format!("mncs-vm-runtime:{}", self.artifact_id),
            artifact_id: self.artifact_id.clone(),
            callable_function: self.callable_function.clone(),
            callable_name: self.callable_name.clone(),
            arguments: self.arguments.clone(),
            admitted_capabilities: self.admitted_capabilities.clone(),
            resource_limits: self.resource_limits.clone(),
            outcome: outcome.clone(),
            returned,
            usage: self.parts.state.usage.clone(),
            effects: self.parts.state.effects.clone(),
            return_digest,
            effects_digest,
            observation: Some(stream.clone()),
        };
        FinishRecord { outcome, record, stream }
    }

}

fn validate_stop_target(target: &StopTarget) -> Result<(), DebugError> {
    match target {
        StopTarget::Operation { instruction } if instruction.trim().is_empty() => {
            Err(DebugError::InvalidStop {
                detail: "operation stop needs an instruction identity".to_owned(),
            })
        }
        StopTarget::Function { function } if function.trim().is_empty() => {
            Err(DebugError::InvalidStop {
                detail: "function stop needs a function identity".to_owned(),
            })
        }
        StopTarget::EffectBoundary { phase }
            if phase != "before" && phase != "after" && phase != "both" =>
        {
            Err(DebugError::InvalidStop {
                detail: "effect boundary phase must be before, after, or both".to_owned(),
            })
        }
        StopTarget::Operation { .. } | StopTarget::Function { .. } | StopTarget::EffectBoundary { .. } | StopTarget::FailureOrTrap => Ok(()),
    }
}

/// Empty observation stream for executions that never started
/// (invalid before entry). Disabled capture, honestly empty.
pub(crate) fn empty_stream() -> ExecutionObservationStream {
    ExecutionObservationStream {
        schema_version: mncs_model::EXECUTION_OBSERVATION_SCHEMA_VERSION.to_owned(),
        identity: SemanticId("mncs:vm:observation:unbound".to_owned()),
        execution_identity: SemanticId("mncs:vm:execution:unbound".to_owned()),
        policy: ExecutionObservationPolicy::default(),
        completeness: ExecutionObservationCompleteness {
            status: ObservationCompletenessStatus::Disabled,
            captured_events: 0,
            dropped_events: 0,
            captured_values: 0,
            dropped_values: 0,
            captured_effects: 0,
            truncated: false,
        },
        frames: Vec::new(),
        values: Vec::new(),
        effects: Vec::new(),
        events: Vec::new(),
    }
}

/// Deterministic execution identity: the same admitted artifact,
/// callable, arguments, capabilities, and envelope name the same
/// execution. Observation policy is excluded: one subject observed
/// twice keeps one identity.
pub fn execution_identity(
    artifact_id: &str,
    callable_function: &str,
    arguments: &[mncs_model::ExecutionValue],
    admitted_capabilities: &[String],
    envelope: &ResourceEnvelope,
) -> String {
    let mut capabilities = admitted_capabilities.to_vec();
    capabilities.sort();
    let material = serde_json::json!({
        "artifact": artifact_id,
        "callable": callable_function,
        "arguments": arguments,
        "capabilities": capabilities,
        "envelope": envelope.limits,
    });
    let bytes = serde_json::to_vec(&material).expect("execution identity serializes");
    format!("mncs:vm:execution:{}", full_digest_hex(&bytes))
}

fn state_digest(execution_id: &str, parts: &LiveParts) -> String {
    let mut stack = Vec::new();
    for caller in &parts.stack {
        stack.push(frame_material(&caller.frame));
    }
    stack.push(frame_material(&parts.frame));
    let material = serde_json::json!({
        "execution": execution_id,
        "stack": stack,
        "completed": parts.state.completed.iter().map(to_wire).collect::<Vec<_>>(),
    });
    let bytes = serde_json::to_vec(&material).expect("state material serializes");
    crate::artifact::artifact_id_of(&bytes)
}

fn frame_view(
    driver: &mut DebugDriver,
    execution_id: &str,
    frame: &crate::engine::Frame,
    depth: usize,
    max_values: usize,
    max_value_bytes: usize,
) -> FrameView {
    // Static position first (immutable module borrow ends before
    // instance minting needs the driver mutably).
    let (function_id, function_semantic, instruction, instruction_semantic) = {
        let module = &driver.module;
        let function = &module.functions[frame.function];
        let found = module.functions[frame.function]
            .blocks
            .iter()
            .find(|block| block.identity.0 == frame.block)
            .and_then(|block| block.instructions.get(frame.ip))
            .map(|instruction| {
                (
                    instruction.identity.0.clone(),
                    instruction
                        .semantic_identity
                        .as_ref()
                        .map(|id| id.0.clone()),
                )
            });
        (
            function.identity.0.clone(),
            function.semantic_identity.0.clone(),
            found.as_ref().map(|pair| pair.0.clone()),
            found.and_then(|pair| pair.1),
        )
    };
    let mut values = Vec::new();
    let mut truncated = false;
    for (binding, value) in frame.values.iter() {
        if values.len() >= max_values {
            truncated = true;
            break;
        }
        let (instance, version) = driver.live_instance(frame.seq, binding, value);
        let wire = to_wire(value);
        let bytes = serde_json::to_vec(&wire).unwrap_or_default();
        let capture = if bytes.len() <= max_value_bytes.max(1) && !bytes.is_empty() {
            ExecutionValueCapture::Full { value: wire }
        } else if bytes.is_empty() {
            ExecutionValueCapture::Unavailable {
                reason: "value did not serialize".to_owned(),
            }
        } else {
            ExecutionValueCapture::Truncated {
                sha256: crate::artifact::artifact_id_of(&bytes),
                bytes: bytes.len(),
            }
        };
        values.push(ValueView {
            binding: binding.clone(),
            instance: instance.0,
            version,
            kind: value.kind_tag().to_owned(),
            type_name: driver.type_name(binding, value),
            capture,
        });
    }
    FrameView {
        frame: format!(
            "mncs:vm:frame:{}:{}",
            short_digest(execution_id),
            frame.seq
        ),
        function: function_id,
        function_semantic,
        depth: depth as u64,
        block: frame.block.clone(),
        ip: frame.ip,
        instruction,
        instruction_semantic,
        values,
        truncated,
    }
}

fn frame_material(frame: &crate::engine::Frame) -> serde_json::Value {
    serde_json::json!({
        "function": frame.function,
        "block": frame.block,
        "ip": frame.ip,
        "values": frame.values.iter().map(|(binding, value)| (binding, to_wire(value))).collect::<BTreeMap<_, _>>(),
    })
}

fn transition_digest(
    execution_id: &str,
    state: &crate::engine::State,
    safe_point: &SafePoint,
    state_digest: &str,
) -> String {
    let effects_digest = ExecutionRecord::digest_of(&state.effects);
    let material = serde_json::json!({
        "execution": execution_id,
        "steps": state.usage.steps,
        "effects": effects_digest,
        "safe_point": safe_point,
        "state": state_digest,
    });
    let bytes = serde_json::to_vec(&material).expect("transition material serializes");
    crate::artifact::artifact_id_of(&bytes)
}

fn mint_value_id(execution_id: &str, frame_seq: u64, binding: &str, version: u64) -> SemanticId {
    // Borrowing material, byte-identical to the previous json! object
    // (keys alphabetical). Pinned by the derivation-stability test.
    #[derive(Serialize)]
    struct ValueMaterial<'a> {
        binding: &'a str,
        execution: &'a str,
        frame: u64,
        version: u64,
    }
    let material = ValueMaterial { binding, execution: execution_id, frame: frame_seq, version };
    let bytes = serde_json::to_vec(&material).expect("value identity serializes");
    SemanticId(format!("mncs:vm:value:{}", full_digest_hex(&bytes)))
}

fn full_digest_hex(bytes: &[u8]) -> String {
    crate::artifact::artifact_id_of(bytes)
        .strip_prefix("sha256:")
        .unwrap_or_default()
        .to_owned()
}

fn short_digest(text: &str) -> String {
    short_digest_hex(text.as_bytes())
}

fn short_digest_hex(bytes: &[u8]) -> String {
    full_digest_hex(bytes).chars().take(12).collect()
}

fn token_salt() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    nanos ^ (std::process::id() as u64).wrapping_mul(0x9E3779B97F4A7C15)
}

fn token_stem(token: &str) -> &str {
    // `dbg:<exec12>:<seq>:<nonce>`; the stem binds the execution.
    let mut parts = token.splitn(2, ':');
    let _ = parts.next();
    parts.next().unwrap_or_default().split(':').next().unwrap_or_default()
}

fn parse_token_sequence(token: &str) -> Option<u64> {
    let mut parts = token.split(':');
    let _ = parts.next()?;
    let _ = parts.next()?;
    parts.next()?.parse().ok()
}

/// Names of [`StopTarget`] variants for capability reporting.
pub fn stop_target_names() -> Vec<String> {
    ["operation", "function", "effect_boundary", "failure_or_trap"]
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

/// Event kinds the VM emits (shared `mncs.execution-observation/1` vocabulary).
pub fn observation_event_kinds() -> Vec<String> {
    [
        "execution_enter",
        "execution_exit",
        "frame_enter",
        "frame_exit",
        "block_enter",
        "operation_enter",
        "operation_result",
        "return",
        "failure",
        "effect_invoke",
        "effect_result",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect()
}

/// Capabilities the live-debug contract explicitly does NOT provide.
pub fn unsupported_debug_capabilities() -> Vec<BTreeMap<String, String>> {
    [
        ("live_watch_stop", "stop-on-value-change needs a watched-state relation the VM does not yet define; post-execution watch queries stay in mncs-debug"),
        ("branch_condition_stop", "no stop condition names a branch-taken decision; block_enter events show direction"),
        ("async_effect_suspension", "provider dispatch is synchronous in v1; there is no unresolved-effect state to suspend in"),
        ("expression_evaluation", "the debugger cannot evaluate new expressions against stopped state"),
        ("time_travel", "no reverse execution; replay is forward re-execution with recorded observations"),
        ("multi_execution", "one live execution per debug handle; no scheduler control"),
    ]
    .iter()
    .map(|(capability, reason)| {
        BTreeMap::from([
            ("capability".to_owned(), (*capability).to_owned()),
            ("reason".to_owned(), (*reason).to_owned()),
        ])
    })
    .collect()
}
