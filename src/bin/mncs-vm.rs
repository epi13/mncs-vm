//! `mncs-vm`: canonical VM runner and live-debug driver.
//!
//! - `mncs-vm run`: one-shot execution of an admitted artifact with
//!   an optional observation policy. Prints the outcome + record.
//! - `mncs-vm debug`: JSONL live-debug driver over stdio (`--stdio`)
//!   or a Unix socket (`--serve PATH`). One execution at a time;
//!   every request and response is a typed JSON document
//!   (`mncs.vm.debug-cli/1`). This is the process boundary the
//!   debugger drives: the execution lives in this process across
//!   requests, so stops, inspection, and resume are truthful.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use mncs_vm::admit::{admit, Admitted};
use mncs_vm::capability::{CapabilityEnv, ConstProvider};
use mncs_vm::debug::{
    DebugConfig, DebugError, DebugStart, LiveEvent, StopTarget,
};
use mncs_vm::engine::CallTarget;
use mncs_vm::outcome::Outcome;
use mncs_vm::resource::{ResourceEnvelope, ResourceLimit};
use mncs_vm::session::{CallSpec, Session};
use mncs_vm::value::from_wire;

const CLI_SCHEMA_VERSION: &str = "mncs.vm.debug-cli/1";

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let command = argv.get(1).map(String::as_str).unwrap_or("help");
    let code = match command {
        "run" => cmd_run(&argv[2..]),
        "compile" => cmd_compile(&argv[2..]),
        "debug" => cmd_debug(&argv[2..]),
        "version" => {
            println!("mncs-vm {}", env!("CARGO_PKG_VERSION"));
            0
        }
        _ => {
            print_help();
            if command == "help" { 0 } else { 2 }
        }
    };
    std::process::exit(code);
}

fn print_help() {
    println!(
        "mncs-vm: canonical MNCS VM runner and live-debug driver\n\nusage:\n  \
         mncs-vm run --artifact FILE|--compile FILE --callable MODULE::NAME|--function ID\n    \
         [--args FILE] [--envelope FILE] [--observe POLICY] [--provider CAP=FILE]...\n    \
         [--output FILE]\n  mncs-vm compile FILE --output FILE\n  mncs-vm debug --stdio\n  mncs-vm debug --serve SOCKET\n  mncs-vm version"
    );
}

// ---------------------------------------------------------------------------
// run: one-shot execution
// ---------------------------------------------------------------------------

