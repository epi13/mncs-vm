//! Live-debug contract: observation, stops, inspection, resume,
//! stepping, effects, termination, and replay equivalence.
//!
//! Every test drives a real admitted corpus execution through the
//! canonical engine. Suspension holds the same run state that resume
//! continues: nothing here re-executes to fake a stop.

use std::collections::BTreeSet;

use mncs_vm::capability::{CapabilityEnv, ConstProvider};
use mncs_vm::debug::{
    DebugConfig, DebugError, DebugStart, LiveEvent, StopCondition, StopReason, StopTarget,
};
use mncs_vm::engine::CallTarget;
use mncs_vm::harness::tests_only::compile_corpus;
use mncs_vm::outcome::Outcome;
use mncs_vm::resource::{ResourceEnvelope, ResourceLimit};
use mncs_vm::session::{CallSpec, Session};
use mncs_vm::value::Value;

fn i64_arg(value: i64) -> mncs_model::ExecutionValue {
    mncs_model::ExecutionValue::Integer {
        value: value as i128,
        ty: mncs_model::IntegerType {
            bits: 64,
            signed: true,
        },
    }
}

fn envelope() -> ResourceEnvelope {
    ResourceEnvelope {
        limits: vec![
            ResourceLimit {
                dimension: "steps".to_owned(),
                limit: 10_000,
            },
            ResourceLimit {
                dimension: "call_depth".to_owned(),
                limit: 64,
            },
            ResourceLimit {
                dimension: "memory_cells".to_owned(),
                limit: 100_000,
            },
            ResourceLimit {
                dimension: "effects".to_owned(),
                limit: 64,
            },
        ],
    }
}

fn spec(module: &str, function: &str, args: Vec<mncs_model::ExecutionValue>) -> CallSpec {
    CallSpec {
        target: CallTarget::ByName {
            module: module.to_owned(),
            name: function.to_owned(),
        },
        arguments: args,
        type_arguments: Vec::new(),
        envelope: envelope(),
    }
}

fn bounded_config() -> DebugConfig {
    DebugConfig {
        policy: mncs_model::ExecutionObservationPolicy::bounded(
            mncs_model::ObservationCapturePolicy::Bounded,
        ),
        stops: Vec::new(),
        stop_on_abnormal_terminal: true,
    }
}

fn quiet_config() -> DebugConfig {
    DebugConfig {
        policy: mncs_model::ExecutionObservationPolicy::default(),
        stops: Vec::new(),
        stop_on_abnormal_terminal: true,
    }
}

/// First instruction identity of a named function's entry block.
fn first_instruction(admitted: &mncs_vm::admit::Admitted, name_hint: &str) -> (String, String) {
    let module = admitted.ssa_module().expect("admitted SSA");
    for function in &module.functions {
        if function.semantic_identity.0.contains(name_hint)
            || function.identity.0.contains(name_hint)
        {
            let instruction = &function.blocks[0].instructions[0];
            return (
                instruction.identity.0.clone(),
                function.identity.0.clone(),
            );
        }
    }
    panic!("no function matching {name_hint}");
}

/// Identity of the first Call instruction in a named function.
fn first_call_instruction(
    admitted: &mncs_vm::admit::Admitted,
    name_hint: &str,
) -> (String, String) {
    let module = admitted.ssa_module().expect("admitted SSA");
    for function in &module.functions {
        if function.semantic_identity.0.contains(name_hint)
            || function.identity.0.contains(name_hint)
        {
            for block in &function.blocks {
                for instruction in &block.instructions {
                    if matches!(
                        instruction.kind,
                        mncs_model::SsaInstructionKind::Call { .. }
                    ) {
                        return (
                            instruction.identity.0.clone(),
                            function.identity.0.clone(),
                        );
                    }
                }
            }
        }
    }
    panic!("no call in function matching {name_hint}");
}

fn int_returned(record: &mncs_vm::evidence::ExecutionRecord) -> i128 {
    assert_eq!(record.returned.len(), 1);
    match &record.returned[0] {
        Value::Integer { value, .. } => *value,
        other => panic!("expected integer, got {other:?}"),
    }
}

