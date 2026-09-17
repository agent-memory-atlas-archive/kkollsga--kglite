//! End-to-end `--vault`: the shipped binary, a real vault on disk, and a real
//! JSON-RPC handshake.
//!
//! The subprocess harness rather than the in-process one, because what is
//! under test *is* the boot path — the mode dispatch, the first-party
//! producer, the route registration and the eager build, none of which a
//! hand-assembled `McpServer` exercises. And unlike
//! `workspace_watch_composition.rs` it is not Linux-gated: nothing here waits
//! for a filesystem event. The watcher's own leg is covered by the unit tests
//! in `src/vault_tests.rs`, which drive the dirty-tag entry point directly.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    stderr: mpsc::Receiver<String>,
    next_id: i64,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    fn boot(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kglite-mcp-server"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn server");
        let stdout = child.stdout.take().expect("stdout");
        let (line_tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line_tx.send(line).is_err() {
                    return;
                }
            }
        });
        let stderr_pipe = child.stderr.take().expect("stderr");
        let (err_tx, stderr) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr_pipe.take(256 * 1024).read_to_end(&mut bytes);
            let _ = err_tx.send(String::from_utf8_lossy(&bytes).into_owned());
        });
        let stdin = child.stdin.take().expect("stdin");
        let mut server = Server {
            child,
            stdin,
            lines,
            stderr,
            next_id: 0,
        };
        let initialize = server.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "vault-mode", "version": "1"}
            }),
        );
        assert!(
            initialize.get("result").is_some(),
            "initialize failed: {initialize}"
        );
        server.notify("notifications/initialized");
        server
    }

    fn send(&mut self, payload: serde_json::Value) {
        writeln!(self.stdin, "{payload}").expect("write request");
        self.stdin.flush().expect("flush request");
    }

    fn notify(&mut self, method: &str) {
        self.send(serde_json::json!({"jsonrpc": "2.0", "method": method}));
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }));
        loop {
            let line = match self.lines.recv_timeout(Duration::from_secs(60)) {
                Ok(line) => line,
                Err(_) => panic!("no response to {method}; stderr: {}", self.drain_stderr()),
            };
            let value: serde_json::Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                // Anything that is not JSON-RPC on stdout would be a protocol
                // bug in itself; surface it rather than hanging on the next
                // read.
                Err(e) => panic!("non-JSON line on stdout ({e}): {line}"),
            };
            if value.get("id").and_then(serde_json::Value::as_i64) == Some(id) {
                return value;
            }
        }
    }

    fn call(&mut self, name: &str) -> String {
        let response = self.request(
            "tools/call",
            serde_json::json!({"name": name, "arguments": {}}),
        );
        Self::text_of(&response, name)
    }

    fn text_of(response: &serde_json::Value, what: &str) -> String {
        let result = response
            .get("result")
            .unwrap_or_else(|| panic!("{what} failed: {response}"));
        assert_ne!(
            result.get("isError"),
            Some(&serde_json::Value::Bool(true)),
            "{what} returned an error: {result}"
        );
        result["content"]
            .as_array()
            .expect("content blocks")
            .iter()
            .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn tool_names(&mut self) -> Vec<String> {
        let response = self.request("tools/list", serde_json::json!({}));
        response["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect()
    }

    /// One `RETURN count(x) AS n` row, read out of the structured result
    /// rather than the rendered text — a substring match on "1" would pass
    /// against half the footers the text carries.
    fn count(&mut self, cypher: &str) -> i64 {
        let response = self.request(
            "tools/call",
            serde_json::json!({"name": "cypher_query", "arguments": {"query": cypher}}),
        );
        let result = &response["result"];
        assert_ne!(
            result.get("isError"),
            Some(&serde_json::Value::Bool(true)),
            "cypher_query returned an error: {result}"
        );
        result["structuredContent"]["rows"][0][0]
            .as_i64()
            .unwrap_or_else(|| panic!("a single count row: {result}"))
    }

    fn prompt_names(&mut self) -> Vec<String> {
        let response = self.request("prompts/list", serde_json::json!({}));
        response["result"]["prompts"]
            .as_array()
            .expect("prompts array")
            .iter()
            .filter_map(|prompt| prompt["name"].as_str().map(str::to_owned))
            .collect()
    }

    fn drain_stderr(&mut self) -> String {
        self.stderr
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|_| "<stderr unavailable>".to_string())
    }
}