fn cmd_run(argv: &[String]) -> i32 {
    let mut artifact_path: Option<PathBuf> = None;
    let mut compile_path: Option<PathBuf> = None;
    let mut callable: Option<String> = None;
    let mut function: Option<String> = None;
    let mut args_path: Option<PathBuf> = None;
    let mut envelope_path: Option<PathBuf> = None;
    let mut observe_path: Option<PathBuf> = None;
    let mut providers: Vec<(String, PathBuf)> = Vec::new();
    let mut output_path: Option<PathBuf> = None;
    let mut index = 0;
    while index < argv.len() {
        let key = argv[index].as_str();
        let value = |position: usize| -> Option<&str> {
            if position + 1 < argv.len() {
                Some(argv[position + 1].as_str())
            } else {
                None
            }
        };
        match key {
            "--artifact" => {
                let Some(path) = value(index) else { return usage_error("run --artifact needs a path") };
                artifact_path = Some(PathBuf::from(path));
                index += 2;
            }
            "--compile" => {
                let Some(path) = value(index) else { return usage_error("run --compile needs a path") };
                compile_path = Some(PathBuf::from(path));
                index += 2;
            }
            "--callable" => {
                let Some(name) = value(index) else { return usage_error("run --callable needs MODULE::NAME") };
                callable = Some(name.to_owned());
                index += 2;
            }
            "--function" => {
                let Some(name) = value(index) else { return usage_error("run --function needs an identity") };
                function = Some(name.to_owned());
                index += 2;
            }
            "--args" => {
                let Some(path) = value(index) else { return usage_error("run --args needs a path") };
                args_path = Some(PathBuf::from(path));
                index += 2;
            }
            "--envelope" => {
                let Some(path) = value(index) else { return usage_error("run --envelope needs a path") };
                envelope_path = Some(PathBuf::from(path));
                index += 2;
            }
            "--observe" => {
                let Some(path) = value(index) else { return usage_error("run --observe needs a path") };
                observe_path = Some(PathBuf::from(path));
                index += 2;
            }
            "--provider" => {
                let Some(binding) = value(index) else { return usage_error("run --provider needs CAP=FILE") };
                let Some((capability, path)) = binding.split_once('=') else {
                    return usage_error("run --provider needs CAP=FILE");
                };
                providers.push((capability.to_owned(), PathBuf::from(path)));
                index += 2;
            }
            "--output" => {
                let Some(path) = value(index) else { return usage_error("run --output needs a path") };
                output_path = Some(PathBuf::from(path));
                index += 2;
            }
            other => return usage_error(&format!("run: unknown option {other}")),
        }
    }
    if artifact_path.is_some() == compile_path.is_some() {
        return usage_error("run needs exactly one of --artifact or --compile");
    }
    if callable.is_some() == function.is_some() {
        return usage_error("run needs exactly one of --callable or --function");
    }
    let admitted = match load_admitted(artifact_path.as_deref(), compile_path.as_deref()) {
        Ok(admitted) => admitted,
        Err(message) => return run_error(&message),
    };
    let target = match (callable, function) {
        (Some(name), None) => {
            let Some((module, callable_name)) = name.split_once("::") else {
                return usage_error("run --callable needs MODULE::NAME");
            };
            CallTarget::ByName {
                module: module.to_owned(),
                name: callable_name.to_owned(),
            }
        }
        (None, Some(identity)) => CallTarget::ByFunction { function: identity },
        _ => unreachable!("validated above"),
    };
    let arguments = match load_json_array(args_path.as_deref()) {
        Ok(arguments) => arguments,
        Err(message) => return run_error(&message),
    };
    let envelope = match load_envelope(envelope_path.as_deref()) {
        Ok(envelope) => envelope,
        Err(message) => return run_error(&message),
    };
    let caps = match load_capabilities(&providers) {
        Ok(caps) => caps,
        Err(message) => return run_error(&message),
    };
    let mut session = Session::open(&admitted);
    let document = if let Some(policy_path) = observe_path {
        let policy = match load_policy(&policy_path) {
            Ok(policy) => policy,
            Err(message) => return run_error(&message),
        };
        let config = DebugConfig {
            policy,
            stops: Vec::new(),
            stop_on_abnormal_terminal: false,
        };
        let spec = CallSpec {
            target,
            arguments,
            envelope,
        };
        match session.start_debug(&caps, spec, config) {
            DebugStart::Finished(finished) => {
                serde_json::json!({
                    "outcome": finished.outcome,
                    "record": finished.record,
                })
            }
            DebugStart::Stopped(_, stop) => {
                return run_error(&format!(
                    "unexpected stop with no conditions: {}",
                    stop.stop_sequence
                ));
            }
        }
    } else {
        let spec = CallSpec {
            target,
            arguments,
            envelope,
        };
        let (outcome, record) = session.call(&caps, spec);
        serde_json::json!({
            "outcome": outcome,
            "record": record,
        })
    };
    let text = serde_json::to_string_pretty(&document).unwrap_or_default();
    if let Some(path) = output_path {
        if let Err(error) = std::fs::write(&path, format!("{text}\n")) {
            return run_error(&format!("cannot write {}: {error}", path.display()));
        }
    } else {
        println!("{text}");
    }
    0
}

fn usage_error(message: &str) -> i32 {
    eprintln!("mncs-vm: {message}");
    2
}

// ---------------------------------------------------------------------------
// compile: admit-once artifact emission for reuse
// ---------------------------------------------------------------------------

