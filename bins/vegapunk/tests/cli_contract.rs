//! CLI contract tests: config resolution, typed errors, create-only state,
//! cursor pagination, partial apply reports, reference-only packs, MCP stdio.
//!
//! Every child runs with a cleared env (temp HOME, explicit VEGAPUNK_CONFIG)
//! and a fresh temp cwd — no inherited provider/store vars, no real data.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};

fn vegapunk() -> &'static str {
    env!("CARGO_BIN_EXE_vegapunk")
}

fn run(cwd: &Path, config: Option<&Path>, args: &[&str]) -> Output {
    let mut c = Command::new(vegapunk());
    c.args(args)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", cwd.join("home"))
        .env("NO_COLOR", "1");
    if let Some(cfg) = config {
        c.env("VEGAPUNK_CONFIG", cfg);
    }
    output_with_timeout(c, Duration::from_secs(60))
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_until_exit(child: &mut Child, deadline: std::time::Instant) -> std::process::ExitStatus {
    loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            return status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("owned test process exceeded deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn output_with_timeout(mut command: Command, timeout: Duration) -> Output {
    use std::io::Read;
    let mut child = OwnedChild(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn child"),
    );
    let stdout = child.0.stdout.take().unwrap();
    let stderr = child.0.stderr.take().unwrap();
    let capture = |reader: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            reader
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .unwrap();
            bytes
        })
    };
    let out = capture(Box::new(stdout));
    let err = capture(Box::new(stderr));
    let status = wait_until_exit(&mut child.0, std::time::Instant::now() + timeout);
    let stdout = out.join().unwrap();
    let stderr = err.join().unwrap();
    assert!(
        stdout.len() <= 4 * 1024 * 1024 && stderr.len() <= 4 * 1024 * 1024,
        "test output exceeded capture limit"
    );
    Output {
        status,
        stdout,
        stderr,
    }
}

#[test]
fn owned_process_timeout_helper() {
    if std::env::var_os("NOMISO_CONTRACT_SLEEP").is_some() {
        std::thread::sleep(Duration::from_secs(5));
    }
}

#[test]
fn owned_process_deadline_is_enforced() {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "owned_process_timeout_helper"])
        .env_clear()
        .env("NOMISO_CONTRACT_SLEEP", "1");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        output_with_timeout(command, Duration::from_millis(100))
    }));
    assert!(result.is_err());
}

fn write_cfg(dir: &Path, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join("vegapunk.toml");
    std::fs::write(&p, body).unwrap();
    p
}

fn stdout_json(out: &Output) -> Value {
    let s = String::from_utf8_lossy(&out.stdout);
    let t = s.trim();
    assert!(
        !t.is_empty(),
        "empty stdout; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(t).unwrap_or_else(|e| {
        panic!(
            "stdout not one JSON doc: {e}\nstdout={s}\nstderr={}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[test]
fn config_errors_do_not_echo_sensitive_source() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = write_cfg(
        tmp.path(),
        "embed_url = \"https://user:synthetic-secret@example.invalid/v1\"\nembed_dim = [broken\n",
    );
    let out = run(tmp.path(), Some(&cfg), &["--format", "json", "status"]);
    assert_eq!(out.status.code(), Some(2));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!combined.contains("synthetic-secret"));
    assert_eq!(stdout_json(&out)["code"], "config_error");
}

#[test]
fn blob_environment_override_bypasses_unused_legacy_path() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let cfg = write_cfg(
        &tmp.path().join("config"),
        "endpoint = \"memory\"\nblob_root = \"unused\"\nembed_dim = 8\n",
    );
    let mut command = Command::new(vegapunk());
    command
        .args(["--format", "json", "status"])
        .current_dir(&cwd)
        .env_clear()
        .env("HOME", &cwd)
        .env("VEGAPUNK_CONFIG", &cfg)
        .env("VEGAPUNK_BLOB_ROOT", "chosen");
    let out = output_with_timeout(command, Duration::from_secs(60));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        stdout_json(&out)["blob_root"],
        cwd.join("chosen").to_string_lossy().as_ref()
    );
    assert!(!tmp.path().join("config/unused").exists());
}

