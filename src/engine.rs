//! Reference execution engine.
//!
//! A small, boring, inspectable interpreter over admitted SSA code.
//! The engine owns runtime mechanics — frames, control, metering,
//! capability dispatch, outcomes — while language-owned meaning
//! (integer evaluation, type shapes) is consumed through public
//! upstream contracts, never redefined.
//!
//! Accounting is VM-defined and stated exactly: one step per
//! executed instruction and one per taken terminator edge. Step
//! counts are evidence of engine work, not host CPU claims and not
//! comparable across engines.
//!
//! Supported subset (everything else is explicit `Unsupported`):
//! Constant, Integer, IntegerCompare, BooleanOp, BooleanCompare,
//! BooleanNot, ByteBitwise, ByteShift, ByteCompare, Select (scalar),
//! RecordConstruct, RecordProject, FiniteConstruct,
//! FinitePayloadProject, FiniteIsVariant, SequenceConstruct,
//! SequenceProject, SequenceLength, SequenceReplace, BoundCheck,
//! Convert (integer/boolean/byte totals), Call, Effect (recorded),
//! HostCall (provider-dispatched), Return, Branch,
//! ConditionalBranch, Failure. RuntimeCheck is Unsupported upstream
//! as well, so agreement there is exact.

use std::collections::BTreeMap;
use std::sync::Arc;

use mncs_model::{
    ArithmeticIntent, BodyType, IntegerOperation, IntegerType, SemanticId, SsaInstruction,
    SsaInstructionKind, SsaModule, SsaTerminator,
};

use crate::admit::Admitted;
use crate::capability::{CapabilityEnv, EffectRequest};
use crate::debug::{DebugDriver, DriveExit, StopReason, SuspendRequest};
use crate::evidence::EffectObservation;
use crate::outcome::Outcome;
use crate::resource::{ResourceEnvelope, ResourceUsage};
use crate::value::Value;

/// How the caller names the entry callable.
#[derive(Debug, Clone)]
pub enum CallTarget {
    ByName { module: String, name: String },
    ByFunction { function: String },
}

/// Engine product for one call.
#[derive(Debug, Clone)]
pub struct EngineResult {
    pub outcome: Outcome,
    pub returned: Vec<Value>,
    pub usage: ResourceUsage,
    pub effects: Vec<EffectObservation>,
}

/// One live frame.
pub(crate) struct Frame {
    pub function: usize,
    pub values: BTreeMap<String, Value>,
    pub block: String,
    pub iters: BTreeMap<String, u64>,
    pub ip: usize,
    /// Frame sequence within the execution (entry frame is 0).
    /// Stable for the frame's lifetime; used for value identity.
    pub seq: u64,
}

/// A pushed caller awaiting a callee return.
pub(crate) struct Caller {
    pub frame: Frame,
    pub output: Option<String>,
}

/// Mutable per-run state, split from [`Engine`] so SSA borrows and
/// state borrows never conflict.
pub(crate) struct State {
    pub usage: ResourceUsage,
    pub effects: Vec<EffectObservation>,
    pub effect_sequence: u64,
    pub completed: Vec<Value>,
    next_frame_seq: u64,
}

impl State {
    fn new() -> Self {
        Self {
            usage: ResourceUsage::default(),
            effects: Vec::new(),
            effect_sequence: 0,
            completed: Vec::new(),
            next_frame_seq: 1,
        }
    }

    fn alloc_frame_seq(&mut self) -> u64 {
        let seq = self.next_frame_seq;
        self.next_frame_seq = self.next_frame_seq.saturating_add(1);
        seq
    }
}

/// Everything the drive loop mutates. Plain data, so a debugged run
/// can suspend (return it to the holder) and resume (drive it again)
/// without re-execution or interpreter duplication.
pub(crate) struct LiveParts {
    pub state: State,
    pub frame: Frame,
    pub stack: Vec<Caller>,
}

pub struct Engine<'a> {
    admitted: &'a Admitted,
    module: &'a SsaModule,
    functions: BTreeMap<String, usize>,
    blocks: Vec<BTreeMap<String, usize>>,
    envelope: ResourceEnvelope,
    caps: &'a CapabilityEnv,
}

impl<'a> Engine<'a> {
    pub fn new(
        admitted: &'a Admitted,
        envelope: ResourceEnvelope,
        caps: &'a CapabilityEnv,
    ) -> Option<Self> {
        let module = admitted.ssa_module()?;
        let mut functions = BTreeMap::new();
        let mut blocks = Vec::new();
        for (index, function) in module.functions.iter().enumerate() {
            // Calls name the semantic identity while admission binds
            // the function identity; index both to the same code.
            functions.insert(function.identity.0.clone(), index);
            functions.insert(function.semantic_identity.0.clone(), index);
            let mut map = BTreeMap::new();
            for (block_index, block) in function.blocks.iter().enumerate() {
                map.insert(block.identity.0.clone(), block_index);
            }
            blocks.push(map);
        }
        Some(Self {
            admitted,
            module,
            functions,
            blocks,
            envelope,
            caps,
        })
    }

    /// Function index for a callable identity (function or semantic).
    pub(crate) fn function_index(&self, function: &str) -> Option<usize> {
        self.functions.get(function).copied()
    }

    pub fn run(self, target: CallTarget, arguments: Vec<Value>) -> EngineResult {
        let finish = |state: &State, outcome: Outcome| EngineResult {
            outcome,
            returned: state.completed.clone(),
            usage: state.usage.clone(),
            effects: state.effects.clone(),
        };
        let entry = match target {
            CallTarget::ByName { module, name } => self.admitted.callable_by_name(&module, &name),
            CallTarget::ByFunction { function } => self.admitted.callable_by_function(&function),
        };
        let empty = State::new();
        let Some(callable_index) = entry else {
            return finish(
                &empty,
                Outcome::InvalidRequest {
                    reason: "unknown callable".to_owned(),
                },
            );
        };
        let callable = &self.admitted.artifact.callables[callable_index];
        let Some(&function_index) = self.functions.get(&callable.function) else {
            return finish(
                &empty,
                Outcome::InvalidRequest {
                    reason: "callable function not in code section".to_owned(),
                },
            );
        };
        let (mut parts, begun) = self.begin(function_index, arguments);
        if let Err(outcome) = begun {
            return finish(&parts.state, outcome);
        }
        let outcome = match self.drive(&mut parts, None) {
            DriveExit::Finished(outcome) => outcome,
            DriveExit::Suspended(_) => Outcome::Trap {
                detail: "suspend without debugger".to_owned(),
            },
        };
        parts.state.usage.exit_call();
        finish(&parts.state, outcome)
    }

    /// Begin one call: entry frame plus bound arguments. Shared by
    /// one-shot and debug entry so both bind identically. Always
    /// returns the parts (partial on error) so usage accounting for
    /// failed entry matches one-shot behavior exactly.
    pub(crate) fn begin(
        &self,
        function_index: usize,
        arguments: Vec<Value>,
    ) -> (LiveParts, Result<(), Outcome>) {
        let mut parts = LiveParts {
            state: State::new(),
            frame: Frame {
                function: function_index,
                values: BTreeMap::new(),
                block: String::new(),
                iters: BTreeMap::new(),
                ip: 0,
                seq: 0,
            },
            stack: Vec::new(),
        };
        if let Err(outcome) = parts.state.usage.enter_call(&self.envelope) {
            return (parts, Err(outcome));
        }
        match self.bind_entry(&mut parts.state, function_index, &mut parts.frame, arguments) {
            Ok(entry_block) => {
                parts.frame.block = entry_block;
                (parts, Ok(()))
            }
            Err(outcome) => {
                parts.state.usage.exit_call();
                (parts, Err(outcome))
            }
        }
    }