fn cmd_compile(argv: &[String]) -> i32 {
    let mut source: Option<&str> = None;
    let mut output: Option<&str> = None;
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "--output" => {
                index += 1;
                output = argv.get(index).map(String::as_str);
                if output.is_none() {
                    return usage_error("compile --output needs a path");
                }
            }
            flag if flag.starts_with('-') => {
                return usage_error(&format!("compile: unknown option {flag}"));
            }
            positional => {
                if source.is_some() {
                    return usage_error("compile takes exactly one source file");
                }
                source = Some(positional);
            }
        }
        index += 1;
    }
    let (Some(source), Some(output)) = (source, output) else {
        return usage_error("usage: mncs-vm compile FILE --output FILE");
    };
    let admitted = match mncs_vm::harness::compile_file(std::path::Path::new(source)) {
        Ok(admitted) => admitted,
        Err(error) => return run_error(&format!("cannot compile {source}: {error}")),
    };
    // Sealed bytes (identity bound), not canonical bytes (identity
    // blanked): admission verifies the sealed identity on load.
    let bytes = serde_json::to_vec(&admitted.artifact).expect("artifact is serializable");
    if let Err(error) = std::fs::write(output, &bytes) {
        return run_error(&format!("cannot write {output}: {error}"));
    }
    println!(
        "{}",
        serde_json::json!({
            "artifact_id": admitted.artifact_id(),
            "bytes": bytes.len(),
            "output": output,
        })
    );
    0
}

fn run_error(message: &str) -> i32 {
    let document = serde_json::json!({
        "outcome": Outcome::InvalidRequest { reason: message.to_owned() },
    });
    println!("{}", serde_json::to_string_pretty(&document).unwrap_or_default());
    1
}

fn load_admitted(artifact: Option<&Path>, compile: Option<&Path>) -> Result<Admitted, String> {
    if let Some(path) = artifact {
        let bytes = std::fs::read(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        return admit(&bytes).map_err(|refusal| format!("admission refused: {refusal}"));
    }
    let path = compile.expect("one source is set");
    mncs_vm::harness::compile_file(path)
        .map_err(|error| format!("cannot compile {}: {error}", path.display()))
}

/// Default CLI envelope: generous but explicit. The record carries
/// these limits so the bound is evidence, not folklore.
fn default_envelope() -> ResourceEnvelope {
    ResourceEnvelope {
        limits: vec![
            ResourceLimit {
                dimension: "steps".to_owned(),
                limit: 1_000_000,
            },
            ResourceLimit {
                dimension: "call_depth".to_owned(),
                limit: 1_024,
            },
            ResourceLimit {
                dimension: "memory_cells".to_owned(),
                limit: 10_000_000,
            },
            ResourceLimit {
                dimension: "effects".to_owned(),
                limit: 1_024,
            },
            ResourceLimit {
                dimension: "iterations".to_owned(),
                limit: 1_000_000,
            },
        ],
    }
}

fn load_envelope(path: Option<&Path>) -> Result<ResourceEnvelope, String> {
    let Some(path) = path else {
        return Ok(default_envelope());
    };
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("bad envelope {}: {error}", path.display()))
}

fn load_json_array(path: Option<&Path>) -> Result<Vec<mncs_model::ExecutionValue>, String> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("bad args {}: {error}", path.display()))
}

fn load_policy(path: &Path) -> Result<mncs_model::ExecutionObservationPolicy, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("bad policy {}: {error}", path.display()))
}

fn load_capabilities(providers: &[(String, PathBuf)]) -> Result<CapabilityEnv, String> {
    let mut caps = CapabilityEnv::empty();
    for (capability, path) in providers {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let document: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| format!("bad provider {}: {error}", path.display()))?;
        let identity = document
            .get("identity")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("cli-const-provider")
            .to_owned();
        let outputs: Vec<mncs_model::ExecutionValue> = document
            .get("outputs")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .map_err(|error| format!("bad provider outputs {}: {error}", path.display()))?
            .unwrap_or_default();
        caps = caps.bind(
            capability,
            ConstProvider {
                identity,
                outputs: outputs.iter().map(from_wire).collect(),
            },
        );
    }
    Ok(caps)
}

// ---------------------------------------------------------------------------
// debug: JSONL live-debug driver
// ---------------------------------------------------------------------------