#[test]
fn debug_run_matches_oneshot() {
    let admitted = compile_corpus("arith.mncs");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let (expected_outcome, expected) = session.call(
        &caps,
        spec("mncs.vmcorpus.arith.v1", "muladd", vec![i64_arg(6), i64_arg(7), i64_arg(8)]),
    );
    assert_eq!(expected_outcome, Outcome::Completed);

    let mut session = Session::open(&admitted);
    let started = session.start_debug(
        &caps,
        spec("mncs.vmcorpus.arith.v1", "muladd", vec![i64_arg(6), i64_arg(7), i64_arg(8)]),
        bounded_config(),
    );
    let finished = match started {
        DebugStart::Finished(finished) => finished,
        DebugStart::Stopped(_, stop) => panic!("unexpected stop: {stop:?}"),
    };
    assert_eq!(finished.outcome, expected_outcome);
    assert_eq!(finished.record.returned, expected.returned);
    assert_eq!(finished.record.usage, expected.usage);
    assert_eq!(finished.record.effects, expected.effects);
    assert_eq!(finished.record.return_digest, expected.return_digest);
    assert_eq!(finished.record.effects_digest, expected.effects_digest);
    assert_eq!(int_returned(&finished.record), 50);
    // The debug run additionally retains a shared-shape observation stream.
    assert_eq!(
        finished.stream.schema_version,
        mncs_model::EXECUTION_OBSERVATION_SCHEMA_VERSION
    );
    assert!(
        finished.stream.execution_identity.0.starts_with("mncs:vm:execution:"),
        "{}",
        finished.stream.execution_identity.0
    );
    assert!(finished.record.observation.is_some());
    let kinds: Vec<&str> = finished
        .stream
        .events
        .iter()
        .map(|event| event.kind.as_str())
        .collect();
    assert!(kinds.contains(&"execution_enter"), "{kinds:?}");
    assert!(kinds.contains(&"execution_exit"), "{kinds:?}");
    assert!(kinds.contains(&"frame_enter"), "{kinds:?}");
    assert!(kinds.contains(&"operation_enter"), "{kinds:?}");
    assert!(kinds.contains(&"operation_result"), "{kinds:?}");
}

#[test]
fn operation_results_pair_with_enters_across_calls() {
    // add3 nests three calls: every operation_enter (including calls)
    // must pair with exactly one operation_result.
    let admitted = compile_corpus("arith.mncs");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let started = session.start_debug(
        &caps,
        spec("mncs.vmcorpus.arith.v1", "add3", vec![i64_arg(10)]),
        bounded_config(),
    );
    let finished = match started {
        DebugStart::Finished(finished) => finished,
        DebugStart::Stopped(_, stop) => panic!("unexpected stop: {stop:?}"),
    };
    assert_eq!(finished.outcome, Outcome::Completed);
    let enters = finished.stream.events.iter().filter(|event| event.kind == "operation_enter").count();
    let results = finished.stream.events.iter().filter(|event| event.kind == "operation_result").count();
    assert_eq!(enters, results, "enter/result pairing");
    assert!(enters > 3, "nested calls observed: {enters}");
    // Each result names the same operation as its enter.
    let mut enters_by_op = std::collections::BTreeMap::new();
    let mut results_by_op = std::collections::BTreeMap::new();
    for event in &finished.stream.events {
        if event.kind == "operation_enter" {
            *enters_by_op.entry(event.operation.clone().unwrap().0).or_insert(0) += 1;
        }
        if event.kind == "operation_result" {
            *results_by_op.entry(event.operation.clone().unwrap().0).or_insert(0) += 1;
        }
    }
    assert_eq!(enters_by_op, results_by_op);
}

