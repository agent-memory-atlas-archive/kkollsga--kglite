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
    run_with_extensions, Delivery, RecipeCatalog, ServerExtensions, SkillRecord,
    WorkspaceGraphHooks, WorkspaceGraphResult,
};

const CHILD_ENV: &str = "KGLITE_WATCH_COMPOSITION_CHILD";
const ROOT_ENV: &str = "KGLITE_WATCH_COMPOSITION_ROOT";
/// Set by the methodology test only, so the watch test keeps measuring a
/// producer that contributes nothing but the graph builder.
const METHODOLOGY_ENV: &str = "KGLITE_WATCH_COMPOSITION_METHODOLOGY";
const PRODUCER_SKILL: &str = "fixture_methodology";
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

/// What a producer registers once per server: the methodology for the shapes
/// its builder always emits, and the queries that answer them. Neither is in
/// the manifest and neither is in any graph — this is the codingest shape.
fn producer_methodology(extensions: ServerExtensions) -> ServerExtensions {
    let catalog = RecipeCatalog::from_manifest_value(Some(&serde_json::json!({
        "fixture": {
            "description": "Queries for the shapes this builder emits.",
            "queries": {
                "sources": {
                    "description": "Every source file in the active graph.",
                    "parameters": {
                        "type": "object",
                        "properties": {},
                        "required": [],
                        "additionalProperties": false
                    },
                    "cypher": "MATCH (s:Source) RETURN s.id AS id"
                }
            }
        }
    })))
    .expect("a valid producer catalogue");
    extensions
        .with_skills([SkillRecord {
            name: PRODUCER_SKILL.to_string(),
            description: "How this builder's graphs are shaped.".to_string(),
            body: "Every graph this server serves carries `:Source` nodes keyed by `id`.\n"
                .to_string(),
            references_tools: vec!["cypher_query".to_string()],
            delivery: Delivery::Lazy,
        }])
        .with_recipes(catalog)
}

#[test]
fn producer_server_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let root = std::env::var_os(ROOT_ENV).expect("child root");
    let mut extensions = ServerExtensions::default().with_workspace_graph(producer_hooks());
    if std::env::var_os(METHODOLOGY_ENV).is_some() {
        extensions = producer_methodology(extensions);
    }
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
        Self::spawn_with(root, false)
    }

    fn spawn_with(root: &std::path::Path, methodology: bool) -> Self {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        if methodology {
            command.env(METHODOLOGY_ENV, "1");
        }
        let mut child = command
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

/// The producer-methodology contract, end to end through a real MCP handshake:
/// an embedder that registers skills and a recipe catalogue once per server
/// serves both for every graph it builds, in a workspace mode, with **no
/// manifest `skills:` declaration and no graph at boot**. Every unit test under
/// `src/` composes a registry or a catalogue directly; only this one proves the
/// wiring survives `run_with_extensions`, `initialize` and the frozen
/// capability set.
#[test]
fn a_producer_registers_its_methodology_for_every_graph_it_serves() {
    let fixture = tempfile::tempdir().expect("methodology fixture");
    std::fs::write(fixture.path().join("fixture.rs"), "fn initial() {}\n").expect("source");
    // Deliberately no `skills:` key: the producer's own opt-in is what has to
    // turn the skill plane on here.
    std::fs::write(
        fixture.path().join("manifest.yaml"),
        "name: Producer methodology\nworkspace:\n  kind: local\n  root: .\n  sandbox_root: .\n",
    )
    .expect("manifest");

    let mut server = ServerChild::spawn_with(fixture.path(), true);
    server.initialize();

    let tools = server.request("tools/list", serde_json::json!({}));
    let listed: Vec<String> = tools["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect();
    // The lazy-skill loader exists because a skill does; the recipe routes
    // exist because a catalogue does. Both come from the producer alone.
    assert!(listed.iter().any(|name| name == "skill"), "{listed:?}");
    assert!(
        listed.iter().any(|name| name == "run_recipe_query"),
        "{listed:?}"
    );
    assert!(
        listed.iter().any(|name| name == "list_recipe_queries"),
        "{listed:?}"
    );

    let cypher_description = tools["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "cypher_query")
        .and_then(|tool| tool["description"].as_str())
        .expect("cypher_query description")
        .to_string();
    assert!(
        cypher_description.contains(&format!("<!-- mcp-skill:{PRODUCER_SKILL} -->")),
        "producer skill not injected into cypher_query: {cypher_description}"
    );

    let body = server.call("skill", serde_json::json!({"name": PRODUCER_SKILL}));
    assert!(
        body.contains(":Source` nodes keyed by `id`"),
        "skill: {body}"
    );

    // No graph at boot in a workspace mode: the catalogue was registered before
    // one existed, and runs against whatever `set_root_dir` activates.
    let activated = server.call(
        "set_root_dir",
        serde_json::json!({"path": fixture.path().to_string_lossy()}),
    );
    assert!(
        activated.contains("Graph ready: 1 nodes"),
        "activation: {activated}"
    );
    let rows = server.call(
        "run_recipe_query",
        serde_json::json!({"recipe": "fixture", "query": "sources", "variables": {}}),
    );
    assert!(rows.contains("fixture.rs"), "recipe rows: {rows}");

    let stderr = server.stop();
    assert!(
        stderr.contains("producer skills: 1 served"),
        "boot summary missing the producer skills line: {stderr}"
    );
    assert!(
        stderr.contains("producer recipes: 1 served"),
        "boot summary missing the producer recipes line: {stderr}"
    );
}