fn cmd_debug(argv: &[String]) -> i32 {
    let mut stdio = false;
    let mut serve: Option<PathBuf> = None;
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "--stdio" => {
                stdio = true;
                index += 1;
            }
            "--serve" => {
                if index + 1 >= argv.len() {
                    return usage_error("debug --serve needs a socket path");
                }
                serve = Some(PathBuf::from(&argv[index + 1]));
                index += 2;
            }
            other => return usage_error(&format!("debug: unknown option {other}")),
        }
    }
    match (stdio, serve) {
        (true, None) => serve_stdio(),
        (false, Some(path)) => serve_socket(&path),
        _ => usage_error("debug needs exactly one of --stdio or --serve PATH"),
    }
}

fn serve_stdio() -> i32 {
    let mut handler = DebugHandler::new();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let (response, quit) = handler.handle_line(&line);
        writeln!(stdout, "{response}").ok();
        stdout.flush().ok();
        if quit {
            break;
        }
    }
    0
}

#[cfg(unix)]
fn serve_socket(path: &Path) -> i32 {
    use std::os::unix::net::{UnixListener, UnixStream};
    // Refuse to steal a live daemon's socket; retire a stale file.
    match UnixStream::connect(path) {
        Ok(_) => {
            eprintln!("mncs-vm: socket {} is already served", path.display());
            return 1;
        }
        Err(_) => {
            let _ = std::fs::remove_file(path);
        }
    }
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("mncs-vm: cannot serve {}: {error}", path.display());
            return 1;
        }
    };
    eprintln!("mncs-vm: serving debug socket {}", path.display());
    let mut handler = DebugHandler::new();
    for connection in listener.incoming() {
        let stream = match connection {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        // One connection at a time: the debug handle is single-owner,
        // and short debugger connections never hold the socket long.
        // Clients must use one request-response round trip per
        // connection and close promptly; a client that holds a
        // connection open across operations starves every other
        // client until it reaches EOF.
        let reader = BufReader::new(stream.try_clone().unwrap_or_else(|_| {
            panic!("socket clone failed while serving {}", path.display())
        }));
        let mut writer = stream;
        let mut quit = false;
        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(_) => break,
            };
            if line.trim().is_empty() {
                continue;
            }
            let (response, done) = handler.handle_line(&line);
            if writeln!(writer, "{response}").is_err() {
                break;
            }
            writer.flush().ok();
            if done {
                quit = true;
                break;
            }
        }
        if quit {
            break;
        }
    }
    let _ = std::fs::remove_file(path);
    0
}

#[cfg(not(unix))]
fn serve_socket(path: &Path) -> i32 {
    eprintln!("mncs-vm: --serve needs a Unix socket host (not {})", path.display());
    1
}

/// One debug handle: at most one live execution. The admitted
/// artifact and capability environment are process-lifetime leaks by
/// construction (a daemon owns few executions and exits with them).
struct DebugHandler {
    live: Option<Box<mncs_vm::debug::LiveExecution<'static>>>,
}

impl DebugHandler {
    fn new() -> Self {
        Self { live: None }
    }

    fn handle_line(&mut self, line: &str) -> (String, bool) {
        let request: serde_json::Value = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => {
                return (
                    error_response(
                        &serde_json::Value::Null,
                        "bad_request",
                        &format!("request is not JSON: {error}"),
                    ),
                    false,
                );
            }
        };
        let id = request.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let op = request.get("op").and_then(serde_json::Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(serde_json::Value::Null);
        match op {
            "start" => (self.op_start(&id, &params), false),
            "resume" | "continue" => (self.op_drive(&id, &params, op), false),
            "step_in" | "step_over" | "step_out" => (self.op_drive(&id, &params, op), false),
            "inspect" => (self.op_inspect(&id, &params), false),
            "bind_stop" => (self.op_bind_stop(&id, &params), false),
            "clear_stop" => (self.op_clear_stop(&id, &params), false),
            "terminate" => (self.op_terminate(&id, &params), false),
            "capabilities" => (self.op_capabilities(&id), false),
            "close" | "shutdown" => (ok_response(&id, serde_json::json!({})), true),
            _ => (error_response(&id, "unknown_op", &format!("unknown op {op:?}")), false),
        }
    }