#[test]
fn operation_breakpoint_stops_and_resumes() {
    let admitted = compile_corpus("arith.mncs");
    let (instruction, _) = first_instruction(&admitted, "add2");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let mut config = quiet_config();
    config.stops.push(StopCondition {
        id: "entry-stop".to_owned(),
        target: StopTarget::Operation {
            instruction: instruction.clone(),
        },
    });
    let started = session.start_debug(
        &caps,
        spec("mncs.vmcorpus.arith.v1", "add2", vec![i64_arg(3), i64_arg(4)]),
        config,
    );
    let (mut live, stop) = match started {
        DebugStart::Stopped(live, stop) => (live, stop),
        DebugStart::Finished(finished) => panic!("expected a stop, got {finished:?}"),
    };
    assert_eq!(stop.stop_sequence, 1);
    assert_eq!(stop.safe_point.kind, "operation");
    assert_eq!(stop.safe_point.instruction.as_deref(), Some(instruction.as_str()));
    assert_eq!(stop.safe_point.depth, 0);
    assert!(
        stop.reasons.iter().any(|reason| matches!(
            reason,
            StopReason::Breakpoint { condition } if condition == "entry-stop"
        )),
        "{:?}",
        stop.reasons
    );
    assert_eq!(stop.execution, live.execution_id());
    assert!(stop.transition_digest.starts_with("sha256:"));
    assert!(stop.state_digest.starts_with("sha256:"));

    // Typed inspection while stopped: arguments are visible.
    let stack = live.inspect_stack(8, 32, 4096).expect("inspect while stopped");
    assert_eq!(stack.frames.len(), 1);
    assert_eq!(stack.frames[0].depth, 0);
    assert!(stack.frames[0].values.len() >= 2, "{:?}", stack.frames[0]);
    for view in &stack.frames[0].values {
        assert!(view.instance.starts_with("mncs:vm:value:"), "{view:?}");
    }

    // Resume runs the same execution to completion: 3 + 4 = 7.
    let token = stop.continuation_token.clone();
    match live.resume(&token).expect("resume") {
        LiveEvent::Finished(finished) => {
            assert_eq!(finished.outcome, Outcome::Completed);
            assert_eq!(int_returned(&finished.record), 7);
        }
        LiveEvent::Stopped(stop) => panic!("unexpected second stop: {stop:?}"),
    }
    assert!(live.is_finished());
}

#[test]
fn stale_and_foreign_tokens_fail_closed() {
    let admitted = compile_corpus("arith.mncs");
    let (instruction, _) = first_instruction(&admitted, "add2");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let mut config = quiet_config();
    config.stops.push(StopCondition {
        id: "s".to_owned(),
        target: StopTarget::Operation { instruction },
    });
    let started = session.start_debug(
        &caps,
        spec("mncs.vmcorpus.arith.v1", "add2", vec![i64_arg(1), i64_arg(2)]),
        config,
    );
    let (mut live, stop) = match started {
        DebugStart::Stopped(live, stop) => (live, stop),
        DebugStart::Finished(_) => panic!("expected a stop"),
    };
    let token = stop.continuation_token.clone();
    // Garbage from nowhere is refused.
    assert!(matches!(
        live.resume("dbg:deadbeefdead:9:zzz"),
        Err(DebugError::BadToken { .. })
    ));
    // The live token still works after a refusal.
    match live.resume(&token).expect("resume with live token") {
        LiveEvent::Finished(finished) => assert_eq!(finished.outcome, Outcome::Completed),
        LiveEvent::Stopped(stop) => panic!("unexpected stop: {stop:?}"),
    }
    // Finished executions refuse every token.
    assert_eq!(live.resume(&token), Err(DebugError::AlreadyFinished));
}

#[test]
fn step_in_over_out_follow_frames() {
    let admitted = compile_corpus("arith.mncs");
    // add3(x) = add1(add1(add1(x))): nested calls to step through.
    let (call_instruction, _) = first_call_instruction(&admitted, "add3");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let mut config = quiet_config();
    config.stops.push(StopCondition {
        id: "call".to_owned(),
        target: StopTarget::Operation {
            instruction: call_instruction,
        },
    });
    let started = session.start_debug(
        &caps,
        spec("mncs.vmcorpus.arith.v1", "add3", vec![i64_arg(10)]),
        config,
    );
    let (mut live, stop) = match started {
        DebugStart::Stopped(live, stop) => (live, stop),
        DebugStart::Finished(_) => panic!("expected a stop at the call"),
    };
    assert_eq!(stop.safe_point.depth, 0);

    // Step into the call: the next stop is inside the callee.
    let token = stop.continuation_token.clone();
    let stop = match live.step_in(&token).expect("step in") {
        LiveEvent::Stopped(stop) => stop,
        LiveEvent::Finished(_) => panic!("step in should stop in the callee"),
    };
    assert!(
        stop.reasons.iter().any(|reason| matches!(reason, StopReason::Step)),
        "{:?}",
        stop.reasons
    );
    assert_eq!(stop.safe_point.depth, 1);
    let stack = live.inspect_stack(8, 32, 4096).expect("inspect");
    assert_eq!(stack.frames.len(), 2);
    assert_eq!(stack.frames[0].depth, 1);
    assert_eq!(stack.frames[1].depth, 0);

    // Step out: back in the caller past the call.
    let token = stop.continuation_token.clone();
    let stop = match live.step_out(&token).expect("step out") {
        LiveEvent::Stopped(stop) => stop,
        LiveEvent::Finished(_) => panic!("step out should stop in the caller"),
    };
    assert!(
        stop.reasons.iter().any(|reason| matches!(reason, StopReason::StepOut)),
        "{:?}",
        stop.reasons
    );
    assert_eq!(stop.safe_point.depth, 0);

    // Finish: 10 + 3 = 13, proving the same execution continued.
    let token = stop.continuation_token.clone();
    match live.continue_execution(&token).expect("continue") {
        LiveEvent::Finished(finished) => {
            assert_eq!(finished.outcome, Outcome::Completed);
            assert_eq!(int_returned(&finished.record), 13);
        }
        LiveEvent::Stopped(stop) => panic!("unexpected stop: {stop:?}"),
    }
}