    /// Bind entry arguments to function inputs by position with arity
    /// and coarse type checks. Returns the entry block identity.
    fn bind_entry(
        &self,
        state: &mut State,
        function_index: usize,
        frame: &mut Frame,
        arguments: Vec<Value>,
    ) -> Result<String, Outcome> {
        let function = &self.module.functions[function_index];
        if arguments.len() != function.inputs.len() {
            return Err(Outcome::InvalidRequest {
                reason: format!(
                    "arity mismatch: {} argument(s) for {} input(s)",
                    arguments.len(),
                    function.inputs.len()
                ),
            });
        }
        for (input, value) in function.inputs.iter().zip(arguments) {
            if !value_matches_input(&input.ty, &value) {
                return Err(Outcome::InvalidRequest {
                    reason: format!(
                        "argument does not match input type {}",
                        input.ty.semantic_name()
                    ),
                });
            }
            let cells = value.cells();
            state.usage.note_cells(cells, &self.envelope)?;
            frame.values.insert(input.identity.0.clone(), value);
        }
        function
            .blocks
            .first()
            .map(|block| block.identity.0.clone())
            .ok_or_else(|| Outcome::InvalidRequest {
                reason: "function has no blocks".to_owned(),
            })
    }

    /// Bind branch arguments to a target block's parameters.
    fn bind_branch(
        &self,
        state: &mut State,
        frame: &mut Frame,
        target: &str,
        arguments: &[String],
    ) -> Result<(), Outcome> {
        let function = &self.module.functions[frame.function];
        let Some(&block_index) = self.blocks[frame.function].get(target) else {
            return Err(Outcome::Trap {
                detail: format!("branch to unknown block {target}"),
            });
        };
        let params = &function.blocks[block_index].parameters;
        if params.len() != arguments.len() {
            return Err(Outcome::InvalidRequest {
                reason: "branch arguments did not match target parameters".to_owned(),
            });
        }
        let mut bound = Vec::with_capacity(params.len());
        for (param, name) in params.iter().zip(arguments.iter()) {
            let Some(value) = frame.values.get(name).cloned() else {
                return Err(Outcome::InvalidRequest {
                    reason: "branch argument value is unavailable".to_owned(),
                });
            };
            if !value_matches_input(&param.ty, &value) {
                return Err(Outcome::InvalidRequest {
                    reason: "branch argument does not match parameter type".to_owned(),
                });
            }
            bound.push((param.identity.0.clone(), value));
        }
        for (identity, value) in bound {
            let cells = value.cells();
            state.usage.note_cells(cells, &self.envelope)?;
            frame.values.insert(identity, value);
        }
        Ok(())
    }

    /// Main loop over an explicit frame stack. Calls push, returns pop.
    /// With a debugger attached, matched safe points return
    /// [`DriveExit::Suspended`] instead of continuing; the holder
    /// resumes with the same parts, so suspension never re-executes.
    /// Without a debugger the hooks compile to no-ops and behavior is
    /// exactly the one-shot path.
    #[allow(clippy::ptr_arg, clippy::too_many_lines)]
    pub(crate) fn drive(
        &self,
        parts: &mut LiveParts,
        mut dbg: Option<&mut DebugDriver>,
    ) -> DriveExit {
        loop {
            let function_index = parts.frame.function;
            let block_id = parts.frame.block.clone();
            let depth = parts.stack.len();
            let Some(&block_index) = self.blocks[function_index].get(&block_id) else {
                return self.terminal(
                    parts,
                    dbg,
                    Outcome::Trap {
                        detail: format!("unknown block {block_id}"),
                    },
                );
            };
            // A function-entry suspension already ran block entry for
            // the callee entry block; the resumed loop skips it once
            // so iteration accounting and events run exactly once.
            let prefetched = dbg
                .as_deref_mut()
                .is_some_and(|driver| driver.take_block_prefetched());
            if !prefetched {
                if let Err(outcome) =
                    self.enter_block(&mut parts.frame, function_index, &block_id)
                {
                    return self.terminal(parts, dbg, outcome);
                }
                if let Some(driver) = dbg.as_deref_mut() {
                    driver.note_block_enter(depth, &block_id);
                }
            }
            let block = &self.module.functions[function_index].blocks[block_index];
            let mut jumped = false;
            while parts.frame.ip < block.instructions.len() {
                let instruction = &block.instructions[parts.frame.ip];
                if let Some(driver) = dbg.as_deref_mut() {
                    let reasons = driver.before_operation(
                        parts.frame.seq,
                        depth,
                        parts.frame.ip,
                        function_index,
                        &block_id,
                        instruction,
                        &parts.frame.values,
                    );
                    if !reasons.is_empty() {
                        let kinds = DebugDriver::suspend_kinds("operation", &reasons);
                        let safe_point = driver.operation_safe_point(
                            parts.frame.seq,
                            depth,
                            parts.state.usage.steps,
                        );
                        return DriveExit::Suspended(Box::new(SuspendRequest {
                            reasons,
                            safe_point,
                            position: (
                                parts.frame.seq,
                                parts.frame.block.clone(),
                                parts.frame.ip,
                            ),
                            kinds,
                            pending_terminal: None,
                        }));
                    }
                }
                if let Err(outcome) = parts.state.usage.charge_step(&self.envelope) {
                    return self.terminal(parts, dbg, outcome);
                }
                match self.step_instruction(
                    &mut parts.state,
                    &mut parts.frame,
                    &mut parts.stack,
                    instruction,
                    dbg.as_deref_mut(),
                ) {
                    Step::Continue => {
                        if let Some(driver) = dbg.as_deref_mut() {
                            driver.after_operation(
                                parts.frame.seq,
                                depth,
                                &block_id,
                                instruction,
                                &parts.frame.values,
                            );
                        }
                        parts.frame.ip += 1;
                    }
                    Step::Jump(target) => {
                        // Instruction-level jumps come only from Call:
                        // the frame already switched to the callee.
                        if matches!(
                            instruction.kind,
                            mncs_model::SsaInstructionKind::Call { .. }
                        ) {
                            if let Some(driver) = dbg.as_deref_mut() {
                                let callee_seq = parts.frame.seq;
                                let callee_function = parts.frame.function;
                                let new_depth = parts.stack.len();
                                let arguments: Vec<(String, Value)> = parts
                                    .frame
                                    .values
                                    .iter()
                                    .map(|(binding, value)| (binding.clone(), value.clone()))
                                    .collect();
                                driver.note_call_enter(
                                    &instruction.identity.0,
                                    callee_function,
                                    callee_seq,
                                    new_depth,
                                    &arguments,
                                );
                                let mut entry_reasons =
                                    driver.evaluate_function_entry(callee_function);
                                // Merge the callee's first-operation
                                // reasons so entry suspends once.
                                let first = self.module.functions[callee_function]
                                    .blocks
                                    .first()
                                    .and_then(|entry| entry.instructions.first());
                                if let Some(first) = first {
                                    if driver.stop_arrived(callee_seq, &target, 0, "operation") {
                                        entry_reasons.extend(
                                            driver.evaluate_stops_for_instruction(new_depth, first),
                                        );
                                        if matches!(
                                            first.kind,
                                            mncs_model::SsaInstructionKind::HostCall { .. }
                                        ) {
                                            entry_reasons
                                                .extend(driver.effect_before_reasons());
                                        }
                                    }
                                }
                                if !entry_reasons.is_empty() {
                                    if let Err(outcome) =
                                        parts.state.usage.charge_step(&self.envelope)
                                    {
                                        return self.terminal(parts, dbg, outcome);
                                    }
                                    parts.frame.block = target.clone();
                                    parts.frame.ip = 0;
                                    if let Err(outcome) = self.enter_block(
                                        &mut parts.frame,
                                        callee_function,
                                        &target,
                                    ) {
                                        return self.terminal(parts, dbg, outcome);
                                    }
                                    driver.note_block_enter(new_depth, &target);
                                    // Suspend with the jump applied and
                                    // the block entered: resume skips
                                    // entry once and executes the
                                    // first operation.
                                    driver.note_function_entry_position(
                                        callee_function,
                                        &target,
                                        first.map(|instruction| {
                                            (
                                                instruction.identity.0.clone(),
                                                instruction
                                                    .semantic_identity
                                                    .as_ref()
                                                    .map(|id| id.0.clone()),
                                            )
                                        }),
                                    );
                                    driver.set_block_prefetched();
                                    let kinds = DebugDriver::suspend_kinds("operation", &entry_reasons);
                                    let safe_point = driver.operation_safe_point(
                                        callee_seq,
                                        new_depth,
                                        parts.state.usage.steps,
                                    );
                                    return DriveExit::Suspended(Box::new(SuspendRequest {
                                        reasons: entry_reasons,
                                        safe_point,
                                        position: (callee_seq, target, 0),
                                        kinds,
                                        pending_terminal: None,
                                    }));
                                }
                            }
                        }
                        if let Err(outcome) = parts.state.usage.charge_step(&self.envelope) {
                            return self.terminal(parts, dbg, outcome);
                        }
                        parts.frame.block = target;
                        parts.frame.ip = 0;
                        jumped = true;
                        break;
                    }
                    Step::Suspend(kind) => {
                        return self.suspend_effect(parts, dbg, instruction, kind);
                    }
                    Step::Halt(outcome) => return self.terminal(parts, dbg, outcome),
                }
            }
            if jumped {
                continue;
            }
            if let Err(outcome) = parts.state.usage.charge_step(&self.envelope) {
                return self.terminal(parts, dbg, outcome);
            }
            let terminator = &block.terminator;
            match self.step_terminator(
                &mut parts.state,
                &mut parts.frame,
                &mut parts.stack,
                function_index,
                &block_id,
                terminator,
            ) {
                Step::Continue => {
                    // Return unwound one frame: resume the caller frame.
                    let Some(caller) = parts.stack.pop() else {
                        return self.terminal(
                            parts,
                            dbg,
                            Outcome::Trap {
                                detail: "return stack underflow".to_owned(),
                            },
                        );
                    };
                    if let Some(driver) = dbg.as_deref_mut() {
                        // Capture returned values from the callee frame
                        // before it is dropped.
                        let returned: Vec<(String, Value)> = match terminator {
                            SsaTerminator::Return { values } => values
                                .iter()
                                .filter_map(|name| {
                                    parts.frame.values.get(&name.0).map(|value| {
                                        (name.0.clone(), value.clone())
                                    })
                                })
                                .collect(),
                            _ => Vec::new(),
                        };
                        driver.note_return(parts.frame.seq, &returned, "returned");
                    }
                    parts.frame = caller.frame;
                    // The call instruction sits at the restored ip;
                    // capture it before advancing past the call.
                    let caller_block = parts.frame.block.clone();
                    let caller_ip = parts.frame.ip;
                    parts.frame.ip += 1;
                    if caller.output.is_some() && parts.state.completed.is_empty() {
                        return self.terminal(
                            parts,
                            dbg,
                            Outcome::InvalidRequest {
                                reason: "callee returned no value".to_owned(),
                            },
                        );
                    }
                    if let Some(output) = caller.output {
                        if let Some(value) = parts.state.completed.first().cloned() {
                            parts.frame.values.insert(output, value);
                        }
                    }
                    if let Some(driver) = dbg.as_deref_mut() {
                        // The call's result lands now: emit its
                        // operation_result so every operation_enter
                        // pairs with exactly one result.
                        let call_instruction = self.module.functions[parts.frame.function]
                            .blocks
                            .iter()
                            .find(|block| block.identity.0 == caller_block)
                            .and_then(|block| block.instructions.get(caller_ip));
                        if let Some(call_instruction) = call_instruction {
                            let call_depth = parts.stack.len();
                            driver.after_operation(
                                parts.frame.seq,
                                call_depth,
                                &caller_block,
                                call_instruction,
                                &parts.frame.values,
                            );
                        }
                    }
                    parts.state.completed.clear();
                }
                Step::Jump(target) => {
                    if let Err(outcome) = parts.state.usage.charge_step(&self.envelope) {
                        return self.terminal(parts, dbg, outcome);
                    }
                    parts.frame.block = target;
                    parts.frame.ip = 0;
                }
                Step::Suspend(_) => {
                    // Terminators never suspend; the arm exists so the
                    // shared Step type stays total.
                    return self.terminal(
                        parts,
                        dbg,
                        Outcome::Trap {
                            detail: "terminator suspended".to_owned(),
                        },
                    );
                }
                Step::Halt(outcome) => return self.terminal(parts, dbg, outcome),
            }
        }
    }