    fn op_start(&mut self, id: &serde_json::Value, params: &serde_json::Value) -> String {
        if self.live.as_ref().is_some_and(|live| !live.is_finished()) {
            return error_response(id, "execution_active", "one live execution per debug handle");
        }
        let admitted = match start_admitted(params) {
            Ok(admitted) => admitted,
            Err(message) => return error_response(id, "admission_refused", &message),
        };
        let target = match start_target(params) {
            Ok(target) => target,
            Err(message) => return error_response(id, "invalid_request", &message),
        };
        let arguments: Vec<mncs_model::ExecutionValue> = match params
            .get("arguments")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
        {
            Ok(arguments) => arguments.unwrap_or_default(),
            Err(error) => return error_response(id, "invalid_request", &format!("bad arguments: {error}")),
        };
        let envelope: ResourceEnvelope = match params
            .get("envelope")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
        {
            Ok(envelope) => envelope.unwrap_or_else(default_envelope),
            Err(error) => return error_response(id, "invalid_request", &format!("bad envelope: {error}")),
        };
        let caps = match start_capabilities(params) {
            Ok(caps) => caps,
            Err(message) => return error_response(id, "invalid_request", &message),
        };
        let config: DebugConfig = match params.get("debug").map(|value| serde_json::from_value(value.clone())).transpose() {
            Ok(config) => config.unwrap_or_default(),
            Err(error) => return error_response(id, "invalid_request", &format!("bad debug config: {error}")),
        };
        // Process-lifetime by construction: the daemon owns few
        // executions and all state dies with it.
        let admitted: &'static Admitted = Box::leak(Box::new(admitted));
        let caps: &'static CapabilityEnv = Box::leak(Box::new(caps));
        let session: &'static mut Session = Box::leak(Box::new(Session::open(admitted)));
        let include_stream = params
            .get("include_stream")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        match session.start_debug(caps, CallSpec { target, arguments, envelope }, config) {
            DebugStart::Stopped(live, stop) => {
                self.live = Some(live);
                ok_response(id, serde_json::json!({"event": "stopped", "stop": stop}))
            }
            DebugStart::Finished(finished) => ok_response(id, finish_json(&finished, include_stream)),
        }
    }