#[test]
fn step_over_skips_nested_calls() {
    let admitted = compile_corpus("arith.mncs");
    let (call_instruction, _) = first_call_instruction(&admitted, "add3");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let mut config = quiet_config();
    config.stops.push(StopCondition {
        id: "call".to_owned(),
        target: StopTarget::Operation {
            instruction: call_instruction,
        },
    });
    let started = session.start_debug(
        &caps,
        spec("mncs.vmcorpus.arith.v1", "add3", vec![i64_arg(10)]),
        config,
    );
    let (mut live, stop) = match started {
        DebugStart::Stopped(live, stop) => (live, stop),
        DebugStart::Finished(_) => panic!("expected a stop at the call"),
    };
    let token = stop.continuation_token.clone();
    // Step over the nested add1 calls: the stop stays in add3.
    let stop = match live.step_over(&token).expect("step over") {
        LiveEvent::Stopped(stop) => stop,
        LiveEvent::Finished(finished) => panic!("step over finished early: {finished:?}"),
    };
    assert!(
        stop.reasons.iter().any(|reason| matches!(reason, StopReason::StepOver)),
        "{:?}",
        stop.reasons
    );
    assert_eq!(stop.safe_point.depth, 0);
    let token = stop.continuation_token.clone();
    match live.resume(&token).expect("resume") {
        LiveEvent::Finished(finished) => {
            assert_eq!(finished.outcome, Outcome::Completed);
            assert_eq!(int_returned(&finished.record), 13);
        }
        LiveEvent::Stopped(stop) => panic!("unexpected stop: {stop:?}"),
    }
}

#[test]
fn function_entry_stop_names_callee_entry() {
    let admitted = compile_corpus("arith.mncs");
    let (_, add1_function) = first_instruction(&admitted, "add1");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let mut config = quiet_config();
    config.stops.push(StopCondition {
        id: "enter-add1".to_owned(),
        target: StopTarget::Function {
            function: add1_function.clone(),
        },
    });
    let started = session.start_debug(
        &caps,
        spec("mncs.vmcorpus.arith.v1", "add3", vec![i64_arg(10)]),
        config,
    );
    let (mut live, stop) = match started {
        DebugStart::Stopped(live, stop) => (live, stop),
        DebugStart::Finished(_) => panic!("expected an entry stop"),
    };
    assert!(
        stop.reasons.iter().any(|reason| matches!(
            reason,
            StopReason::FunctionEntry { condition } if condition == "enter-add1"
        )),
        "{:?}",
        stop.reasons
    );
    assert_eq!(stop.safe_point.depth, 1);
    assert_eq!(stop.safe_point.function, add1_function);
    // The callee argument is already bound and inspectable.
    let stack = live.inspect_stack(8, 32, 4096).expect("inspect");
    assert_eq!(stack.frames.len(), 2);
    assert!(!stack.frames[0].values.is_empty());
    let token = stop.continuation_token.clone();
    match live.resume(&token).expect("resume") {
        LiveEvent::Finished(finished) => {
            // Entry stops re-fire per call; add3 calls add1 three times.
            // This resume may stop again at the next entry.
            let _ = finished;
        }
        LiveEvent::Stopped(next) => {
            assert!(
                next.reasons.iter().any(|reason| matches!(
                    reason,
                    StopReason::FunctionEntry { .. }
                )),
                "{:?}",
                next.reasons
            );
            let token = next.continuation_token.clone();
            // Clear the entry stop and run out.
            assert!(live.clear_stop("enter-add1").expect("clear"));
            match live.resume(&token).expect("resume") {
                LiveEvent::Finished(finished) => {
                    assert_eq!(finished.outcome, Outcome::Completed);
                    assert_eq!(int_returned(&finished.record), 13);
                }
                LiveEvent::Stopped(stop) => panic!("unexpected stop: {stop:?}"),
            }
        }
    }
}