    /// Suspend at an effect boundary. `effect_after` advances past the
    /// completed instruction first so resume cannot dispatch twice,
    /// and emits the pending `operation_result` for the completed op.
    fn suspend_effect(
        &self,
        parts: &mut LiveParts,
        dbg: Option<&mut DebugDriver>,
        instruction: &SsaInstruction,
        kind: SuspendKind,
    ) -> DriveExit {
        let Some(driver) = dbg else {
            return DriveExit::Finished(Outcome::Trap {
                detail: "suspend without debugger".to_owned(),
            });
        };
        let depth = parts.stack.len();
        match kind {
            SuspendKind::EffectBefore(reasons) => {
                let kinds = DebugDriver::suspend_kinds("effect_before", &reasons);
                let safe_point =
                    driver.effect_safe_point(parts.frame.seq, depth, parts.state.usage.steps, "before");
                DriveExit::Suspended(Box::new(SuspendRequest {
                    reasons,
                    safe_point,
                    position: (
                        parts.frame.seq,
                        parts.frame.block.clone(),
                        parts.frame.ip,
                    ),
                    kinds,
                    pending_terminal: None,
                }))
            }
            SuspendKind::EffectAfter(reasons) => {
                driver.after_operation(
                    parts.frame.seq,
                    depth,
                    &parts.frame.block.clone(),
                    instruction,
                    &parts.frame.values,
                );
                // Record the arrival at the completed instruction,
                // then advance past it: resume continues at the next
                // operation and can never dispatch twice.
                let position = (
                    parts.frame.seq,
                    parts.frame.block.clone(),
                    parts.frame.ip,
                );
                parts.frame.ip += 1;
                let kinds = DebugDriver::suspend_kinds("effect_after", &reasons);
                let safe_point =
                    driver.effect_safe_point(parts.frame.seq, depth, parts.state.usage.steps, "after");
                DriveExit::Suspended(Box::new(SuspendRequest {
                    reasons,
                    safe_point,
                    position,
                    kinds,
                    pending_terminal: None,
                }))
            }
        }
    }

    /// Single choke point for terminal outcomes. Observed runs emit
    /// terminal events here and suspend before finalizing abnormal
    /// outcomes when armed; unobserved runs finish identically.
    fn terminal(
        &self,
        parts: &mut LiveParts,
        dbg: Option<&mut DebugDriver>,
        outcome: Outcome,
    ) -> DriveExit {
        let Some(driver) = dbg else {
            return DriveExit::Finished(outcome);
        };
        let suspend_armed = driver.note_terminal(&outcome);
        let mut reasons = driver.failure_stop_reasons();
        if !outcome.is_completed() && suspend_armed && reasons.is_empty() {
            reasons.push(crate::debug::StopReason::FailureOrTrap);
        }
        if outcome.is_completed() || (!suspend_armed && reasons.is_empty()) {
            return DriveExit::Finished(outcome);
        }
        let depth = parts.stack.len();
        let safe_point =
            driver.terminal_safe_point(parts.frame.seq, depth, parts.state.usage.steps);
        DriveExit::Suspended(Box::new(SuspendRequest {
            reasons,
            safe_point,
            position: (
                parts.frame.seq,
                parts.frame.block.clone(),
                parts.frame.ip,
            ),
            kinds: vec!["terminal".to_owned()],
            pending_terminal: Some(outcome),
        }))
    }