    fn live_mut(&mut self, id: &serde_json::Value) -> Result<&mut Box<mncs_vm::debug::LiveExecution<'static>>, String> {
        match self.live.as_mut() {
            Some(live) => Ok(live),
            None => Err(error_response(id, "no_execution", "no live execution; send start first")),
        }
    }

    fn op_drive(&mut self, id: &serde_json::Value, params: &serde_json::Value, op: &str) -> String {
        let token = params.get("token").and_then(serde_json::Value::as_str).unwrap_or("");
        let include_stream = params
            .get("include_stream")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let live = match self.live_mut(id) {
            Ok(live) => live,
            Err(response) => return response,
        };
        let event = match op {
            "resume" | "continue" => live.resume(token),
            "step_in" => live.step_in(token),
            "step_over" => live.step_over(token),
            "step_out" => live.step_out(token),
            _ => unreachable!("dispatched above"),
        };
        match event {
            Ok(LiveEvent::Stopped(stop)) => {
                ok_response(id, serde_json::json!({"event": "stopped", "stop": stop}))
            }
            Ok(LiveEvent::Finished(finished)) => ok_response(id, finish_json(&finished, include_stream)),
            Err(error) => error_response(id, &debug_code(&error), &error.to_string()),
        }
    }

    fn op_inspect(&mut self, id: &serde_json::Value, params: &serde_json::Value) -> String {
        // Inspection is read-only, but the daemon still gates it on
        // the current token so concurrent clients cannot interleave.
        if let Some(live) = self.live.as_ref() {
            if !live.is_finished() {
                let token = params.get("token").and_then(serde_json::Value::as_str).unwrap_or("");
                if !live_token_matches(live, token) {
                    return error_response(id, "stale_token", "inspection needs the current continuation token");
                }
            }
        }
        let live = match self.live_mut(id) {
            Ok(live) => live,
            Err(response) => return response,
        };
        let view = params.get("view").and_then(serde_json::Value::as_str).unwrap_or("stack");
        match view {
            "stack" => {
                let max_frames = params.get("max_frames").and_then(serde_json::Value::as_u64).unwrap_or(16) as usize;
                let max_values = params.get("max_values").and_then(serde_json::Value::as_u64).unwrap_or(64) as usize;
                let max_value_bytes = params.get("max_value_bytes").and_then(serde_json::Value::as_u64).unwrap_or(4096) as usize;
                match live.inspect_stack(max_frames, max_values, max_value_bytes) {
                    Ok(stack) => ok_response(id, serde_json::json!({"stack": stack})),
                    Err(error) => error_response(id, &debug_code(&error), &error.to_string()),
                }
            }
            "observation" => ok_response(id, serde_json::json!({"observation": live.inspect_observation()})),
            "effects" => ok_response(id, serde_json::json!({"effects": live.inspect_effects()})),
            "stops" => ok_response(id, serde_json::json!({"stops": live.list_stops()})),
            _ => error_response(id, "invalid_request", &format!("unknown inspect view {view:?}")),
        }
    }

    fn op_bind_stop(&mut self, id: &serde_json::Value, params: &serde_json::Value) -> String {
        let token = params.get("token").and_then(serde_json::Value::as_str).unwrap_or("");
        let live = match self.live_mut(id) {
            Ok(live) => live,
            Err(response) => return response,
        };
        if !live.is_finished() && !live_token_matches(live, token) {
            return error_response(id, "stale_token", "bind_stop needs the current continuation token");
        }
        let target: StopTarget = match params.get("target").map(|value| serde_json::from_value(value.clone())).transpose() {
            Ok(Some(target)) => target,
            Ok(None) => return error_response(id, "invalid_request", "bind_stop needs a target"),
            Err(error) => return error_response(id, "invalid_request", &format!("bad target: {error}")),
        };
        let name = params.get("id").and_then(serde_json::Value::as_str).map(str::to_owned);
        match live.bind_stop(target, name) {
            Ok(condition) => ok_response(id, serde_json::json!({"condition": condition})),
            Err(error) => error_response(id, &debug_code(&error), &error.to_string()),
        }
    }

    fn op_clear_stop(&mut self, id: &serde_json::Value, params: &serde_json::Value) -> String {
        let token = params.get("token").and_then(serde_json::Value::as_str).unwrap_or("");
        let live = match self.live_mut(id) {
            Ok(live) => live,
            Err(response) => return response,
        };
        if !live.is_finished() && !live_token_matches(live, token) {
            return error_response(id, "stale_token", "clear_stop needs the current continuation token");
        }
        let name = params.get("id").and_then(serde_json::Value::as_str).unwrap_or("");
        match live.clear_stop(name) {
            Ok(cleared) => ok_response(id, serde_json::json!({"cleared": cleared})),
            Err(error) => error_response(id, &debug_code(&error), &error.to_string()),
        }
    }

    fn op_terminate(&mut self, id: &serde_json::Value, params: &serde_json::Value) -> String {
        let token = params.get("token").and_then(serde_json::Value::as_str).unwrap_or("");
        let include_stream = params
            .get("include_stream")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let live = match self.live_mut(id) {
            Ok(live) => live,
            Err(response) => return response,
        };
        match live.terminate(token) {
            Ok(finished) => ok_response(id, finish_json(&finished, include_stream)),
            Err(error) => error_response(id, &debug_code(&error), &error.to_string()),
        }
    }

    fn op_capabilities(&mut self, id: &serde_json::Value) -> String {
        ok_response(
            id,
            serde_json::json!({
                "contract": mncs_vm::debug::DEBUG_SCHEMA_VERSION,
                "cli": CLI_SCHEMA_VERSION,
                "stop_targets": mncs_vm::debug::stop_target_names(),
                "event_kinds": mncs_vm::debug::observation_event_kinds(),
                "unsupported": mncs_vm::debug::unsupported_debug_capabilities(),
            }),
        )
    }
}

/// The live handle owns its token opaquely; the daemon re-checks by
/// attempting a token-gated no-op. Binding a fresh probe stop would
/// mutate state, so instead compare against the only token the
/// daemon ever issued for the current stop sequence.
fn live_token_matches(live: &mncs_vm::debug::LiveExecution<'static>, token: &str) -> bool {
    // The live execution is the authority on tokens; the daemon asks
    // it through a read-only check.
    live.check_token_public(token)
}