#[test]
fn failure_suspends_at_terminal_boundary() {
    let admitted = compile_corpus("fail.mncs");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let started = session.start_debug(
        &caps,
        spec("mncs.vmcorpus.neg.v1", "div_or_fail", vec![i64_arg(1), i64_arg(0)]),
        bounded_config(),
    );
    let (mut live, stop) = match started {
        DebugStart::Stopped(live, stop) => (live, stop),
        DebugStart::Finished(finished) => panic!("failure should suspend, got {finished:?}"),
    };
    assert_eq!(stop.safe_point.kind, "terminal");
    assert!(
        stop.reasons.iter().any(|reason| matches!(reason, StopReason::FailureOrTrap)),
        "{:?}",
        stop.reasons
    );
    // The failed state is inspectable, and the stop record names the
    // pending abnormal outcome instead of only its reason class.
    assert!(
        matches!(stop.pending_terminal, Some(Outcome::ProgramFailure { .. })),
        "{:?}",
        stop.pending_terminal
    );
    let stack = live.inspect_stack(8, 32, 4096).expect("inspect failure");
    assert!(!stack.frames.is_empty());
    // Resume from a terminal stop is refused; terminate finalizes it.
    let token = stop.continuation_token.clone();
    assert!(matches!(
        live.resume(&token),
        Err(DebugError::ResumeRefusedTerminal { .. })
    ));
    let finished = live.terminate(&token).expect("terminate");
    assert!(
        matches!(finished.outcome, Outcome::ProgramFailure { .. }),
        "{:?}",
        finished.outcome
    );
    // The terminal stream carries a failure event.
    let kinds: Vec<&str> = finished
        .stream
        .events
        .iter()
        .map(|event| event.kind.as_str())
        .collect();
    assert!(kinds.contains(&"failure"), "{kinds:?}");
    assert!(kinds.contains(&"execution_exit"), "{kinds:?}");
}

#[test]
fn effect_boundary_stop_dispatches_once() {
    let admitted = compile_corpus("clock.mncs");
    let env = CapabilityEnv::empty().bind(
        "clock_capability",
        ConstProvider {
            identity: "clock-stub".to_owned(),
            outputs: vec![Value::Integer {
                value: 1_700_000_000,
                bits: 64,
                signed: false,
            }],
        },
    );
    let mut session = Session::open(&admitted);
    let mut config = bounded_config();
    config.stops.push(StopCondition {
        id: "before-effect".to_owned(),
        target: StopTarget::EffectBoundary {
            phase: "before".to_owned(),
        },
    });
    let started = session.start_debug(
        &env,
        CallSpec {
            target: CallTarget::ByName {
                module: "mncs.vmcorpus.clock.v1".to_owned(),
                name: "read_clock".to_owned(),
            },
            arguments: Vec::new(),
            type_arguments: Vec::new(),
            envelope: envelope(),
        },
        config,
    );
    let (mut live, stop) = match started {
        DebugStart::Stopped(live, stop) => (live, stop),
        DebugStart::Finished(finished) => panic!("effect stop expected, got {finished:?}"),
    };
    assert_eq!(stop.safe_point.kind, "operation");
    assert!(
        stop.reasons.iter().any(|reason| matches!(
            reason,
            StopReason::EffectBoundary { phase, .. } if phase == "before"
        )),
        "{:?}",
        stop.reasons
    );
    // Nothing dispatched yet: the effect log is empty at the stop.
    assert_eq!(live.inspect_effects().len(), 0);
    let token = stop.continuation_token.clone();
    match live.resume(&token).expect("resume") {
        LiveEvent::Finished(finished) => {
            assert_eq!(finished.outcome, Outcome::Completed);
            // Exactly one dispatch across stop + resume: suspension
            // never repeats an effect.
            assert_eq!(finished.record.effects.len(), 1);
            assert_eq!(finished.record.effects[0].outcome_tag, "completed");
        }
        LiveEvent::Stopped(stop) => panic!("unexpected stop: {stop:?}"),
    }
}