    /// Region iteration accounting on block entry. Every entry into a
    /// region body block is one executed iteration; past the declared
    /// bound the run ends with BudgetExhausted. This is how
    /// compiler-visible iteration bounds survive into execution, even
    /// though valid compiler output never exceeds them (the check is
    /// defense in depth, plus enforcement for hand-built artifacts).
    fn enter_block(
        &self,
        frame: &mut Frame,
        function_index: usize,
        block_id: &str,
    ) -> Result<(), Outcome> {
        let function = &self.module.functions[function_index];
        for region in &function.bounded_iterations {
            if !region.body_blocks.iter().any(|b| b.0 == block_id) {
                continue;
            }
            let count = frame.iters.entry(region.identity.0.clone()).or_insert(0);
            *count += 1;
            if *count > u64::from(region.bound) {
                return Err(Outcome::BudgetExhausted {
                    dimension: "iterations".to_owned(),
                });
            }
        }
        if let Some(limit) = self.envelope.limit_of("iterations") {
            let total: u64 = frame.iters.values().sum();
            if total > limit {
                return Err(Outcome::BudgetExhausted {
                    dimension: "iterations".to_owned(),
                });
            }
        }
        Ok(())
    }

    #[allow(clippy::ptr_arg)]
    fn step_terminator(
        &self,
        state: &mut State,
        frame: &mut Frame,
        stack: &mut Vec<Caller>,
        function_index: usize,
        block_id: &str,
        terminator: &SsaTerminator,
    ) -> Step {
        match terminator {
            SsaTerminator::Return { values } => {
                let mut returned = Vec::with_capacity(values.len());
                for name in values {
                    let Some(value) = frame.values.get(&name.0).cloned() else {
                        if std::env::var("VM_TRACE").is_ok() {
                            eprintln!(
                                "TRACE return-missing fn={} block={} want={} have={:?}",
                                self.module.functions[function_index].semantic_identity.0,
                                block_id,
                                name.0,
                                frame.values.keys().collect::<Vec<_>>()
                            );
                        }
                        return Step::Halt(Outcome::InvalidRequest {
                            reason: "return referenced an unavailable value".to_owned(),
                        });
                    };
                    returned.push(value);
                }
                let function = &self.module.functions[function_index];
                if returned.len() != function.outputs.len() {
                    // The oracle reads the declared outputs; extra or
                    // missing values are a malformed return.
                    return Step::Halt(Outcome::InvalidRequest {
                        reason: "return arity does not match function outputs".to_owned(),
                    });
                }
                state.completed = returned;
                state.usage.exit_call();
                if stack.is_empty() {
                    Step::Halt(Outcome::Completed)
                } else {
                    Step::Continue
                }
            }
            SsaTerminator::Branch { target, arguments } => {
                match self.bind_branch(state, frame, &target.0, &names(arguments)) {
                    Ok(()) => Step::Jump(target.0.clone()),
                    Err(outcome) => Step::Halt(outcome),
                }
            }
            SsaTerminator::ConditionalBranch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
            } => {
                let Some(Value::Boolean(taken)) = frame.values.get(&condition.0) else {
                    return Step::Halt(Outcome::InvalidRequest {
                        reason: "conditional branch did not receive a boolean value".to_owned(),
                    });
                };
                let (target, arguments) = if *taken {
                    (then_target, then_arguments)
                } else {
                    (else_target, else_arguments)
                };
                match self.bind_branch(state, frame, &target.0, &names(arguments)) {
                    Ok(()) => Step::Jump(target.0.clone()),
                    Err(outcome) => Step::Halt(outcome),
                }
            }
            SsaTerminator::Failure { mode } => Step::Halt(Outcome::ProgramFailure {
                mode: format!("{mode:?}"),
                detail: format!("failure terminator reached in block {block_id}"),
            }),
        }
    }

    #[allow(clippy::too_many_lines, clippy::ptr_arg)]
    fn step_instruction(
        &self,
        state: &mut State,
        frame: &mut Frame,
        stack: &mut Vec<Caller>,
        instruction: &SsaInstruction,
        mut dbg: Option<&mut DebugDriver>,
    ) -> Step {
        let unsupported = |feature: String| Step::Halt(Outcome::Unsupported { feature });
        let invalid = |reason: String| Step::Halt(Outcome::InvalidRequest { reason });
        let inputs: Vec<Value> = instruction
            .inputs
            .iter()
            .map(|id| frame.values.get(&id.0).cloned())
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();
        // Most instructions need all inputs present; arms with special
        // arity rules read `frame` directly instead.
        let need = |count: usize| -> Option<Vec<Value>> {
            if inputs.len() == count && instruction.inputs.len() == count {
                Some(inputs.clone())
            } else {
                None
            }
        };
        let emit = |frame: &mut Frame, value: Value| {
            if let Some(output) = instruction.outputs.first() {
                frame.values.insert(output.identity.0.clone(), value);
            }
        };
        match &instruction.kind {
            SsaInstructionKind::Constant { value, ty } => {
                let Some(produced) = constant_value(*value, ty) else {
                    return unsupported("constant outside the reference subset".to_owned());
                };
                emit(frame, produced);
                Step::Continue
            }
            SsaInstructionKind::Integer {
                operator,
                operand_type,
                intent,
            } => {
                if !integer_operator_supported(operator, *intent) {
                    return unsupported(format!("integer operator/intent {operator:?}/{intent:?}"));
                }
                let Some(pair) = need(2) else {
                    return invalid("integer operands were unavailable".to_owned());
                };
                let (Value::Integer { value: left, .. }, Value::Integer { value: right, .. }) =
                    (&pair[0], &pair[1])
                else {
                    return invalid("integer operands were not integers".to_owned());
                };
                if !in_range(*left, *operand_type) || !in_range(*right, *operand_type) {
                    return halt_program("integer operand outside declared range");
                }
                let evaluated = IntegerOperation {
                    operator: operator.clone(),
                    operand_type: *operand_type,
                    left: *left,
                    right: *right,
                    intent: *intent,
                }
                .evaluate();
                let Some(value) = evaluated.value else {
                    if evaluated.trapped {
                        return Step::Halt(Outcome::Trap {
                            detail: format!("trapping integer {operator}"),
                        });
                    }
                    return halt_program(format!("integer {operator} overflow or no value"));
                };
                let ty = match instruction.outputs.first().map(|o| &o.ty) {
                    Some(BodyType::Integer(ty)) => *ty,
                    _ => *operand_type,
                };
                emit(
                    frame,
                    Value::Integer {
                        value,
                        bits: u32::from(ty.bits),
                        signed: ty.signed,
                    },
                );
                Step::Continue
            }
            SsaInstructionKind::IntegerCompare {
                predicate,
                operand_type,
            } => {
                let Some(pair) = need(2) else {
                    return invalid("integer comparison operands were unavailable".to_owned());
                };
                let (Value::Integer { value: left, .. }, Value::Integer { value: right, .. }) =
                    (&pair[0], &pair[1])
                else {
                    return invalid("integer comparison operands were not integers".to_owned());
                };
                if !in_range(*left, *operand_type) || !in_range(*right, *operand_type) {
                    return halt_program("integer operand outside declared range");
                }
                let Some(value) = compare_integers(predicate, *left, *right) else {
                    return unsupported(format!("integer predicate {predicate:?}"));
                };
                emit(frame, Value::Boolean(value));
                Step::Continue
            }
            SsaInstructionKind::BooleanOp { operator } => {
                let Some(pair) = need(2) else {
                    return invalid("boolean operands were unavailable".to_owned());
                };
                let (Value::Boolean(left), Value::Boolean(right)) = (&pair[0], &pair[1]) else {
                    return invalid("boolean operands were not booleans".to_owned());
                };
                match operator.as_str() {
                    "and" => emit(frame, Value::Boolean(*left && *right)),
                    "or" => emit(frame, Value::Boolean(*left || *right)),
                    _ => return unsupported(format!("boolean operator {operator:?}")),
                }
                Step::Continue
            }
            SsaInstructionKind::BooleanCompare { predicate } => {
                let Some(pair) = need(2) else {
                    return invalid("boolean comparison operands were unavailable".to_owned());
                };
                let (Value::Boolean(left), Value::Boolean(right)) = (&pair[0], &pair[1]) else {
                    return invalid("boolean comparison operands were not booleans".to_owned());
                };
                match predicate.as_str() {
                    "eq" => emit(frame, Value::Boolean(left == right)),
                    "ne" => emit(frame, Value::Boolean(left != right)),
                    _ => return unsupported(format!("boolean predicate {predicate:?}")),
                }
                Step::Continue
            }
            SsaInstructionKind::BooleanNot => {
                if instruction.inputs.len() != 1 {
                    return invalid("boolean negation requires one operand".to_owned());
                }
                let Some(Value::Boolean(value)) = frame.values.get(&instruction.inputs[0].0) else {
                    return invalid("boolean negation operand was not a boolean".to_owned());
                };
                let value = *value;
                emit(frame, Value::Boolean(!value));
                Step::Continue
            }
            SsaInstructionKind::ByteBitwise { operator } => {
                let Some(pair) = need(2) else {
                    return invalid("byte operands were unavailable".to_owned());
                };
                let (Value::Byte(left), Value::Byte(right)) = (&pair[0], &pair[1]) else {
                    return invalid("byte operands were not bytes".to_owned());
                };
                match operator.as_str() {
                    "and" => emit(frame, Value::Byte(left & right)),
                    "or" => emit(frame, Value::Byte(left | right)),
                    "xor" => emit(frame, Value::Byte(left ^ right)),
                    _ => return unsupported(format!("byte operator {operator:?}")),
                }
                Step::Continue
            }
            SsaInstructionKind::ByteShift { operator } => {
                if instruction.inputs.len() != 2 {
                    return invalid("byte shift requires two operands".to_owned());
                }
                let byte = frame.values.get(&instruction.inputs[0].0).cloned();
                let count = frame.values.get(&instruction.inputs[1].0).cloned();
                let (Some(Value::Byte(byte)), Some(Value::Integer { value: count, .. })) =
                    (byte, count)
                else {
                    return invalid("byte shift operands were mistyped".to_owned());
                };
                if count < 0 {
                    return unsupported("negative byte shift count".to_owned());
                }
                let count = (count % 8) as u32;
                match operator.as_str() {
                    "shl" => emit(frame, Value::Byte(byte.wrapping_shl(count))),
                    "shr" => emit(frame, Value::Byte(byte.wrapping_shr(count))),
                    _ => return unsupported(format!("byte shift {operator:?}")),
                }
                Step::Continue
            }
            SsaInstructionKind::ByteCompare { predicate } => {
                let Some(pair) = need(2) else {
                    return invalid("byte comparison operands were unavailable".to_owned());
                };
                let (Value::Byte(left), Value::Byte(right)) = (&pair[0], &pair[1]) else {
                    return invalid("byte comparison operands were not bytes".to_owned());
                };
                match predicate.as_str() {
                    "eq" => emit(frame, Value::Boolean(left == right)),
                    "ne" => emit(frame, Value::Boolean(left != right)),
                    "lt" => emit(frame, Value::Boolean(left < right)),
                    "le" => emit(frame, Value::Boolean(left <= right)),
                    "gt" => emit(frame, Value::Boolean(left > right)),
                    "ge" => emit(frame, Value::Boolean(left >= right)),
                    _ => return unsupported(format!("byte predicate {predicate:?}")),
                }
                Step::Continue
            }
            SsaInstructionKind::Select { .. } => {
                if instruction.inputs.len() != 3 {
                    return invalid("selection requires condition and two candidates".to_owned());
                }
                let Some(Value::Boolean(taken)) =
                    frame.values.get(&instruction.inputs[0].0).cloned()
                else {
                    return invalid("selection condition was not a boolean".to_owned());
                };
                let slot = if taken { 1 } else { 2 };
                let Some(chosen) = frame.values.get(&instruction.inputs[slot].0).cloned() else {
                    return invalid("selection candidate was unavailable".to_owned());
                };
                emit(frame, chosen);
                Step::Continue
            }
            SsaInstructionKind::RecordConstruct {
                type_identity,
                field_names,
            } => {
                if field_names.len() != instruction.inputs.len() {
                    return invalid(
                        "record construction field count does not match operands".to_owned(),
                    );
                }
                let Some(output) = instruction.outputs.first() else {
                    return Step::Continue;
                };
                let mut fields = Vec::with_capacity(field_names.len());
                for (name, input) in field_names.iter().zip(instruction.inputs.iter()) {
                    let Some(value) = frame.values.get(&input.0).cloned() else {
                        return invalid("record construction operand was unavailable".to_owned());
                    };
                    fields.push((name.clone(), value));
                }
                fields.sort_by(|left, right| left.0.cmp(&right.0));
                let name = output.ty.semantic_name();
                emit(
                    frame,
                    Value::Record {
                        type_id: type_identity.0.clone(),
                        name,
                        fields: Arc::new(fields),
                    },
                );
                Step::Continue
            }
            SsaInstructionKind::RecordProject { field, .. } => {
                if instruction.inputs.len() != 1 {
                    return invalid("record projection requires one operand".to_owned());
                }
                let Some(Value::Record { fields, .. }) =
                    frame.values.get(&instruction.inputs[0].0).cloned()
                else {
                    return invalid("record projection operand is not a record".to_owned());
                };
                match fields.iter().find(|(name, _)| name == field) {
                    Some((_, value)) => {
                        emit(frame, value.clone());
                        Step::Continue
                    }
                    None => invalid("record has no such field".to_owned()),
                }
            }
            SsaInstructionKind::FiniteConstruct {
                type_identity,
                variant_identity,
                discriminant,
                payload_fields,
            } => {
                if instruction.inputs.len() != payload_fields.len() {
                    return invalid("finite payload count does not match operands".to_owned());
                }
                let Some(output) = instruction.outputs.first() else {
                    return Step::Continue;
                };
                let output_id = output.identity.0.clone();
                let mut payload = Vec::with_capacity(payload_fields.len());
                for (field, input) in payload_fields.iter().zip(instruction.inputs.iter()) {
                    let Some(value) = frame.values.get(&input.0).cloned() else {
                        return invalid("finite payload operand was unavailable".to_owned());
                    };
                    payload.push((field.clone(), value));
                }
                frame.values.insert(
                    output_id,
                    Value::Finite {
                        type_id: type_identity.0.clone(),
                        variant_id: variant_identity.0.clone(),
                        discriminant: *discriminant,
                        payload: Arc::new(payload),
                    },
                );
                Step::Continue
            }
            SsaInstructionKind::FinitePayloadProject {
                type_identity,
                variant_identity,
                discriminant,
                field,
            } => {
                if instruction.inputs.len() != 1 {
                    return invalid("payload projection requires one operand".to_owned());
                }
                let Some(Value::Finite {
                    type_id: actual_type,
                    variant_id: actual_variant,
                    discriminant: actual_disc,
                    payload,
                }) = frame.values.get(&instruction.inputs[0].0).cloned()
                else {
                    return invalid("payload projection operand is not finite".to_owned());
                };
                if actual_type != type_identity.0
                    || actual_variant != variant_identity.0
                    || actual_disc != *discriminant
                {
                    return invalid("payload projection type/variant mismatch".to_owned());
                }
                match payload.iter().find(|(name, _)| name == field) {
                    Some((_, value)) => {
                        emit(frame, value.clone());
                        Step::Continue
                    }
                    None => invalid("payload field was not carried by the value".to_owned()),
                }
            }
            SsaInstructionKind::FiniteIsVariant {
                type_identity,
                variant_identity,
                discriminant,
                ..
            } => {
                if instruction.inputs.len() != 1 {
                    return invalid("variant test requires one operand".to_owned());
                }
                let Some(Value::Finite {
                    type_id: actual_type,
                    variant_id: actual_variant,
                    discriminant: actual_disc,
                    ..
                }) = frame.values.get(&instruction.inputs[0].0).cloned()
                else {
                    return invalid("variant test operand is not finite".to_owned());
                };
                emit(
                    frame,
                    Value::Boolean(
                        actual_type == type_identity.0
                            && actual_variant == variant_identity.0
                            && actual_disc == *discriminant,
                    ),
                );
                Step::Continue
            }
            SsaInstructionKind::SequenceConstruct { length, .. } => {
                if *length as usize != instruction.inputs.len() {
                    return invalid("sequence element count does not match operands".to_owned());
                }
                let Some(output) = instruction.outputs.first() else {
                    return Step::Continue;
                };
                let output_id = output.identity.0.clone();
                let mut elements = Vec::with_capacity(instruction.inputs.len());
                for input in instruction.inputs.iter() {
                    let Some(value) = frame.values.get(&input.0).cloned() else {
                        return invalid("sequence operand was unavailable".to_owned());
                    };
                    elements.push(value);
                }
                frame.values.insert(
                    output_id,
                    Value::Sequence {
                        elements: Arc::new(elements),
                    },
                );
                Step::Continue
            }
            SsaInstructionKind::SequenceProject { .. } => {
                if instruction.inputs.len() != 2 {
                    return invalid("sequence projection requires sequence and index".to_owned());
                }
                let sequence = frame.values.get(&instruction.inputs[0].0).cloned();
                let index = frame.values.get(&instruction.inputs[1].0).cloned();
                let (Some(Value::Sequence { elements }), Some(Value::Integer { value: index, .. })) =
                    (sequence, index)
                else {
                    return invalid("sequence projection operands were mistyped".to_owned());
                };
                if index < 0 {
                    return invalid("sequence projection index is negative".to_owned());
                }
                match elements.get(index as usize) {
                    Some(value) => {
                        emit(frame, value.clone());
                        Step::Continue
                    }
                    None => halt_program("sequence projection index out of bounds"),
                }
            }
            SsaInstructionKind::SequenceLength { .. } => {
                if instruction.inputs.len() != 1 {
                    return invalid("sequence length requires one operand".to_owned());
                }
                let Some(Value::Sequence { elements }) =
                    frame.values.get(&instruction.inputs[0].0).cloned()
                else {
                    return invalid("sequence length operand is not a sequence".to_owned());
                };
                emit(
                    frame,
                    Value::Integer {
                        value: elements.len() as i128,
                        bits: 64,
                        signed: false,
                    },
                );
                Step::Continue
            }
            SsaInstructionKind::SequenceReplace { bound, .. } => {
                if instruction.inputs.len() != 3 {
                    return invalid("sequence replace requires three operands".to_owned());
                }
                let sequence = frame.values.get(&instruction.inputs[0].0).cloned();
                let index = frame.values.get(&instruction.inputs[1].0).cloned();
                let element = frame.values.get(&instruction.inputs[2].0).cloned();
                let (
                    Some(Value::Sequence { elements }),
                    Some(Value::Integer { value: index, .. }),
                    Some(element),
                ) = (sequence, index, element)
                else {
                    return invalid("sequence replace operands were mistyped".to_owned());
                };
                if index < 0 {
                    return invalid("sequence replace index is negative".to_owned());
                }
                let capacity = sequence_capacity(bound);
                if elements.len() > capacity || index as usize >= capacity {
                    return halt_program("sequence replace outside declared bound");
                }
                let mut next = elements.as_ref().clone();
                if index as usize >= next.len() {
                    return halt_program("sequence replace index out of bounds");
                }
                next[index as usize] = element;
                emit(
                    frame,
                    Value::Sequence {
                        elements: Arc::new(next),
                    },
                );
                Step::Continue
            }
            SsaInstructionKind::BoundCheck { bound } => {
                if instruction.inputs.len() != 1 {
                    return invalid("bound check requires one operand".to_owned());
                }
                let Some(Value::Integer { value: index, .. }) =
                    frame.values.get(&instruction.inputs[0].0).cloned()
                else {
                    return invalid("bound check operand is not an integer".to_owned());
                };
                if index < 0 || index as usize >= sequence_capacity(bound) {
                    return halt_program("bound check failed");
                }
                emit(
                    frame,
                    Value::Integer {
                        value: index,
                        bits: 64,
                        signed: false,
                    },
                );
                Step::Continue
            }
            SsaInstructionKind::Convert { from, to } => {
                if instruction.inputs.len() != 1 {
                    return invalid("conversion requires one operand".to_owned());
                }
                let Some(operand) = frame.values.get(&instruction.inputs[0].0).cloned() else {
                    return invalid("conversion operand was unavailable".to_owned());
                };
                match convert_scalar(&operand, from, to) {
                    ConvertOut::Value(value) => {
                        emit(frame, value);
                        Step::Continue
                    }
                    ConvertOut::Trapped => halt_program("conversion trapped on out-of-range input"),
                    ConvertOut::Unsupported => {
                        unsupported("conversion outside the integer/boolean/byte subset".to_owned())
                    }
                }
            }
            SsaInstructionKind::Call {
                function,
                required_capabilities,
            } => {
                for required in required_capabilities {
                    if !self.caps.admits(&required.0) {
                        return Step::Halt(Outcome::CapabilityDenied {
                            capability: required.0.clone(),
                            operation: format!("call {}", function.0),
                        });
                    }
                }
                let Some(&callee_index) = self.functions.get(&function.0) else {
                    return invalid("call target does not exist".to_owned());
                };
                let callee = &self.module.functions[callee_index];
                if instruction.inputs.len() != callee.inputs.len() {
                    return invalid("call arity does not match callee inputs".to_owned());
                }
                let mut arguments = Vec::with_capacity(instruction.inputs.len());
                for (input, name) in callee.inputs.iter().zip(instruction.inputs.iter()) {
                    let Some(value) = frame.values.get(&name.0).cloned() else {
                        return invalid("call operand was unavailable".to_owned());
                    };
                    if !value_matches_input(&input.ty, &value) {
                        return invalid("call argument does not match input type".to_owned());
                    }
                    arguments.push(value);
                }
                if let Err(outcome) = state.usage.enter_call(&self.envelope) {
                    return Step::Halt(outcome);
                }
                let output = instruction.outputs.first().map(|o| o.identity.0.clone());
                let callee_seq = state.alloc_frame_seq();
                let caller = Caller {
                    frame: std::mem::replace(
                        frame,
                        Frame {
                            function: callee_index,
                            values: BTreeMap::new(),
                            block: String::new(),
                            iters: BTreeMap::new(),
                            ip: 0,
                            seq: callee_seq,
                        },
                    ),
                    output,
                };
                match self.bind_entry(state, callee_index, frame, arguments) {
                    Ok(entry_block) => {
                        frame.block = entry_block.clone();
                        stack.push(caller);
                        // Frame switched to the callee: restart the
                        // loop at the callee entry, not the next
                        // caller instruction.
                        Step::Jump(entry_block)
                    }
                    Err(outcome) => {
                        state.usage.exit_call();
                        *frame = caller.frame;
                        Step::Halt(outcome)
                    }
                }
            }
            SsaInstructionKind::Effect => {
                // Record-only effects, mirroring the oracle's Record
                // policy: nothing external is performed. Authority is
                // still checked where the instruction names it.
                for use_ in &instruction.capability_uses {
                    if !self.caps.admits(&use_.capability.0) {
                        return Step::Halt(Outcome::CapabilityDenied {
                            capability: use_.capability.0.clone(),
                            operation: "effect".to_owned(),
                        });
                    }
                }
                for effect in &instruction.effects {
                    self.observe_effect(
                        state,
                        EffectRequest {
                            capability: instruction
                                .capability_uses
                                .first()
                                .map(|use_| use_.capability.0.clone())
                                .unwrap_or_default(),
                            operation: format!("effect:{}", effect.0),
                            inputs: Vec::new(),
                        },
                        "recorded",
                        None,
                    );
                    // Record-only: observable, never suspending (no
                    // external boundary is crossed).
                    if let Some(driver) = dbg.as_deref_mut() {
                        driver.note_recorded_effect(
                            stack.len(),
                            &frame.block,
                            instruction,
                            &format!("effect:{}", effect.0),
                        );
                    }
                }
                Step::Continue
            }
            SsaInstructionKind::HostCall {
                capability,
                operation,
            } => {
                if mncs_model::host_call_arity(operation).is_none() {
                    return unsupported(format!("host operation {operation:?}"));
                }
                if instruction.inputs.len() != mncs_model::host_call_arity(operation).unwrap_or(0) {
                    return invalid("host call arity does not match operation".to_owned());
                }
                let mut inputs = Vec::with_capacity(instruction.inputs.len());
                for name in instruction.inputs.iter() {
                    let Some(value) = frame.values.get(&name.0).cloned() else {
                        return invalid("host call operand was unavailable".to_owned());
                    };
                    inputs.push(value);
                }
                if let Err(outcome) = state.usage.charge_effect(&self.envelope) {
                    return Step::Halt(outcome);
                }
                let request = EffectRequest {
                    capability: capability.clone(),
                    operation: operation.clone(),
                    inputs,
                };
                if let Some(driver) = dbg.as_deref_mut() {
                    let reasons = driver.before_effect(
                        frame.seq,
                        stack.len(),
                        frame.ip,
                        &frame.block,
                        instruction,
                        &request,
                    );
                    if !reasons.is_empty() {
                        return Step::Suspend(SuspendKind::EffectBefore(reasons));
                    }
                }
                match self.caps.dispatch(&request) {
                    Ok(response) => {
                        let provider = response.provider.clone();
                        let outputs = response.outputs.clone();
                        self.observe_effect(
                            state,
                            request,
                            "completed",
                            Some((provider, outputs.clone())),
                        );
                        if outputs.len() != instruction.outputs.len() {
                            return Step::Halt(Outcome::ProviderFailure {
                                provider: "host-call".to_owned(),
                                detail: "provider output arity mismatch".to_owned(),
                            });
                        }
                        for (output, value) in instruction.outputs.iter().zip(outputs) {
                            frame.values.insert(output.identity.0.clone(), value);
                        }
                        if let Some(driver) = dbg.as_deref_mut() {
                            let reasons =
                                driver.after_effect(stack.len(), &frame.block, instruction, "completed");
                            if !reasons.is_empty() {
                                return Step::Suspend(SuspendKind::EffectAfter(reasons));
                            }
                        }
                        Step::Continue
                    }
                    Err(outcome) => {
                        self.observe_effect(state, request, outcome.tag(), None);
                        // Failed dispatch still closes the observation
                        // honestly; stop reasons are discarded because
                        // the terminal boundary owns this stop.
                        if let Some(driver) = dbg {
                            driver.after_effect(stack.len(), &frame.block, instruction, outcome.tag());
                        }
                        Step::Halt(outcome)
                    }
                }
            }
            _ => unsupported(format!(
                "instruction {}",
                instruction_tag(&instruction.kind)
            )),
        }
    }

    fn observe_effect(
        &self,
        state: &mut State,
        request: EffectRequest,
        outcome_tag: &str,
        completed: Option<(String, Vec<Value>)>,
    ) {
        state.effect_sequence += 1;
        state.effects.push(EffectObservation {
            sequence: state.effect_sequence,
            request,
            outcome_tag: outcome_tag.to_owned(),
            response: completed.map(|(provider, outputs)| crate::capability::EffectResponse {
                provider,
                outputs,
                note: "provider".to_owned(),
            }),
        });
    }
}

