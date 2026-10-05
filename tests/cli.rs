//! `mncs-vm` binary: one-shot `run` and the JSONL `debug` driver.
//!
//! The driver tests speak the typed protocol over real process and
//! socket boundaries: scripted sessions prove stops, inspection,
//! resume, and cross-connection continuity without any log scraping.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_mncs-vm"))
}

fn corpus(name: &str) -> String {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("corpus")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn int_arg(value: i64) -> serde_json::Value {
    serde_json::json!({"integer": {"value": value, "type": {"bits": 64, "signed": true}}})
}

fn function_identity(corpus_name: &str, name_hint: &str) -> String {
    let admitted = mncs_vm::harness::tests_only::compile_corpus(corpus_name);
    let module = admitted.ssa_module().expect("admitted SSA");
    for function in &module.functions {
        if function.semantic_identity.0.contains(name_hint)
            || function.identity.0.contains(name_hint)
        {
            return function.identity.0.clone();
        }
    }
    panic!("no function matching {name_hint}");
}

fn first_instruction(corpus_name: &str, name_hint: &str) -> String {
    let admitted = mncs_vm::harness::tests_only::compile_corpus(corpus_name);
    let module = admitted.ssa_module().expect("admitted SSA");
    for function in &module.functions {
        if function.semantic_identity.0.contains(name_hint)
            || function.identity.0.contains(name_hint)
        {
            return function.blocks[0].instructions[0].identity.0.clone();
        }
    }
    panic!("no function matching {name_hint}");
}

#[test]
fn run_executes_oneshot() {
    let args_path = std::env::temp_dir().join(format!("mncs-vm-cli-args-{}.json", std::process::id()));
    std::fs::write(&args_path, serde_json::to_string(&vec![int_arg(3), int_arg(4)]).unwrap()).unwrap();
    let output = Command::new(bin())
        .args([
            "run",
            "--compile",
            &corpus("arith.mncs"),
            "--callable",
            "mncs.vmcorpus.arith.v1::add2",
            "--args",
            args_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn mncs-vm run");
    let _ = std::fs::remove_file(&args_path);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["outcome"]["kind"], "completed");
    assert_eq!(document["record"]["returned"][0]["Integer"]["value"], 7);
    assert!(document["record"].get("observation").is_none());
}

#[test]
fn run_with_type_args_executes_generic() {
    // Seeded bytes via the in-process harness: the CLI only ever sees
    // frozen artifact JSON plus JSON argument files.
    let source = std::fs::read_to_string(corpus("generic.mncs")).unwrap();
    let seeds = vec![mncs_model::HostGenericSeedRequest {
        module: "mncs.vmcorpus.generic.v1".to_owned(),
        function: "first".to_owned(),
        type_arguments: vec![mncs_model::ExecutionTypeArgument::Nat { value: 4 }],
    }];
    let (_, backend) = mncs_vm::harness::compile_to_backend_seeded(&source, "generic.mncs", &seeds)
        .expect("seeded compile");
    let admitted = mncs_vm::migrate::admit_research_artifact(&backend).expect("admit");
    assert_eq!(admitted.artifact.generic_entrypoints.len(), 1);
    let stamp = std::process::id();
    let artifact_path = std::env::temp_dir().join(format!("mncs-vm-cli-generic-{stamp}.json"));
    let args_path = std::env::temp_dir().join(format!("mncs-vm-cli-generic-args-{stamp}.json"));
    let targs_path = std::env::temp_dir().join(format!("mncs-vm-cli-generic-targs-{stamp}.json"));
    std::fs::write(
        &artifact_path,
        serde_json::to_vec(&admitted.artifact).unwrap(),
    )
    .unwrap();
    std::fs::write(
        &args_path,
        serde_json::to_string(&vec![serde_json::json!({"sequence": {"values": [
            int_arg(11),
            int_arg(22),
        ]}})])
        .unwrap(),
    )
    .unwrap();
    std::fs::write(&targs_path, r#"[{"kind": "nat", "value": 4}]"#).unwrap();
    let output = Command::new(bin())
        .args([
            "run",
            "--artifact",
            artifact_path.to_str().unwrap(),
            "--callable",
            "mncs.vmcorpus.generic.v1::first",
            "--args",
            args_path.to_str().unwrap(),
            "--type-args",
            targs_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn mncs-vm run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["outcome"]["kind"], "completed");
    assert_eq!(document["record"]["returned"][0]["Integer"]["value"], 11);
    assert!(document["record"]["callable_name"]
        .as_str()
        .unwrap()
        .starts_with("mncs.vmcorpus.generic.v1::first<"));
    // Uncompiled instantiation is a structured refusal, not a crash.
    std::fs::write(&targs_path, r#"[{"kind": "nat", "value": 5}]"#).unwrap();
    let output = Command::new(bin())
        .args([
            "run",
            "--artifact",
            artifact_path.to_str().unwrap(),
            "--callable",
            "mncs.vmcorpus.generic.v1::first",
            "--args",
            args_path.to_str().unwrap(),
            "--type-args",
            targs_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn mncs-vm run");
    let _ = std::fs::remove_file(&artifact_path);
    let _ = std::fs::remove_file(&args_path);
    let _ = std::fs::remove_file(&targs_path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["outcome"]["kind"], "invalid_request");
    assert!(document["outcome"]["reason"]
        .as_str()
        .unwrap()
        .contains("no compiled specialization"));
}

#[test]
fn run_with_observe_retains_stream() {
    let policy_path = std::env::temp_dir().join(format!("mncs-vm-cli-policy-{}.json", std::process::id()));
    std::fs::write(
        &policy_path,
        serde_json::to_string(&serde_json::json!({
            "schema_version": "mncs.execution-observation-policy/1",
            "capture": "bounded",
            "max_events": 64,
            "max_values": 32,
            "max_value_bytes": 4096,
            "include_frames": true,
            "include_effects": true,
            "selected_operations": [],
        }))
        .unwrap(),
    )
    .unwrap();
    let args_path = std::env::temp_dir().join(format!("mncs-vm-cli-args2-{}.json", std::process::id()));
    std::fs::write(&args_path, serde_json::to_string(&vec![int_arg(10)]).unwrap()).unwrap();
    let output = Command::new(bin())
        .args([
            "run",
            "--compile",
            &corpus("arith.mncs"),
            "--callable",
            "mncs.vmcorpus.arith.v1::add3",
            "--args",
            args_path.to_str().unwrap(),
            "--observe",
            policy_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn mncs-vm run");
    let _ = std::fs::remove_file(&args_path);
    let _ = std::fs::remove_file(&policy_path);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["outcome"]["kind"], "completed");
    assert_eq!(document["record"]["returned"][0]["Integer"]["value"], 13);
    let observation = &document["record"]["observation"];
    assert_eq!(observation["schema_version"], "mncs.execution-observation/1");
    assert!(!observation["events"].as_array().unwrap().is_empty());
}

/// Drive a full stdio session: capabilities, start with an operation
/// stop, inspect, step, resume to finish.
#[test]
fn stdio_debug_session_stops_inspects_resumes() {
    let instruction = first_instruction("arith.mncs", "add2");
    let mut child = Command::new(bin())
        .args(["debug", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mncs-vm debug");
    let mut stdin = child.stdin.take().expect("driver stdin");
    let stdout = child.stdout.take().expect("driver stdout");
    let mut lines = BufReader::new(stdout).lines();

    let roundtrip = |stdin: &mut std::process::ChildStdin,
                     lines: &mut std::io::Lines<BufReader<std::process::ChildStdout>>,
                     id: u64,
                     op: &str,
                     params: serde_json::Value|
     -> serde_json::Value {
        let request = serde_json::json!({"id": id, "op": op, "params": params});
        writeln!(stdin, "{}", request).expect("write request");
        stdin.flush().ok();
        let line = lines.next().expect("response line").expect("read response");
        serde_json::from_str(&line).expect("response JSON")
    };

    let response = roundtrip(&mut stdin, &mut lines, 1, "capabilities", serde_json::json!({}));
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["contract"], "mncs.vm.debug/1");

    let response = roundtrip(
        &mut stdin,
        &mut lines,
        2,
        "start",
        serde_json::json!({
            "compile": corpus("arith.mncs"),
            "target": {"module": "mncs.vmcorpus.arith.v1", "name": "add2"},
            "arguments": [int_arg(3), int_arg(4)],
            "debug": {
                "policy": {
                    "schema_version": "mncs.execution-observation-policy/1",
                    "capture": "none",
                    "max_events": 64,
                    "max_values": 32,
                    "max_value_bytes": 4096,
                    "include_frames": true,
                    "include_effects": true,
                    "selected_operations": [],
                },
                "stops": [{"id": "s", "target": {"kind": "operation", "instruction": instruction}}],
                "stop_on_abnormal_terminal": true,
            },
        }),
    );
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"]["event"], "stopped");
    let token = response["result"]["stop"]["continuation_token"].as_str().unwrap().to_owned();
    assert_eq!(response["result"]["stop"]["safe_point"]["depth"], 0);

    let response = roundtrip(
        &mut stdin,
        &mut lines,
        3,
        "inspect",
        serde_json::json!({"token": token, "view": "stack", "max_frames": 4, "max_values": 8, "max_value_bytes": 1024}),
    );
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"]["stack"]["frames"].as_array().unwrap().len(), 1);
    assert!(response["result"]["stack"]["frames"][0]["values"].as_array().unwrap().len() >= 2);

    // A stale token is refused without disturbing the live stop.
    let response = roundtrip(&mut stdin, &mut lines, 4, "resume", serde_json::json!({"token": "dbg:deadbeefdead:9:zzz"}));
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "bad_token");

    let response = roundtrip(&mut stdin, &mut lines, 5, "resume", serde_json::json!({"token": token}));
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"]["event"], "finished");
    assert_eq!(response["result"]["outcome"]["kind"], "completed");
    assert_eq!(response["result"]["record"]["returned"][0]["Integer"]["value"], 7);

    let _ = roundtrip(&mut stdin, &mut lines, 6, "close", serde_json::json!({}));
    let status = child.wait().expect("driver exit");
    assert!(status.success());
}

/// The socket daemon holds one execution across connections: stop on
/// one connection, then reconnect and continue the same execution.
#[cfg(unix)]
#[test]
fn socket_daemon_reconnects_to_live_execution() {
    use std::os::unix::net::UnixStream;

    let socket = std::env::temp_dir().join(format!("mncs-vm-debug-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let mut child = Command::new(bin())
        .args(["debug", "--serve", socket.to_str().unwrap()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mncs-vm daemon");
    // Wait for the socket to accept.
    let mut stream = None;
    for _ in 0..100 {
        match UnixStream::connect(&socket) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    let stream = stream.expect("daemon socket accepts");
    let communicate = |stream: &UnixStream, requests: Vec<serde_json::Value>| -> Vec<serde_json::Value> {
        let mut writer = stream.try_clone().expect("clone socket");
        let reader = BufReader::new(stream.try_clone().expect("clone socket"));
        let mut responses = Vec::new();
        let mut lines = reader.lines();
        for request in requests {
            writeln!(writer, "{request}").expect("write request");
            writer.flush().ok();
            let line = lines.next().expect("response line").expect("read response");
            responses.push(serde_json::from_str(&line).expect("response JSON"));
        }
        responses
    };

    // First connection: start with a function-entry stop, then drop it.
    let add1 = function_identity("arith.mncs", "add1");
    let responses = communicate(
        &stream,
        vec![serde_json::json!({
            "id": 1,
            "op": "start",
            "params": {
                "compile": corpus("arith.mncs"),
                "target": {"module": "mncs.vmcorpus.arith.v1", "name": "add3"},
                "arguments": [int_arg(10)],
                "debug": {
                    "policy": {
                        "schema_version": "mncs.execution-observation-policy/1",
                        "capture": "none",
                        "max_events": 16,
                        "max_values": 8,
                        "max_value_bytes": 1024,
                        "include_frames": true,
                        "include_effects": true,
                        "selected_operations": [],
                    },
                    "stops": [{"id": "enter", "target": {"kind": "function", "function": add1}}],
                    "stop_on_abnormal_terminal": true,
                },
            },
        })],
    );
    assert_eq!(responses[0]["ok"], true, "{}", responses[0]);
    assert_eq!(responses[0]["result"]["event"], "stopped");
    let token = responses[0]["result"]["stop"]["continuation_token"].as_str().unwrap().to_owned();
    assert_eq!(responses[0]["result"]["stop"]["safe_point"]["depth"], 1);
    drop(stream);

    // Second connection, same execution: inspect, then step out and finish.
    let stream = UnixStream::connect(&socket).expect("reconnect");
    let responses = communicate(
        &stream,
        vec![
            serde_json::json!({"id": 2, "op": "inspect", "params": {"token": token, "view": "stack", "max_frames": 4, "max_values": 8, "max_value_bytes": 1024}}),
        ],
    );
    assert_eq!(responses[0]["ok"], true, "{}", responses[0]);
    assert_eq!(responses[0]["result"]["stack"]["frames"].as_array().unwrap().len(), 2);

    let responses = communicate(
        &stream,
        vec![serde_json::json!({"id": 3, "op": "step_out", "params": {"token": token}})],
    );
    assert_eq!(responses[0]["ok"], true, "{}", responses[0]);
    assert_eq!(responses[0]["result"]["event"], "stopped");
    let token = responses[0]["result"]["stop"]["continuation_token"].as_str().unwrap().to_owned();

    let responses = communicate(
        &stream,
        vec![serde_json::json!({"id": 4, "op": "resume", "params": {"token": token}})],
    );
    assert_eq!(responses[0]["ok"], true, "{}", responses[0]);
    // The entry stop re-fires on the next add1 call or the run
    // finishes; either is the same live execution continuing.
    if responses[0]["result"]["event"] == "stopped" {
        let token = responses[0]["result"]["stop"]["continuation_token"].as_str().unwrap().to_owned();
        let responses = communicate(
            &stream,
            vec![
                serde_json::json!({"id": 5, "op": "clear_stop", "params": {"token": token, "id": "enter"}}),
            ],
        );
        assert_eq!(responses[0]["result"]["cleared"], true);
        let responses = communicate(
            &stream,
            vec![serde_json::json!({"id": 6, "op": "resume", "params": {"token": token}})],
        );
        assert_eq!(responses[0]["result"]["event"], "finished");
        assert_eq!(responses[0]["result"]["record"]["returned"][0]["Integer"]["value"], 13);
    } else {
        assert_eq!(responses[0]["result"]["record"]["returned"][0]["Integer"]["value"], 13);
    }

    let _ = communicate(&stream, vec![serde_json::json!({"id": 7, "op": "shutdown", "params": {}})]);
    drop(stream);
    let status = child.wait().expect("daemon exit");
    assert!(status.success());
    assert!(!socket.exists(), "daemon removes its socket");
}

#[test]
fn compile_then_run_matches_direct_compile() {
    let artifact_path =
        std::env::temp_dir().join(format!("mncs-vm-cli-artifact-{}.json", std::process::id()));
    let output = Command::new(bin())
        .args(["compile", &corpus("arith.mncs"), "--output", artifact_path.to_str().unwrap()])
        .output()
        .expect("spawn mncs-vm compile");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let compiled: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(compiled["artifact_id"].as_str().is_some_and(|id| id.starts_with("sha256:")));
    let args_path = std::env::temp_dir().join(format!("mncs-vm-cli-args3-{}.json", std::process::id()));
    std::fs::write(&args_path, serde_json::to_string(&vec![int_arg(3), int_arg(4)]).unwrap()).unwrap();
    let via_artifact = Command::new(bin())
        .args([
            "run",
            "--artifact",
            artifact_path.to_str().unwrap(),
            "--callable",
            "mncs.vmcorpus.arith.v1::add2",
            "--args",
            args_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn mncs-vm run --artifact");
    let via_compile = Command::new(bin())
        .args([
            "run",
            "--compile",
            &corpus("arith.mncs"),
            "--callable",
            "mncs.vmcorpus.arith.v1::add2",
            "--args",
            args_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn mncs-vm run --compile");
    let _ = std::fs::remove_file(&args_path);
    let _ = std::fs::remove_file(&artifact_path);
    assert!(via_artifact.status.success(), "{}", String::from_utf8_lossy(&via_artifact.stderr));
    assert!(via_compile.status.success(), "{}", String::from_utf8_lossy(&via_compile.stderr));
    let from_artifact: serde_json::Value = serde_json::from_slice(&via_artifact.stdout).unwrap();
    let from_compile: serde_json::Value = serde_json::from_slice(&via_compile.stdout).unwrap();
    assert_eq!(from_artifact, from_compile, "artifact reuse is byte-identical");
}