#[test]
fn effect_after_stop_continues_past_dispatch() {
    let admitted = compile_corpus("clock.mncs");
    let env = CapabilityEnv::empty().bind(
        "clock_capability",
        ConstProvider {
            identity: "clock-stub".to_owned(),
            outputs: vec![Value::Integer {
                value: 42,
                bits: 64,
                signed: false,
            }],
        },
    );
    let mut session = Session::open(&admitted);
    let mut config = quiet_config();
    config.stops.push(StopCondition {
        id: "after-effect".to_owned(),
        target: StopTarget::EffectBoundary {
            phase: "after".to_owned(),
        },
    });
    let started = session.start_debug(
        &env,
        CallSpec {
            target: CallTarget::ByName {
                module: "mncs.vmcorpus.clock.v1".to_owned(),
                name: "read_clock".to_owned(),
            },
            arguments: Vec::new(),
            type_arguments: Vec::new(),
            envelope: envelope(),
        },
        config,
    );
    let (mut live, stop) = match started {
        DebugStart::Stopped(live, stop) => (live, stop),
        DebugStart::Finished(finished) => panic!("effect stop expected, got {finished:?}"),
    };
    assert_eq!(stop.safe_point.kind, "effect_after");
    // The dispatch already happened exactly once.
    assert_eq!(live.inspect_effects().len(), 1);
    let token = stop.continuation_token.clone();
    match live.resume(&token).expect("resume") {
        LiveEvent::Finished(finished) => {
            assert_eq!(finished.outcome, Outcome::Completed);
            assert_eq!(finished.record.effects.len(), 1);
            assert_eq!(finished.record.effects[0].outcome_tag, "completed");
        }
        LiveEvent::Stopped(stop) => panic!("unexpected stop: {stop:?}"),
    }
}

#[test]
fn selected_capture_scopes_values_to_selection() {
    let admitted = compile_corpus("arith.mncs");
    let (instruction, _) = first_instruction(&admitted, "muladd");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let mut selected = BTreeSet::new();
    selected.insert(mncs_model::SemanticId(instruction.clone()));
    let mut config = DebugConfig {
        policy: mncs_model::ExecutionObservationPolicy {
            schema_version: mncs_model::EXECUTION_OBSERVATION_POLICY_SCHEMA_VERSION.to_owned(),
            capture: mncs_model::ObservationCapturePolicy::Selected,
            max_events: 256,
            max_values: 128,
            max_value_bytes: 4096,
            include_frames: true,
            include_effects: true,
            selected_operations: selected,
        },
        stops: Vec::new(),
        stop_on_abnormal_terminal: false,
    };
    config.stops.push(StopCondition {
        id: "s".to_owned(),
        target: StopTarget::Operation { instruction: instruction.clone() },
    });
    let started = session.start_debug(
        &caps,
        spec("mncs.vmcorpus.arith.v1", "muladd", vec![i64_arg(2), i64_arg(3), i64_arg(4)]),
        config,
    );
    let (mut live, stop) = match started {
        DebugStart::Stopped(live, stop) => (live, stop),
        DebugStart::Finished(_) => panic!("expected a stop"),
    };
    let token = stop.continuation_token.clone();
    let finished = match live.resume(&token).expect("resume") {
        LiveEvent::Finished(finished) => finished,
        LiveEvent::Stopped(stop) => panic!("unexpected stop: {stop:?}"),
    };
    assert_eq!(finished.outcome, Outcome::Completed);
    assert_eq!(int_returned(&finished.record), 10);
    // Every retained operation-scoped event names the selection, and
    // every retained value belongs to it: no ambient value universe.
    for event in &finished.stream.events {
        if let Some(operation) = &event.operation {
            assert_eq!(operation.0, instruction, "event {event:?}");
        }
    }
    for value in &finished.stream.values {
        let operation = value.operation.as_ref().expect("selected value names its operation");
        assert_eq!(operation.0, instruction, "value {value:?}");
    }
    assert!(!finished.stream.events.is_empty());
}