enum Step {
    Continue,
    Jump(String),
    Halt(Outcome),
    Suspend(SuspendKind),
}

/// Suspension requested from inside one instruction's execution.
/// Only effect boundaries suspend mid-instruction; everything else
/// suspends between transitions.
pub(crate) enum SuspendKind {
    EffectBefore(Vec<StopReason>),
    EffectAfter(Vec<StopReason>),
}

fn names(ids: &[SemanticId]) -> Vec<String> {
    ids.iter().map(|id| id.0.clone()).collect()
}

fn halt_program(detail: impl Into<String>) -> Step {
    Step::Halt(Outcome::ProgramFailure {
        mode: "runtime-failure".to_owned(),
        detail: detail.into(),
    })
}

/// Coarse input-type check: kind and, for nominal aggregates, the
/// declared identity. `Named` and `GenericParam` spellings never
/// match: admission only resolves what the compiler resolved.
fn value_matches_input(ty: &BodyType, value: &Value) -> bool {
    match (ty, value) {
        (BodyType::Bool, Value::Boolean(_)) => true,
        (BodyType::Integer(_), Value::Integer { .. }) => true,
        (BodyType::Float(_), Value::FloatBits(_)) => true,
        (BodyType::Byte, Value::Byte(_)) => true,
        (BodyType::Sequence { .. }, Value::Sequence { .. }) => true,
        (BodyType::Vector { .. }, Value::Vector { .. }) => true,
        (BodyType::Mask { .. }, Value::Mask { .. }) => true,
        (BodyType::Finite { identity, .. }, Value::Finite { type_id, .. }) => {
            identity.0 == *type_id
        }
        (BodyType::Record { identity, .. }, Value::Record { type_id, .. }) => {
            identity.0 == *type_id
        }
        _ => false,
    }
}

