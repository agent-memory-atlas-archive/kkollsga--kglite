#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kglite::api::session::{execute_mut, ExecuteOptions};
use kglite::api::storage::{new_dir_graph_in_mode, StorageMode};
use kglite_mcp_server::{
    run_with_extensions, ServerExtensions, WorkspaceGraphHooks, WorkspaceGraphResult,
};

const CHILD_ENV: &str = "KGLITE_WATCH_COMPOSITION_CHILD";
const ROOT_ENV: &str = "KGLITE_WATCH_COMPOSITION_ROOT";
const RPC_TIMEOUT: Duration = Duration::from_secs(10);
const DEBOUNCE_SETTLE: Duration = Duration::from_millis(900);

fn producer_hooks() -> WorkspaceGraphHooks {
    WorkspaceGraphHooks {
        build: Box::new(|request| {
            let source = request.root().join("fixture.rs");
            let contents = std::fs::read_to_string(&source)
                .map_err(|error| format!("read {}: {error}", source.display()))?;
            let mut graph = new_dir_graph_in_mode(StorageMode::Memory, None)
                .map_err(|error| error.to_string())?;
            let params =
                HashMap::from([("contents".to_string(), kglite::api::Value::String(contents))]);
            let options = ExecuteOptions::eager(&params);
            execute_mut(
                &mut graph,
                "CREATE (:Source {id: 'fixture.rs', contents: $contents})",
                &options,
            )
            .map_err(|error| error.to_string())?;
            Ok(WorkspaceGraphResult::new(Arc::new(graph)))
        }),
        is_relevant: Box::new(|change| {
            change
                .path()
                .extension()
                .is_some_and(|extension| extension == "rs")
        }),
    }
}

#[test]
fn producer_server_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let root = std::env::var_os(ROOT_ENV).expect("child root");
    let extensions = ServerExtensions::default().with_workspace_graph(producer_hooks());
    let manifest = std::path::PathBuf::from(root).join("manifest.yaml");
    run_with_extensions(
        [
            "kglite-watch-composition".into(),
            "--mcp-config".into(),
            manifest.into_os_string(),
            "--writable".into(),
        ],
        extensions,
    )
    .expect("serve watch fixture");
}

struct ServerChild {
    child: Child,
    responses: mpsc::Receiver<std::io::Result<String>>,
    stderr: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    next_id: u64,
}

impl ServerChild {
    fn spawn(root: &std::path::Path) -> Self {
        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "producer_server_child", "--nocapture"])
            .env(CHILD_ENV, "1")
            .env(ROOT_ENV, root)
            .env("RUST_LOG", "mcp_methods=info,kglite_mcp_server=info")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn composed server");
        let stdout = child.stdout.take().expect("child stdout");
        let stderr = child.stderr.take().expect("child stderr");
        let (response_tx, responses) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if response_tx.send(line).is_err() {
                    break;
                }
            }
        });
        let (stderr_tx, stderr_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stderr
                .take(64 * 1024)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = stderr_tx.send(result);
        });
        Self {
            child,
            responses,
            stderr: stderr_rx,
            next_id: 1,
        }
    }

    fn send(&mut self, frame: serde_json::Value) {
        let stdin = self.child.stdin.as_mut().expect("child stdin");
        writeln!(stdin, "{frame}").expect("write MCP frame");
        stdin.flush().expect("flush MCP frame");
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }));
        let deadline = Instant::now() + RPC_TIMEOUT;
        loop {
            let line = self
                .responses
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("{method} response timed out: {error}"))
                .unwrap_or_else(|error| panic!("read {method} response: {error}"));
            if !line.starts_with('{') {
                continue;
            }
            let frame: serde_json::Value = serde_json::from_str(&line)
                .unwrap_or_else(|error| panic!("invalid frame {line:?}: {error}"));
            if frame["id"] == id {
                assert!(frame.get("error").is_none(), "{method} failed: {frame}");
                return frame["result"].clone();
            }
        }
    }

    fn initialize(&mut self) {
        self.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "watch-composition", "version": "1"}
            }),
        );
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        }));
    }

    fn call(&mut self, name: &str, arguments: serde_json::Value) -> String {
        let result = self.request(
            "tools/call",
            serde_json::json!({"name": name, "arguments": arguments}),
        );
        assert_ne!(result["isError"], true, "{name} failed: {result}");
        result["content"]
            .as_array()
            .expect("tool content")
            .iter()
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn stop(mut self) -> String {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if self.child.try_wait().expect("child status").is_some() {
                break;
            }
            if Instant::now() >= deadline {
                self.child.kill().expect("kill timed-out child");
                self.child.wait().expect("reap timed-out child");
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.stderr
            .recv_timeout(Duration::from_secs(1))
            .ok()
            .and_then(Result::ok)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_else(|| "<stderr unavailable>".to_string())
    }
}

impl Drop for ServerChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn source_reads_retain_graph_only_state_and_real_edits_rebuild() {
    let fixture = tempfile::tempdir().expect("watch fixture");
    let source = fixture.path().join("fixture.rs");
    std::fs::write(&source, "fn initial() {}\n").expect("initial source");
    std::fs::write(
        fixture.path().join("manifest.yaml"),
        "name: Watch composition\nworkspace:\n  kind: local\n  root: .\n  sandbox_root: .\n  watch: true\n",
    )
    .expect("watch manifest");
    let mut server = ServerChild::spawn(fixture.path());
    server.initialize();
    let activated = server.call(
        "set_root_dir",
        serde_json::json!({"path": fixture.path().to_string_lossy()}),
    );
    assert!(
        activated.contains("Graph ready: 1 nodes"),
        "activation: {activated}"
    );

    let created = server.call(
        "cypher_query",
        serde_json::json!({"query": "CREATE (:GraphOnly {id: 'marker'})"}),
    );
    assert!(created.contains("1 node"), "marker creation: {created}");

    // The producer's activation-time source read is inside this window. A Linux
    // Access(Open/Read) event must not survive debounce and replace the graph.
    std::thread::sleep(DEBOUNCE_SETTLE);
    let read = server.call(
        "read_source",
        serde_json::json!({"file_path": "fixture.rs"}),
    );
    assert!(read.contains("fn initial()"), "source read: {read}");
    std::thread::sleep(DEBOUNCE_SETTLE);
    let retained = server.call(
        "cypher_query",
        serde_json::json!({"query": "MATCH (n:GraphOnly) RETURN count(n) AS markers"}),
    );
    assert!(
        retained.contains("markers\n1"),
        "marker was replaced after reads: {retained}"
    );

    std::fs::write(&source, "fn edited() {}\n").expect("edit source");
    std::thread::sleep(DEBOUNCE_SETTLE);
    let rebuilt = server.call(
        "cypher_query",
        serde_json::json!({
            "query": "MATCH (s:Source) RETURN s.contents AS contents"
        }),
    );
    assert!(
        rebuilt.contains("fn edited()"),
        "real edit did not rebuild: {rebuilt}"
    );
    let replaced = server.call(
        "cypher_query",
        serde_json::json!({"query": "MATCH (n:GraphOnly) RETURN count(n) AS markers"}),
    );
    assert!(
        replaced.contains("markers\n0"),
        "rebuild retained graph-only marker: {replaced}"
    );

    let stderr = server.stop();
    assert!(
        stderr.contains("watch: file change debounced"),
        "missing mutation event log: {stderr}"
    );
}