#[test]
fn value_identity_is_stable_and_deterministic() {
    let admitted = compile_corpus("arith.mncs");
    let (instruction, _) = first_instruction(&admitted, "add2");
    let caps = CapabilityEnv::empty();
    let run_once = |session: &mut Session| {
        let mut config = quiet_config();
        config.stops.push(StopCondition {
            id: "s".to_owned(),
            target: StopTarget::Operation {
                instruction: instruction.clone(),
            },
        });
        let started = session.start_debug(
            &caps,
            spec("mncs.vmcorpus.arith.v1", "add2", vec![i64_arg(3), i64_arg(4)]),
            config,
        );
        let (mut live, stop) = match started {
            DebugStart::Stopped(live, stop) => (live, stop),
            DebugStart::Finished(_) => panic!("expected a stop"),
        };
        let first = live.inspect_stack(8, 32, 4096).expect("inspect");
        let second = live.inspect_stack(8, 32, 4096).expect("inspect again");
        let instances: Vec<String> = first.frames[0]
            .values
            .iter()
            .map(|view| view.instance.clone())
            .collect();
        let again: Vec<String> = second.frames[0]
            .values
            .iter()
            .map(|view| view.instance.clone())
            .collect();
        assert_eq!(instances, again, "repeated inspection is stable");
        (live.execution_id().to_owned(), stop.transition_digest.clone(), instances)
    };
    let mut first_session = Session::open(&admitted);
    let (first_exec, first_digest, first_instances) = run_once(&mut first_session);
    let mut second_session = Session::open(&admitted);
    let (second_exec, second_digest, second_instances) = run_once(&mut second_session);
    // Identical admitted inputs name the same execution and produce
    // the same stop digests and value identities: replay-grade.
    assert_eq!(first_exec, second_exec);
    assert_eq!(first_digest, second_digest);
    assert_eq!(first_instances, second_instances);
}

#[test]
fn terminate_mid_run_records_termination() {
    let admitted = compile_corpus("arith.mncs");
    let (instruction, _) = first_instruction(&admitted, "add2");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let mut config = quiet_config();
    config.stops.push(StopCondition {
        id: "s".to_owned(),
        target: StopTarget::Operation { instruction },
    });
    let started = session.start_debug(
        &caps,
        spec("mncs.vmcorpus.arith.v1", "add2", vec![i64_arg(3), i64_arg(4)]),
        config,
    );
    let (mut live, stop) = match started {
        DebugStart::Stopped(live, stop) => (live, stop),
        DebugStart::Finished(_) => panic!("expected a stop"),
    };
    let token = stop.continuation_token.clone();
    let finished = live.terminate(&token).expect("terminate");
    assert!(
        matches!(finished.outcome, Outcome::Terminated { .. }),
        "{:?}",
        finished.outcome
    );
    // Terminated state stays inspectable.
    let stack = live.inspect_stack(8, 32, 4096).expect("inspect after terminate");
    assert!(!stack.frames.is_empty());
}

#[test]
fn stop_binding_is_validated() {
    let admitted = compile_corpus("arith.mncs");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let started = session.start_debug(
        &caps,
        spec("mncs.vmcorpus.arith.v1", "add2", vec![i64_arg(3), i64_arg(4)]),
        DebugConfig {
            policy: mncs_model::ExecutionObservationPolicy::default(),
            stops: Vec::new(),
            stop_on_abnormal_terminal: false,
        },
    );
    // No stops and no abnormal terminal: runs straight through.
    match started {
        DebugStart::Finished(finished) => {
            assert_eq!(finished.outcome, Outcome::Completed);
        }
        DebugStart::Stopped(_, stop) => panic!("unexpected stop: {stop:?}"),
    }

    // Mid-run binding and validation on a stopped execution.
    let (instruction, _) = first_instruction(&admitted, "add2");
    let mut session = Session::open(&admitted);
    let mut config = quiet_config();
    config.stops.push(StopCondition {
        id: "s".to_owned(),
        target: StopTarget::Operation {
            instruction: instruction.clone(),
        },
    });
    let started = session.start_debug(
        &caps,
        spec("mncs.vmcorpus.arith.v1", "add2", vec![i64_arg(3), i64_arg(4)]),
        config,
    );
    let (mut live, stop) = match started {
        DebugStart::Stopped(live, stop) => (live, stop),
        DebugStart::Finished(_) => panic!("expected a stop"),
    };
    assert!(matches!(
        live.bind_stop(
            StopTarget::EffectBoundary {
                phase: "sometimes".to_owned()
            },
            None
        ),
        Err(DebugError::InvalidStop { .. })
    ));
    assert!(matches!(
        live.bind_stop(StopTarget::Operation { instruction: String::new() }, None),
        Err(DebugError::InvalidStop { .. })
    ));
    let bound = live
        .bind_stop(
            StopTarget::Operation { instruction },
            Some("late".to_owned()),
        )
        .expect("bind mid-run");
    assert_eq!(bound.id, "late");
    assert_eq!(live.list_stops().len(), 2);
    assert!(!live.clear_stop("missing").expect("clear missing"));
    assert!(live.clear_stop("late").expect("clear late"));
    assert_eq!(live.list_stops().len(), 1);
    let token = stop.continuation_token.clone();
    match live.resume(&token).expect("resume") {
        LiveEvent::Finished(finished) => assert_eq!(finished.outcome, Outcome::Completed),
        LiveEvent::Stopped(stop) => panic!("unexpected stop: {stop:?}"),
    }
}