fn rocks_cfg(dir: &Path) -> PathBuf {
    write_cfg(
        dir,
        "endpoint = \"rocksdb://./data\"\npath_base = \"config\"\nembed_dim = 8\ndefault_scope = \"org/t\"\n",
    )
}

#[test]
fn missing_explicit_config_errors_without_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nope.toml");
    let out = run(tmp.path(), Some(&missing), &["--format", "json", "status"]);
    assert_eq!(out.status.code(), Some(2));
    let v = stdout_json(&out);
    assert_eq!(v["code"], "config_error");
    assert!(!tmp.path().join(".nomiso-data").exists());
}

#[test]
fn malformed_discovered_config_errors() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("vegapunk.toml"), "not = [valid\n").unwrap();
    let out = run(tmp.path(), None, &["--format", "json", "status"]);
    assert_eq!(out.status.code(), Some(2));
    let v = stdout_json(&out);
    assert_eq!(v["code"], "config_error");
    assert!(!tmp.path().join(".nomiso-data").exists());
}

#[test]
fn path_base_config_resolves_under_config_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("cwd");
    let cfgdir = tmp.path().join("cfg");
    std::fs::create_dir_all(&cwd).unwrap();
    let cfg = write_cfg(
        &cfgdir,
        "endpoint = \"rocksdb://./data\"\npath_base = \"config\"\nblob_root = \"blobs\"\nembed_dim = 8\ndefault_scope = \"org/t\"\n",
    );
    let out = run(
        &cwd,
        Some(&cfg),
        &[
            "--format",
            "json",
            "working-state",
            "--scope",
            "org/t",
            "--put-json",
            "{\"a\":1}",
        ],
    );
    assert!(
        out.status.success(),
        "stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(cfgdir.join("data").exists(), "data dir under config dir");
    assert!(!cwd.join("data").exists(), "no data dir under cwd");

    let out = run(&cwd, Some(&cfg), &["--format", "json", "status"]);
    assert!(out.status.success());
    let v = stdout_json(&out);
    assert_eq!(
        v["blob_root"].as_str().unwrap(),
        cfgdir.join("blobs").display().to_string()
    );
}