fn start_admitted(params: &serde_json::Value) -> Result<Admitted, String> {
    if let Some(path) = params.get("artifact_path").and_then(serde_json::Value::as_str) {
        let bytes = std::fs::read(path).map_err(|error| format!("cannot read {path}: {error}"))?;
        return admit(&bytes).map_err(|refusal| format!("admission refused: {refusal}"));
    }
    if let Some(document) = params.get("artifact") {
        let bytes = serde_json::to_vec(document).map_err(|error| format!("bad artifact: {error}"))?;
        return admit(&bytes).map_err(|refusal| format!("admission refused: {refusal}"));
    }
    if let Some(path) = params.get("compile").and_then(serde_json::Value::as_str) {
        return mncs_vm::harness::compile_file(Path::new(path))
            .map_err(|error| format!("cannot compile {path}: {error}"));
    }
    Err("start needs artifact_path, artifact, or compile".to_owned())
}

fn start_target(params: &serde_json::Value) -> Result<CallTarget, String> {
    let target = params.get("target").ok_or("start needs a target")?;
    if let Some(function) = target.get("function").and_then(serde_json::Value::as_str) {
        return Ok(CallTarget::ByFunction {
            function: function.to_owned(),
        });
    }
    let module = target.get("module").and_then(serde_json::Value::as_str).ok_or("target needs module+name or function")?;
    let name = target.get("name").and_then(serde_json::Value::as_str).ok_or("target needs module+name or function")?;
    Ok(CallTarget::ByName {
        module: module.to_owned(),
        name: name.to_owned(),
    })
}

fn start_capabilities(params: &serde_json::Value) -> Result<CapabilityEnv, String> {
    let Some(providers) = params.get("providers") else {
        return Ok(CapabilityEnv::empty());
    };
    let providers: BTreeMap<String, serde_json::Value> = serde_json::from_value(providers.clone())
        .map_err(|error| format!("bad providers: {error}"))?;
    let mut caps = CapabilityEnv::empty();
    for (capability, document) in providers {
        let identity = document
            .get("identity")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("cli-const-provider")
            .to_owned();
        let outputs: Vec<mncs_model::ExecutionValue> = document
            .get("outputs")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .map_err(|error| format!("bad provider outputs for {capability}: {error}"))?
            .unwrap_or_default();
        caps = caps.bind(
            &capability,
            ConstProvider {
                identity,
                outputs: outputs.iter().map(from_wire).collect(),
            },
        );
    }
    Ok(caps)
}

fn finish_json(finished: &mncs_vm::debug::FinishRecord, include_stream: bool) -> serde_json::Value {
    if include_stream {
        serde_json::json!({
            "event": "finished",
            "outcome": finished.outcome,
            "record": finished.record,
            "stream": finished.stream,
        })
    } else {
        serde_json::json!({
            "event": "finished",
            "outcome": finished.outcome,
            "record": finished.record,
        })
    }
}

fn ok_response(id: &serde_json::Value, result: serde_json::Value) -> String {
    serde_json::json!({
        "schema_version": CLI_SCHEMA_VERSION,
        "id": id,
        "ok": true,
        "result": result,
    })
    .to_string()
}

fn error_response(id: &serde_json::Value, code: &str, message: &str) -> String {
    serde_json::json!({
        "schema_version": CLI_SCHEMA_VERSION,
        "id": id,
        "ok": false,
        "error": {"code": code, "message": message},
    })
    .to_string()
}

fn debug_code(error: &DebugError) -> String {
    match error {
        DebugError::StaleToken { .. } => "stale_token".to_owned(),
        DebugError::BadToken { .. } => "bad_token".to_owned(),
        DebugError::AlreadyFinished => "already_finished".to_owned(),
        DebugError::ResumeRefusedTerminal { .. } => "resume_refused_terminal".to_owned(),
        DebugError::UnknownStop { .. } => "unknown_stop".to_owned(),
        DebugError::InvalidStop { .. } => "invalid_stop".to_owned(),
        DebugError::InvalidRequest { .. } => "invalid_request".to_owned(),
        DebugError::InspectionBound { .. } => "inspection_bound".to_owned(),
    }
}
