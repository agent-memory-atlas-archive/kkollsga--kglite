//! The shipped binary against clients on protocol 2026-07-28, which open with
//! `server/discover` and never send `initialize`, and against a handshake
//! client for contrast. A vault is the fixture because editing its carried
//! skill and calling `rebuild_graph` is a real tool-list change.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

const SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";
const WAIT: Duration = Duration::from_secs(60);

struct Client {
    child: Child,
    stdin: ChildStdin,
    frames: Receiver<Value>,
    /// Frames read while waiting for a response: a list-changed notification
    /// can arrive before the response of the call that caused it.
    skipped: VecDeque<Value>,
    next_id: i64,
    stateless: bool,
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Client {
    fn boot(vault: &Path, stateless: bool) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kglite-mcp-server"))
            .arg("--vault")
            .arg(vault)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server");
        let stdout = child.stdout.take().expect("stdout");
        let stdin = child.stdin.take().expect("stdin");
        let (tx, frames) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if tx.send(value).is_err() {
                        return;
                    }
                }
            }
        });
        Client {
            child,
            stdin,
            frames,
            skipped: VecDeque::new(),
            next_id: 0,
            stateless,
        }
    }

    fn meta() -> Value {
        json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientInfo": {"name": "stateless-test", "version": "1"},
            "io.modelcontextprotocol/clientCapabilities": {},
        })
    }

    fn send(&mut self, frame: Value) {
        writeln!(self.stdin, "{frame}").expect("write frame");
        self.stdin.flush().expect("flush frame");
    }

    fn recv(&mut self) -> Value {
        self.frames
            .recv_timeout(WAIT)
            .expect("a frame from the server")
    }

    /// Send a request (with `_meta` for a stateless client) and return its id.
    fn send_request(&mut self, method: &str, mut params: Value) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        if self.stateless {
            params["_meta"] = Self::meta();
        }
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send_request(method, params);
        loop {
            let frame = self.recv();
            if frame["id"] == json!(id) {
                return frame;
            }
            self.skipped.push_back(frame);
        }
    }

    fn handshake(&mut self) {
        let init = self.request(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {},
                   "clientInfo": {"name": "legacy-test", "version": "1"}}),
        );
        assert!(init.get("result").is_some(), "initialize failed: {init}");
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
    }

    /// The first frame with this `method`, from those already read or the wire.
    fn next_notification(&mut self, method: &str) -> Value {
        if let Some(at) = self
            .skipped
            .iter()
            .position(|f| f["method"] == json!(method))
        {
            return self.skipped.remove(at).expect("position is in range");
        }
        loop {
            let frame = self.recv();
            if frame["method"] == json!(method) {
                return frame;
            }
        }
    }
}

fn golden_vault_copy() -> tempfile::TempDir {
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
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/okf/golden/vault")
        .canonicalize()
        .expect("the golden vault fixture");
    let temp = tempfile::tempdir().expect("tempdir");
    copy_tree(&source, temp.path());
    temp
}

/// Rename the vault's carried skill: the next graph rebuild changes the
/// served skill layer, which announces a tool-list change.
fn rename_carried_skill(vault: &Path, name: &str) {
    std::fs::write(
        vault.join(".kglite/skills/one.md"),
        format!("---\nname: {name}\ndescription: A renamed skill.\n---\n\nBody.\n"),
    )
    .expect("edit the carried skill");
}

fn assert_ok(frame: &Value) {
    assert!(frame.get("error").is_none(), "request failed: {frame}");
    assert_ne!(
        frame["result"]["isError"],
        json!(true),
        "tool error: {frame}"
    );
}

#[test]
fn a_stateless_client_calls_tools_without_initialize() {
    let vault = golden_vault_copy();
    let mut client = Client::boot(vault.path(), true);
    assert_ok(&client.request("server/discover", json!({})));
    let listed = client.request("tools/list", json!({}));
    assert_ok(&listed);
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"cypher_query"), "{names:?}");
    let called = client.call(
        "cypher_query",
        json!({"query": "MATCH (n:Article) RETURN count(n) AS n"}),
    );
    assert_ok(&called);
}

#[test]
fn a_stateless_listen_stream_is_acknowledged_and_receives_the_tagged_list_change() {
    let vault = golden_vault_copy();
    let mut client = Client::boot(vault.path(), true);
    assert_ok(&client.request("server/discover", json!({})));
    let listen_id = client.send_request(
        "subscriptions/listen",
        json!({"notifications": {"toolsListChanged": true, "promptsListChanged": true}}),
    );
    client.next_notification("notifications/subscriptions/acknowledged");

    rename_carried_skill(vault.path(), "vault_overview_v2");
    assert_ok(&client.call("rebuild_graph", json!({})));

    let changed = client.next_notification("notifications/tools/list_changed");
    assert_eq!(
        changed["params"]["_meta"][SUBSCRIPTION_ID],
        json!(listen_id),
        "the notification is tagged with the listen request's id: {changed}"
    );
}

#[test]
fn a_handshake_client_still_gets_the_direct_list_change() {
    let vault = golden_vault_copy();
    let mut client = Client::boot(vault.path(), false);
    client.handshake();

    rename_carried_skill(vault.path(), "vault_overview_v2");
    assert_ok(&client.call("rebuild_graph", json!({})));

    let changed = client.next_notification("notifications/tools/list_changed");
    assert!(
        changed["params"]["_meta"].get(SUBSCRIPTION_ID).is_none(),
        "a direct notification carries no subscription id: {changed}"
    );
}