/// A copy of the golden vault, so a test that edits notes cannot touch the
/// fixture every other okf suite asserts against.
fn golden_vault_copy() -> tempfile::TempDir {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/okf/golden/vault")
        .canonicalize()
        .expect("the golden vault fixture");
    let temp = tempfile::tempdir().expect("tempdir");
    copy_tree(&source, temp.path());
    temp
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

#[test]
fn a_vault_server_boots_serves_its_notes_and_carries_its_skills() {
    let vault = golden_vault_copy();
    let mut server = Server::boot(&["--vault", &vault.path().to_string_lossy()]);

    let tools = server.tool_names();
    assert!(
        tools.iter().any(|name| name == "rebuild_graph"),
        "vault mode registers rebuild_graph: {tools:?}"
    );
    assert!(
        !tools.iter().any(|name| name == "reload_graph"),
        "there is no served file to re-read: {tools:?}"
    );

    // The producer ran: without the `Mode::Vault` arm on `workspace_graph_mode`
    // this is an empty graph and every count below is zero.
    let overview = server.call("graph_overview");
    assert!(
        overview.contains("Article"),
        "the vault's notes must be in the graph: {overview}"
    );

    assert_eq!(
        server.count("MATCH (n:Article) RETURN count(n) AS n"),
        11,
        "the golden vault's Articles"
    );

    // The graph-carried layers — `.kglite/skills/` and `.kglite/recipes/` —
    // reach the graph only because vault mode is aliased at `skills.rs` and
    // `graph_layer.rs`. Both fail silently when the arm is missed.
    assert_eq!(
        server.count("MATCH (s:KgliteSkill) RETURN count(s) AS n"),
        1,
        "the vault's own skill is carried into the graph"
    );
    assert_eq!(
        server.count("MATCH (r:KgliteRecipe) RETURN count(r) AS n"),
        1,
        "and so is its recipe"
    );
}

#[test]
fn rebuild_graph_picks_up_a_new_note_and_reports_the_build() {
    let vault = golden_vault_copy();
    let mut server = Server::boot(&["--vault", &vault.path().to_string_lossy()]);

    assert_eq!(server.count("MATCH (n:Article) RETURN count(n) AS n"), 11);

    std::fs::write(
        vault.path().join("notes/fresh.md"),
        "---\ntitle: Fresh\n---\nWritten after boot.\n",
    )
    .expect("write note");

    let report = server.call("rebuild_graph");
    assert!(
        report.contains("Article"),
        "the reply is the build report: {report}"
    );

    assert_eq!(
        server.count("MATCH (n:Article) RETURN count(n) AS n"),
        12,
        "the rebuilt graph carries the new note"
    );
}

/// A `.kglite/vault.yaml` the build cannot read must fail loudly and leave the
/// booted graph serving — not empty it, which would read as "the vault has no
/// notes" rather than "the vault is misconfigured".
#[test]
fn a_broken_config_fails_the_rebuild_and_keeps_serving() {
    let vault = golden_vault_copy();
    let mut server = Server::boot(&["--vault", &vault.path().to_string_lossy()]);

    std::fs::write(vault.path().join(".kglite/vault.yaml"), "kglite_vault: 7\n")
        .expect("break the config");

    let response = server.request(
        "tools/call",
        serde_json::json!({"name": "rebuild_graph", "arguments": {}}),
    );
    let result = &response["result"];
    assert_eq!(
        result.get("isError"),
        Some(&serde_json::Value::Bool(true)),
        "a build that cannot run is an error, not an empty graph: {response}"
    );
    let message = Server::text_of(
        &serde_json::json!({"result": {"content": result["content"]}}),
        "rebuild_graph",
    );
    assert!(
        message.contains("kglite_vault"),
        "the refusal names what is wrong: {message}"
    );

    assert_eq!(
        server.count("MATCH (n:Article) RETURN count(n) AS n"),
        11,
        "the previously built graph is still served"
    );
}

/// `--vault` and the four other mode flags are mutually exclusive at the clap
/// level, so a typo'd invocation fails instead of silently picking one.
#[test]
fn the_mode_flags_stay_mutually_exclusive() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path().to_string_lossy().into_owned();
    let output = Command::new(env!("CARGO_BIN_EXE_kglite-mcp-server"))
        .args(["--vault", &dir, "--watch", &dir])
        .output()
        .expect("run server");
    assert!(!output.status.success(), "two modes must not both apply");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--vault") || stderr.contains("--watch"),
        "clap names the conflict: {stderr}"
    );
}

/// D6's other half, over the wire: a vault carries its own skills, and editing
/// one has to reach the served registry. Before the rebuild path learned to
/// refresh, the boot-resolved set was served until restart — the edit rebuilt
/// the graph and changed nothing an agent could see.
#[test]
fn an_edited_carried_skill_is_served_after_a_rebuild() {
    let vault = golden_vault_copy();
    let mut server = Server::boot(&["--vault", &vault.path().to_string_lossy()]);

    let booted = server.prompt_names();
    assert!(
        booted.iter().any(|name| name == "vault_overview"),
        "the vault's own skill is served at boot: {booted:?}"
    );
    assert!(
        booted.iter().any(|name| name == "vault_authoring"),
        "the bundled authoring skill is gated on rebuild_graph being registered: {booted:?}"
    );

    std::fs::write(
        vault.path().join(".kglite/skills/one.md"),
        "---\nname: vault_overview_v2\ndescription: The same vault, renamed skill.\n---\n\nBody.\n",
    )
    .expect("edit the carried skill");
    server.call("rebuild_graph");

    let after = server.prompt_names();
    assert!(
        after.iter().any(|name| name == "vault_overview_v2"),
        "the rebuilt graph's skills must be the served ones: {after:?}"
    );
    assert!(
        !after.iter().any(|name| name == "vault_overview"),
        "and the replaced one must be gone: {after:?}"
    );
}