fn constant_value(value: i128, ty: &BodyType) -> Option<Value> {
    match ty.clone() {
        BodyType::Integer(integer) if integer.bits == 1 && !integer.signed => match value {
            0 => Some(Value::Boolean(false)),
            1 => Some(Value::Boolean(true)),
            _ => None,
        },
        BodyType::Integer(integer) if in_range(value, integer) => Some(Value::Integer {
            value,
            bits: u32::from(integer.bits),
            signed: integer.signed,
        }),
        BodyType::Bool => match value {
            0 => Some(Value::Boolean(false)),
            1 => Some(Value::Boolean(true)),
            _ => None,
        },
        BodyType::Named(name) if name == "bool" => match value {
            0 => Some(Value::Boolean(false)),
            1 => Some(Value::Boolean(true)),
            _ => None,
        },
        BodyType::Byte if (0..=255).contains(&value) => Some(Value::Byte(value as u8)),
        _ => None,
    }
}

fn limits(bits: u16, signed: bool) -> Option<(i128, i128)> {
    if !(1..=126).contains(&bits) {
        return None;
    }
    if signed {
        let maximum = (1_i128 << (bits - 1)) - 1;
        Some((-maximum - 1, maximum))
    } else {
        Some((0, (1_i128 << bits) - 1))
    }
}