#[test]
fn legacy_ambiguous_relative_config_errors_and_override_bypasses() {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("cwd");
    let cfgdir = tmp.path().join("cfg");
    std::fs::create_dir_all(&cwd).unwrap();
    let cfg = write_cfg(
        &cfgdir,
        "endpoint = \"rocksdb://./filedata\"\nembed_dim = 8\ndefault_scope = \"org/t\"\n",
    );
    let out = run(
        &cwd,
        Some(&cfg),
        &[
            "--format",
            "json",
            "working-state",
            "--scope",
            "org/t",
            "--put-json",
            "{}",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    let v = stdout_json(&out);
    assert_eq!(v["code"], "config_error");
    assert!(
        v["error"].as_str().unwrap().contains("path_base"),
        "got {v}"
    );
    assert!(!cwd.join("filedata").exists());
    assert!(!cfgdir.join("filedata").exists());

    // Explicit CLI endpoint resolves against cwd and bypasses the unused file endpoint.
    let out = run(
        &cwd,
        Some(&cfg),
        &[
            "--format",
            "json",
            "--endpoint",
            "rocksdb://./ovr",
            "working-state",
            "--scope",
            "org/t",
            "--put-json",
            "{}",
        ],
    );
    assert!(
        out.status.success(),
        "stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(cwd.join("ovr").exists());

    // path_base = "cwd" preserves legacy location.
    let cfgdir2 = tmp.path().join("cfg2");
    let cfg2 = write_cfg(
        &cfgdir2,
        "endpoint = \"rocksdb://./data2\"\npath_base = \"cwd\"\nembed_dim = 8\ndefault_scope = \"org/t\"\n",
    );
    let out = run(
        &cwd,
        Some(&cfg2),
        &[
            "--format",
            "json",
            "working-state",
            "--scope",
            "org/t",
            "--put-json",
            "{}",
        ],
    );
    assert!(out.status.success());
    assert!(cwd.join("data2").exists());
    assert!(!cfgdir2.join("data2").exists());
}

#[test]
fn metadata_ops_work_without_embed_key_recall_is_typed() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = write_cfg(
        tmp.path(),
        "endpoint = \"memory\"\nembed_dim = 8\nembed_url = \"http://127.0.0.1:9/v1\"\nembed_model = \"test-model\"\nembed_api_key_env = \"VEGAPUNK_TEST_ABSENT_KEY\"\ndefault_scope = \"org/t\"\n",
    );
    for args in [
        vec![
            "working-state",
            "--scope",
            "org/t",
            "--put-json",
            "{\"g\":\"x\"}",
        ],
        vec!["working-state", "--scope", "org/t"],
        vec!["list", "--scope", "org/t"],
    ] {
        let mut full = vec!["--format", "json"];
        full.extend(args.iter().copied());
        let out = run(tmp.path(), Some(&cfg), &full);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out = run(
        tmp.path(),
        Some(&cfg),
        &[
            "--format", "json", "recall", "--scope", "org/t", "--query", "x",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let v = stdout_json(&out);
    assert_eq!(v["code"], "provider_unavailable", "got {v}");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!combined.contains("sk-"), "leaked secret: {combined}");
}

#[test]
fn keyed_encode_replays_across_processes_and_rejects_changed_text() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = rocks_cfg(tmp.path());
    let args = [
        "--format",
        "json",
        "encode",
        "--text",
        "same durable fact",
        "--idempotency-key",
        "call-1",
    ];
    let first = run(tmp.path(), Some(&cfg), &args);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let replay = run(tmp.path(), Some(&cfg), &args);
    assert!(replay.status.success());
    let first = stdout_json(&first);
    let replay = stdout_json(&replay);
    assert_eq!(replay["id"], first["id"]);
    assert_eq!(replay["replayed"], true);
    let changed = run(
        tmp.path(),
        Some(&cfg),
        &[
            "--format",
            "json",
            "encode",
            "--text",
            "different durable fact",
            "--idempotency-key",
            "call-1",
        ],
    );
    assert_eq!(changed.status.code(), Some(1));
    assert_eq!(stdout_json(&changed)["code"], "idempotency_conflict");
}

#[test]
fn partial_apply_ops_single_json_report() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = rocks_cfg(tmp.path());
    let ops = json!([
        {"op":"put","scope":"org/t","text":"first durable fact"},
        {"op":"forget","id":"memory:does-not-exist","scope":"org/t","expected_version":1},
        {"op":"put","scope":"org/t","text":"third durable fact"}
    ])
    .to_string();
    let out = run(
        tmp.path(),
        Some(&cfg),
        &[
            "--format",
            "json",
            "--scope",
            "org/t",
            "apply-ops",
            "--json",
            &ops,
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let v = stdout_json(&out);
    assert_eq!(v["status"], "partial", "got {v}");
    assert_eq!(v["outcomes"].as_array().unwrap().len(), 3);
    assert_eq!(v["outcomes"][0]["outcome"], "ok");
    assert_eq!(v["outcomes"][1]["code"], "not_found");
    assert_eq!(v["outcomes"][2]["code"], "not_attempted");

    let out = run(
        tmp.path(),
        Some(&cfg),
        &["--format", "json", "count", "--scope", "org/t"],
    );
    assert!(out.status.success());
    assert_eq!(stdout_json(&out)["count"], 1);
}

#[test]
fn long_memory_reference_only_card_and_full_read() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = rocks_cfg(tmp.path());
    let long = format!("uniquetoken start {} end", "filler ".repeat(900));
    let out = run(
        tmp.path(),
        Some(&cfg),
        &[
            "--format", "json", "encode", "--scope", "org/t", "--text", &long,
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = stdout_json(&out);
    let id = v["id"].as_str().unwrap().to_string();

    let out = run(
        tmp.path(),
        Some(&cfg),
        &[
            "--format",
            "json",
            "hard-recall",
            "--scope",
            "org/t",
            "--query",
            "uniquetoken",
            "--pack",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = stdout_json(&out);
    let cards = v["pack"]["cards"].as_array().unwrap();
    let card = cards
        .iter()
        .find(|c| c["id"].as_str() == Some(id.as_str()))
        .expect("card for id");
    assert_eq!(card["reference_only"], true, "card={card}");
    assert!(
        card["original_bytes"].as_u64().unwrap() > 4000,
        "card={card}"
    );

    let out = run(
        tmp.path(),
        Some(&cfg),
        &["--format", "json", "read", "--scope", "org/t", "--id", &id],
    );
    assert!(out.status.success());
    let rows = stdout_json(&out);
    assert_eq!(rows[0]["content"]["text"].as_str().unwrap(), long);
}

#[test]
fn list_cursor_pagination_and_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = rocks_cfg(tmp.path());
    for i in 0..6 {
        let out = run(
            tmp.path(),
            Some(&cfg),
            &[
                "--format",
                "json",
                "encode",
                "--scope",
                "org/t",
                "--text",
                &format!("paginated fact {i}"),
            ],
        );
        assert!(out.status.success());
    }
    let out = run(
        tmp.path(),
        Some(&cfg),
        &[
            "--format", "json", "list", "--scope", "org/t", "--limit", "4",
        ],
    );
    assert!(out.status.success());
    let page1 = stdout_json(&out);
    assert_eq!(page1["count"], 4);
    let cursor = page1["next_cursor"].clone();
    assert!(cursor.is_object(), "page1={page1}");

    let out = run(
        tmp.path(),
        Some(&cfg),
        &[
            "--format",
            "json",
            "list",
            "--scope",
            "org/t",
            "--limit",
            "4",
            "--cursor-json",
            &cursor.to_string(),
        ],
    );
    assert!(out.status.success());
    let page2 = stdout_json(&out);
    assert_eq!(page2["count"], 2);

    // Cursor issued under a different scope must be rejected.
    let out = run(
        tmp.path(),
        Some(&cfg),
        &[
            "--format",
            "json",
            "list",
            "--scope",
            "org/other",
            "--limit",
            "4",
            "--cursor-json",
            &cursor.to_string(),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let v = stdout_json(&out);
    assert_eq!(v["code"], "invalid_request", "got {v}");
}

#[test]
fn working_state_create_only_then_cas() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = rocks_cfg(tmp.path());
    let ws = |extra: &[&str]| {
        let mut a = vec!["--format", "json", "working-state", "--scope", "org/t"];
        a.extend(extra.iter().copied());
        run(tmp.path(), Some(&cfg), &a)
    };

    let out = ws(&["--put-json", "{\"goal\":\"a\"}"]);
    assert!(out.status.success());
    assert_eq!(stdout_json(&out)["version"], 1);

    // Second no-version write must conflict, not overwrite.
    let out = ws(&["--put-json", "{\"goal\":\"b\"}"]);
    assert_eq!(out.status.code(), Some(1));
    let v = stdout_json(&out);
    assert_eq!(v["code"], "conflict", "got {v}");

    let out = ws(&[]);
    assert!(out.status.success());
    assert_eq!(stdout_json(&out)["body"]["goal"], "a");

    // Correct CAS succeeds; stale version conflicts.
    let out = ws(&["--put-json", "{\"goal\":\"b\"}", "--expected-version", "1"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_json(&out)["version"], 2);
    let out = ws(&["--put-json", "{\"goal\":\"c\"}", "--expected-version", "1"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_json(&out)["code"], "conflict");
}

struct McpChild {
    child: Child,
    rx: mpsc::Receiver<Result<Value, String>>,
}

impl McpChild {
    fn spawn(cwd: &Path, config: &Path) -> Self {
        let mut child = Command::new(vegapunk())
            .args(["mcp"])
            .current_dir(cwd)
            .env_clear()
            .env("HOME", cwd.join("home"))
            .env("VEGAPUNK_CONFIG", config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn vegapunk mcp");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let value = line.map_err(|e| e.to_string()).and_then(|line| {
                    serde_json::from_str::<Value>(&line)
                        .map_err(|e| format!("invalid MCP stdout: {e}"))
                });
                let failed = value.is_err();
                if tx.send(value).is_err() || failed {
                    break;
                }
            }
        });
        Self { child, rx }
    }

    fn send(&mut self, msg: &Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&self) -> Value {
        self.recv_until(std::time::Instant::now() + Duration::from_secs(15))
    }

    fn recv_until(&self, deadline: std::time::Instant) -> Value {
        self.rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("mcp response timeout")
            .expect("MCP stdout must be valid JSON")
    }

    fn call(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }));
        loop {
            let v = self.recv_until(deadline);
            if v["id"] == id {
                return v;
            }
        }
    }
}

impl Drop for McpChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_stdio_typed_errors_and_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = write_cfg(tmp.path(), "endpoint = \"memory\"\nembed_dim = 8\n");
    let mut mcp = McpChild::spawn(tmp.path(), &cfg);

    mcp.send(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "cli-contract-test", "version": "0"},
        },
    }));
    let init = mcp.recv();
    assert_eq!(init["id"], 1);
    assert!(init["result"]["serverInfo"].is_object(), "init={init}");
    mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    mcp.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let list = mcp.recv();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 42, "tools={names:?}");
    assert!(names.contains(&"vegapunk_apply_ops"));
    assert!(names.contains(&"vegapunk_entity_put"));
    assert!(names.contains(&"vegapunk_traverse"));
    assert!(names.contains(&"vegapunk_embedding_state"));
    assert!(names.contains(&"vegapunk_job_enqueue"));
    assert!(names.contains(&"vegapunk_job_supersede"));

    // working-state create + get roundtrip.
    let r = mcp.call(
        3,
        "vegapunk_working_state",
        json!({"scope": "org/t", "put_json": {"goal": "mcp"}}),
    );
    assert_eq!(r["result"]["isError"], false, "resp={r}");
    assert_eq!(
        r["result"]["structuredContent"]["body"]["goal"], "mcp",
        "resp={r}"
    );

    // Bad scope is a typed tool error, not a protocol error.
    let r = mcp.call(4, "vegapunk_working_state", json!({"scope": "!!bad scope"}));
    assert_eq!(r["result"]["isError"], true, "resp={r}");
    assert_eq!(
        r["result"]["structuredContent"]["code"], "invalid_request",
        "resp={r}"
    );

    // Second create-only write conflicts via structured error.
    let r = mcp.call(
        5,
        "vegapunk_working_state",
        json!({"scope": "org/t", "put_json": {"goal": "other"}}),
    );
    assert_eq!(r["result"]["isError"], true, "resp={r}");
    assert_eq!(
        r["result"]["structuredContent"]["code"], "conflict",
        "resp={r}"
    );

    // Partial apply report is a structured error carrying the full report.
    let ops = json!([
        {"op":"put","scope":"org/t","text":"first durable fact"},
        {"op":"forget","id":"memory:does-not-exist","scope":"org/t","expected_version":1},
        {"op":"put","scope":"org/t","text":"third durable fact"}
    ]);
    let r = mcp.call(
        6,
        "vegapunk_apply_ops",
        json!({"scope": "org/t", "ops_json": ops.to_string()}),
    );
    assert_eq!(r["result"]["isError"], true, "resp={r}");
    let sc = &r["result"]["structuredContent"];
    assert_eq!(sc["status"], "partial", "resp={r}");
    assert_eq!(sc["outcomes"].as_array().unwrap().len(), 3);

    // ── API-007 transport conformance ──
    // Every advertised tool carries a valid generated inputSchema object
    // (zero-arg tools may omit `properties` — that's still a valid schema).
    for t in list["result"]["tools"].as_array().unwrap() {
        let schema = &t["inputSchema"];
        assert_eq!(
            schema["type"], "object",
            "tool {} schema={schema}",
            t["name"]
        );
        if let Some(props) = schema.get("properties") {
            assert!(props.is_object(), "tool {}", t["name"]);
        }
    }
    // Unknown tool → protocol-level error (JSON-RPC error object), not a tool result.
    let r = mcp.call(7, "vegapunk_no_such_tool", json!({}));
    assert!(r.get("error").is_some(), "unknown tool must error: {r}");
    assert!(r["error"]["code"].is_i64());
    // Unknown method → -32601.
    mcp.send(&json!({"jsonrpc": "2.0", "id": 8, "method": "nomiso/bogus"}));
    let r = mcp.recv();
    assert_eq!(r["error"]["code"], -32601, "resp={r}");
    // Malformed JSON must not poison the connection or corrupt stdout:
    // rmcp drops the unparseable frame (no id to answer), so the conformance
    // property is that the next well-formed call still succeeds.
    {
        let stdin = mcp.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{{not json").unwrap();
        stdin.flush().unwrap();
    }
    // A request following malformed input is answered normally.
    let r = mcp.call(9, "vegapunk_count", json!({"scope": "org/t"}));
    assert_eq!(r["result"]["isError"], false, "resp={r}");

    // Graceful EOF shutdown.
    drop(mcp.child.stdin.take());
    let status = wait_until_exit(
        &mut mcp.child,
        std::time::Instant::now() + Duration::from_secs(15),
    );
    assert!(status.success(), "mcp exit={status}");
}

#[test]
fn operator_surfaces_entity_relationship_job_embed() {
    let tmp = tempfile::tempdir().unwrap();
    // Multi-invocation state requires a persistent store (memory is ephemeral).
    let cfg = rocks_cfg(tmp.path());
    let j = |args: &[&str]| -> Value {
        let mut full = vec!["--format", "json", "--no-help"];
        full.extend(args.iter().copied());
        let out = run(tmp.path(), Some(&cfg), &full);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout_json(&out)
    };

    // Entities.
    let a = j(&[
        "entity-put",
        "--kind",
        "service",
        "--name",
        "api",
        "--alias",
        "edge",
    ]);
    assert_eq!(a["name"], "api");
    assert_eq!(a["version"], 1);
    let b = j(&["entity-put", "--kind", "service", "--name", "db"]);
    let id_a = a["id"].as_str().unwrap();
    let id_b = b["id"].as_str().unwrap();

    let got = j(&["entity-get", "--id", id_a]);
    assert_eq!(got["name"], "api");

    // Relationship + list + traverse.
    let rel = j(&[
        "rel-put",
        "--predicate",
        "depends_on",
        "--from",
        id_a,
        "--to",
        id_b,
    ]);
    assert_eq!(rel["record"]["predicate"], "depends_on");
    let rows = j(&["rel-list", "--endpoint-ref", id_a]);
    assert_eq!(rows.as_array().unwrap().len(), 1);
    let trav = j(&["traverse", "--seed", id_b, "--direction", "both"]);
    assert_eq!(trav["nodes"].as_array().unwrap().len(), 1);

    // Embedding state (generation 1 bootstrap).
    let st = j(&["embed-state"]);
    assert_eq!(st["generations"][0]["generation"], 1);

    // Job lifecycle: enqueue → summary privacy → claim → stale-fence error → complete.
    let enq = j(&[
        "job-enqueue",
        "--kind",
        "reindex",
        "--payload",
        "{\"secret\":\"sensitive-body\"}",
    ]);
    let job_id = enq["job"]["id"].as_str().unwrap();

    let list = j(&["job-list"]);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let serialized = list.to_string();
    assert!(
        !serialized.contains("sensitive-body"),
        "summary leaked payload: {serialized}"
    );

    let lease = j(&["job-claim", "--worker", "w1", "--kind", "reindex"]);
    let fence = lease["fence"].as_u64().unwrap();

    // Stale fence is a typed error.
    let out = run(
        tmp.path(),
        Some(&cfg),
        &[
            "--format",
            "json",
            "--no-help",
            "job-complete",
            "--id",
            job_id,
            "--fence",
            &(fence + 9).to_string(),
            "--worker",
            "w1",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let err = stdout_json(&out);
    assert_eq!(err["code"], "lease_lost", "got {err}");

    let done = j(&[
        "job-complete",
        "--id",
        job_id,
        "--fence",
        &fence.to_string(),
        "--worker",
        "w1",
        "--result",
        "{\"ok\":true}",
    ]);
    assert_eq!(done["state"], "succeeded");

    let job = j(&["job-get", "--id", job_id]);
    assert_eq!(job["state"], "succeeded");
    assert_eq!(job["result"]["ok"], true);
}

#[test]
fn prepare_context_and_record_insertion_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = rocks_cfg(tmp.path());
    let j = |args: &[&str]| -> Value {
        let mut full = vec!["--format", "json", "--no-help"];
        full.extend(args.iter().copied());
        let out = run(tmp.path(), Some(&cfg), &full);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout_json(&out)
    };

    // Seed via the writer path.
    j(&[
        "apply-ops",
        "--json",
        r#"[{"op":"put","scope":"org/t","text":"deploy freeze starts friday","category":"semantic"},
            {"op":"put","scope":"org/t","text":"rollback needs two approvals","category":"semantic"}]"#,
    ]);

    // T4 typed request → proposal + manifest.
    let p = j(&[
        "prepare-context",
        "--task",
        "deploy freeze approvals",
        "--budget-tokens",
        "400",
        "--candidates",
        "6",
        "--request-id",
        "req-1",
    ]);
    assert_eq!(p["status"].as_str().unwrap(), "ready", "{p}");
    assert!(p["proposal_id"].is_string());
    assert_eq!(p["request_id"], "req-1");
    let blocks = p["blocks"].as_array().unwrap();
    assert!(!blocks.is_empty());
    // Manifest fidelity: rendered derives from the same blocks.
    let rendered = p["rendered"].as_str().unwrap();
    for b in blocks {
        assert!(rendered.contains(b["excerpt"].as_str().unwrap()));
    }
    assert!(p["manifest"]["candidates"].as_array().unwrap().len() >= blocks.len());

    // Actual-insertion ack: real subset accepted + replayed idempotently.
    let bid = blocks[0]["block_id"].as_str().unwrap();
    let ack = j(&[
        "record-insertion",
        "--trace-id",
        p["trace_id"].as_str().unwrap(),
        "--proposal-id",
        p["proposal_id"].as_str().unwrap(),
        "--host",
        "contract-test",
        "--inserted-json",
        &json!([{ "block_id": bid, "truncated": true }]).to_string(),
    ]);
    assert_eq!(ack["inserted"].as_array().unwrap()[0], bid);
    assert_eq!(ack["replayed"], false);
    let ack2 = j(&[
        "record-insertion",
        "--trace-id",
        p["trace_id"].as_str().unwrap(),
        "--proposal-id",
        p["proposal_id"].as_str().unwrap(),
        "--host",
        "contract-test",
        "--inserted-json",
        &json!([{ "block_id": bid, "truncated": true }]).to_string(),
    ]);
    assert_eq!(ack2["replayed"], true);

    // Fabricated block is a typed failure, exit nonzero.
    let bad = run(
        tmp.path(),
        Some(&cfg),
        &[
            "--format",
            "json",
            "--no-help",
            "record-insertion",
            "--trace-id",
            p["trace_id"].as_str().unwrap(),
            "--proposal-id",
            p["proposal_id"].as_str().unwrap(),
            "--block",
            "never-proposed",
        ],
    );
    assert!(!bad.status.success());
    assert_eq!(stdout_json(&bad)["code"], "invalid_request");
}