#[test]
fn debug_contract_reports_its_shape() {
    let kinds = mncs_vm::debug::observation_event_kinds();
    assert!(kinds.contains(&"operation_enter".to_owned()));
    assert!(kinds.contains(&"effect_invoke".to_owned()));
    let targets = mncs_vm::debug::stop_target_names();
    assert!(targets.contains(&"operation".to_owned()));
    assert!(targets.contains(&"function".to_owned()));
    let unsupported = mncs_vm::debug::unsupported_debug_capabilities();
    assert!(!unsupported.is_empty());
    assert!(
        unsupported.iter().any(|entry| entry.get("capability").is_some_and(|name| name == "live_watch_stop")),
        "{unsupported:?}"
    );
}

#[test]
fn debug_identities_are_derivation_stable() {
    // Golden identities for a fixed scenario: performance work may
    // change HOW identities derive, never WHAT they derive to.
    // Historical witnesses stay comparable across VM versions.
    let mut admitted = compile_corpus("arith.mncs");
    // Freeze the historical artifact identity for this derivation witness.
    // Direct lowering/encoding legitimately changes artifact content identity;
    // the debug identity algorithm must still reproduce this fixed scenario.
    admitted.artifact.artifact_id = "sha256:c453e45c682a425eea6a56598005bff1a2aea8840f7d117e955f6f10fedaa7ab".into();
    let (instruction, _) = first_instruction(&admitted, "add2");
    let caps = CapabilityEnv::empty();
    let mut session = Session::open(&admitted);
    let mut config = bounded_config();
    config.stops.push(StopCondition {
        id: "s".to_owned(),
        target: StopTarget::Operation { instruction },
    });
    let started = session.start_debug(
        &caps,
        spec("mncs.vmcorpus.arith.v1", "add2", vec![i64_arg(3), i64_arg(4)]),
        config,
    );
    let (mut live, stop) = match started {
        DebugStart::Stopped(live, stop) => (live, stop),
        DebugStart::Finished(_) => panic!("expected a stop"),
    };
    assert_eq!(
        live.execution_id(),
        "mncs:vm:execution:be70b9436eb70552ffaa1abe78cf5640ee9a0f85774713edf9aef19d81f4e0e8"
    );
    assert_eq!(
        stop.transition_digest,
        "sha256:d6836a52be08f295d91b8c470366e382ce3f361d987b906e05dd201c7b67a9c2"
    );
    assert_eq!(
        stop.state_digest,
        "sha256:802bee6f58f96c99360b892d220ab49b1e8047064f0cef70dfabadb32dd6245b"
    );
    let stack = live.inspect_stack(8, 32, 4096).expect("inspect");
    let instances: Vec<&str> = stack.frames[0]
        .values
        .iter()
        .map(|view| view.instance.as_str())
        .collect();
    assert_eq!(
        instances,
        [
            "mncs:vm:value:151bf39dbbe637e2374c0a910514e60e53841503e2cc844d7a02037b39bce58e",
            "mncs:vm:value:a229a31169985680db664182797c7a94286eb337955c55e3d2daaefaa1058349",
        ]
    );
    let stream = live.inspect_observation();
    let event_ids: Vec<(&str, &str)> = stream
        .events
        .iter()
        .take(4)
        .map(|event| (event.kind.as_str(), event.identity.0.as_str()))
        .collect();
    assert_eq!(
        event_ids,
        [
            ("execution_enter", "mncs:vm:event:3124a32baefe"),
            ("frame_enter", "mncs:vm:event:12370e7820e8"),
            ("block_enter", "mncs:vm:event:76ff556776f7"),
            ("operation_enter", "mncs:vm:event:b151e2dea353"),
        ]
    );
}