fn in_range(value: i128, ty: IntegerType) -> bool {
    limits(ty.bits, ty.signed)
        .is_some_and(|(minimum, maximum)| (minimum..=maximum).contains(&value))
}

fn integer_operator_supported(operator: &str, intent: ArithmeticIntent) -> bool {
    matches!(operator, "add" | "sub" | "mul" | "div" | "mod")
        || (matches!(operator, "and" | "or" | "xor" | "shl" | "shr")
            && matches!(intent, ArithmeticIntent::Wrapping))
}

fn compare_integers(predicate: &str, left: i128, right: i128) -> Option<bool> {
    match predicate {
        "eq" => Some(left == right),
        "ne" => Some(left != right),
        "lt" => Some(left < right),
        "le" => Some(left <= right),
        "gt" => Some(left > right),
        "ge" => Some(left >= right),
        _ => None,
    }
}

fn sequence_capacity(bound: &mncs_model::SequenceBound) -> usize {
    bound.ceiling() as usize
}

enum ConvertOut {
    Value(Value),
    Trapped,
    Unsupported,
}

/// Total scalar conversions over the integer/boolean/byte subset.
/// Extension preserves value with a range check; truncation wraps
/// into the target width; float involvement is Unsupported.
fn convert_scalar(operand: &Value, from: &BodyType, to: &BodyType) -> ConvertOut {
    let as_int = |value: &Value| -> Option<(i128, IntegerType)> {
        match value {
            Value::Integer {
                value,
                bits,
                signed,
            } => Some((
                *value,
                IntegerType {
                    bits: (*bits).min(126) as u16,
                    signed: *signed,
                },
            )),
            _ => None,
        }
    };
    match (from, to) {
        (BodyType::Integer(from_ty), BodyType::Integer(to_ty)) => {
            let Some((value, _)) = as_int(operand) else {
                return ConvertOut::Unsupported;
            };
            if !in_range(value, *from_ty) {
                return ConvertOut::Unsupported;
            }
            match wrap_to_bits(value, to_ty.bits, to_ty.signed) {
                Some(wrapped) => ConvertOut::Value(Value::Integer {
                    value: wrapped,
                    bits: u32::from(to_ty.bits),
                    signed: to_ty.signed,
                }),
                None => ConvertOut::Unsupported,
            }
        }
        (BodyType::Bool, BodyType::Integer(to_ty)) => {
            let Value::Boolean(value) = operand else {
                return ConvertOut::Unsupported;
            };
            ConvertOut::Value(Value::Integer {
                value: i128::from(*value),
                bits: u32::from(to_ty.bits),
                signed: to_ty.signed,
            })
        }
        (BodyType::Integer(_), BodyType::Bool) => {
            let Some((value, _)) = as_int(operand) else {
                return ConvertOut::Unsupported;
            };
            match value {
                0 => ConvertOut::Value(Value::Boolean(false)),
                1 => ConvertOut::Value(Value::Boolean(true)),
                _ => ConvertOut::Trapped,
            }
        }
        (BodyType::Byte, BodyType::Integer(to_ty)) => {
            let Value::Byte(value) = operand else {
                return ConvertOut::Unsupported;
            };
            ConvertOut::Value(Value::Integer {
                value: i128::from(*value),
                bits: u32::from(to_ty.bits),
                signed: to_ty.signed,
            })
        }
        (BodyType::Integer(_), BodyType::Byte) => {
            let Some((value, _)) = as_int(operand) else {
                return ConvertOut::Unsupported;
            };
            if (0..=255).contains(&value) {
                ConvertOut::Value(Value::Byte(value as u8))
            } else {
                ConvertOut::Trapped
            }
        }
        _ => ConvertOut::Unsupported,
    }
}

fn wrap_to_bits(value: i128, bits: u16, signed: bool) -> Option<i128> {
    if !(1..=126).contains(&bits) {
        return None;
    }
    let modulus = 1_i128.checked_shl(u32::from(bits))?;
    let truncated = value.rem_euclid(modulus);
    if signed {
        let sign = 1_i128.checked_shl(u32::from(bits - 1))?;
        Some(if truncated >= sign {
            truncated - modulus
        } else {
            truncated
        })
    } else {
        Some(truncated)
    }
}

fn instruction_tag(kind: &SsaInstructionKind) -> &'static str {
    match kind {
        SsaInstructionKind::Constant { .. } => "constant",
        SsaInstructionKind::Integer { .. } => "integer",
        SsaInstructionKind::IntegerCompare { .. } => "integer_compare",
        SsaInstructionKind::FloatConstant { .. } => "float_constant",
        SsaInstructionKind::Float { .. } => "float",
        SsaInstructionKind::FloatCompare { .. } => "float_compare",
        SsaInstructionKind::FloatIntrinsic { .. } => "float_intrinsic",
        SsaInstructionKind::BooleanOp { .. } => "boolean_op",
        SsaInstructionKind::BooleanCompare { .. } => "boolean_compare",
        SsaInstructionKind::BooleanNot => "boolean_not",
        SsaInstructionKind::ByteBitwise { .. } => "byte_bitwise",
        SsaInstructionKind::ByteShift { .. } => "byte_shift",
        SsaInstructionKind::ByteCompare { .. } => "byte_compare",
        SsaInstructionKind::Select { .. } => "select",
        SsaInstructionKind::SequenceReplace { .. } => "sequence_replace",
        SsaInstructionKind::BoundCheck { .. } => "bound_check",
        SsaInstructionKind::SequenceCopy { .. } => "sequence_copy",
        SsaInstructionKind::VectorConstruct { .. } => "vector_construct",
        SsaInstructionKind::VectorSplat { .. } => "vector_splat",
        SsaInstructionKind::VectorExtract { .. } => "vector_extract",
        SsaInstructionKind::VectorReplace { .. } => "vector_replace",
        SsaInstructionKind::VectorBinary { .. } => "vector_binary",
        SsaInstructionKind::VectorCompare { .. } => "vector_compare",
        SsaInstructionKind::MaskBinary { .. } => "mask_binary",
        SsaInstructionKind::MaskNot { .. } => "mask_not",
        SsaInstructionKind::MaskReduce { .. } => "mask_reduce",
        SsaInstructionKind::VectorReduce { .. } => "vector_reduce",
        SsaInstructionKind::Convert { .. } => "convert",
        SsaInstructionKind::SequenceConstruct { .. } => "sequence_construct",
        SsaInstructionKind::SequenceProject { .. } => "sequence_project",
        SsaInstructionKind::SequenceLength { .. } => "sequence_length",
        SsaInstructionKind::ViewConstruct { .. } => "view_construct",
        SsaInstructionKind::ViewNarrow { .. } => "view_narrow",
        SsaInstructionKind::FiniteConstruct { .. } => "finite_construct",
        SsaInstructionKind::FinitePayloadProject { .. } => "finite_payload_project",
        SsaInstructionKind::FiniteIsVariant { .. } => "finite_is_variant",
        SsaInstructionKind::RecordConstruct { .. } => "record_construct",
        SsaInstructionKind::RecordProject { .. } => "record_project",
        SsaInstructionKind::Call { .. } => "call",
        SsaInstructionKind::Effect => "effect",
        SsaInstructionKind::HostCall { .. } => "host_call",
        SsaInstructionKind::RuntimeCheck { .. } => "runtime_check",
    }
}
