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

use base64::Engine as _;
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

    /// The raw `result` of a tool call with arguments — the caller inspects
    /// the content blocks itself, which a `String` return would have thrown
    /// away.
    fn call_raw(&mut self, name: &str, arguments: serde_json::Value) -> serde_json::Value {
        let response = self.request(
            "tools/call",
            serde_json::json!({"name": name, "arguments": arguments}),
        );
        response
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("{name} failed: {response}"))
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

    // The vault's own recipe catalogue reaches the router only because vault
    // mode is aliased in `graph_layer.rs` — one of the three sites that fail
    // silently when the arm is missed. The routes are the observable.
    assert!(
        tools.iter().any(|name| name == "list_recipe_queries")
            && tools.iter().any(|name| name == "run_recipe_query"),
        "the vault's carried recipes are served as routes: {tools:?}"
    );

    // And the nodes themselves are in the graph, which is what a rebuild
    // re-reads them from.
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

/// Bytes that are recognisably not text and do not compress to nothing:
/// 40 000 bytes, big enough that their base64 is well past the mcp-methods
/// response budget (16 384 by default) and so the *only* way this survives
/// the round trip is the non-text exemption.
fn large_png_bytes() -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut state: u32 = 0x1234_5678;
    while bytes.len() < 40_000 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        bytes.push((state >> 24) as u8);
    }
    bytes
}

fn image_blocks(result: &serde_json::Value) -> Vec<(String, String)> {
    result["content"]
        .as_array()
        .expect("content blocks")
        .iter()
        .filter(|block| block["type"] == "image")
        .map(|block| {
            (
                block["mimeType"].as_str().expect("mimeType").to_string(),
                block["data"].as_str().expect("data").to_string(),
            )
        })
        .collect()
}

fn summary_text(result: &serde_json::Value) -> String {
    result["content"]
        .as_array()
        .expect("content blocks")
        .iter()
        .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

/// **This test requires mcp-methods ≥ 0.4.12 and is expected red until the
/// dependency bump lands.** The budget exemption for results carrying non-text
/// content blocks is `054fb5c` on mcp-methods' `feat/binary-content-budget-exemption`
/// branch, unpublished at the time of writing; against the pinned 0.4.11 this
/// test fails exactly the way P13a predicted — the image block is replaced by a
/// truncated JSON text preview. It is deliberately **not** `#[ignore]`d: a red
/// test naming its cause is the contract, and an ignored one would go green by
/// disappearing.
///
/// Everything else about `fetch_images` is covered by the unit tests in
/// `src/fetch_images_tests.rs`, which do not cross the protocol boundary. What
/// only this test can see is whether the bytes survive the *server's* response
/// pipeline.
#[test]
fn fetch_images_delivers_image_blocks_through_the_real_response_budget() {
    let vault = golden_vault_copy();
    let large = large_png_bytes();
    std::fs::write(vault.path().join("img/large.png"), &large).expect("write large png");
    let mut server = Server::boot(&["--vault", &vault.path().to_string_lossy()]);

    let tools = server.tool_names();
    assert!(
        tools.iter().any(|name| name == "fetch_images"),
        "a vault has a source root, so the route is enabled: {tools:?}"
    );

    // The id comes from the graph, not from the test's own idea of it: under
    // the vault model an `Image` id *is* its vault-relative path, and this is
    // the assertion that keeps the two the same string.
    let ids = server.call_raw(
        "cypher_query",
        serde_json::json!({"query": "MATCH (i:Image) WHERE i.id = 'img/faults.png' RETURN i.id AS id"}),
    );
    let image_id = ids["structuredContent"]["rows"][0][0]
        .as_str()
        .unwrap_or_else(|| panic!("the fixture's Image node: {ids}"))
        .to_string();
    assert_eq!(image_id, "img/faults.png");

    let result = server.call_raw(
        "fetch_images",
        serde_json::json!({"items": [image_id, "img/large.png"]}),
    );
    assert_ne!(
        result.get("isError"),
        Some(&serde_json::Value::Bool(true)),
        "both items are deliverable: {result}"
    );

    let blocks = image_blocks(&result);
    assert_eq!(
        blocks.len(),
        2,
        "one image block per delivered item — a text-only result here is the \
         response budget replacing them with a preview (mcp-methods < 0.4.12): {result}"
    );
    assert_eq!(blocks[0].0, "image/png");
    assert_eq!(blocks[1].0, "image/png");

    let on_disk = std::fs::read(vault.path().join("img/faults.png")).expect("fixture png");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&blocks[0].1)
        .expect("valid base64");
    assert_eq!(decoded, on_disk, "the fixture image's exact bytes");
    let decoded_large = base64::engine::general_purpose::STANDARD
        .decode(&blocks[1].1)
        .expect("valid base64");
    assert_eq!(
        decoded_large, large,
        "and the 40 000-byte one, whole — this is the byte count the budget would have cut"
    );

    let summary = summary_text(&result);
    assert!(summary.contains("2 delivered, 0 refused"), "{summary}");
}

/// The refusals an operator will actually hit, over the wire: a non-image
/// attachment and a file past the per-image cap. Neither needs the budget
/// exemption — both results are text — so this stays green on 0.4.11 and is
/// the reason a red [`fetch_images_delivers_image_blocks_through_the_real_response_budget`]
/// is a dependency verdict rather than a broken tool.
#[test]
fn fetch_images_refuses_a_pdf_by_type_and_an_oversized_file_by_byte_count() {
    let vault = golden_vault_copy();
    // 5 MiB, past the 4 MiB per-image default, with a deliverable extension so
    // the size check is what stops it.
    std::fs::write(
        vault.path().join("img/huge.png"),
        vec![0u8; 5 * 1024 * 1024],
    )
    .expect("write huge png");
    let mut server = Server::boot(&["--vault", &vault.path().to_string_lossy()]);

    let pdf = server.call_raw(
        "fetch_images",
        serde_json::json!({"items": ["img/handbook.pdf"]}),
    );
    assert_eq!(
        pdf.get("isError"),
        Some(&serde_json::Value::Bool(true)),
        "nothing was delivered: {pdf}"
    );
    assert!(
        summary_text(&pdf).contains("`application/pdf` is not delivered"),
        "the refusal names the type: {pdf}"
    );

    let mixed = server.call_raw(
        "fetch_images",
        serde_json::json!({"items": ["img/huge.png", "img/faults.png"]}),
    );
    let summary = summary_text(&mixed);
    assert!(
        summary.contains(&format!(
            "- refused `img/huge.png`: {} bytes exceeds the per-image cap of {} bytes",
            5 * 1024 * 1024,
            4 * 1024 * 1024
        )),
        "the byte count is named, and it is never resized: {summary}"
    );
    assert!(
        summary.contains("1 delivered, 1 refused"),
        "a partial success is still a success: {summary}"
    );

    // Absolute addressing is refused before the sandbox is asked, so the error
    // names the contract rather than reporting a miss.
    let absolute = server.call_raw(
        "fetch_images",
        serde_json::json!({"items": [vault.path().join("img/faults.png").to_string_lossy()]}),
    );
    assert!(
        summary_text(&absolute).contains("absolute paths are refused"),
        "{absolute}"
    );
}

/// The skill only reaches an agent if it is both bundled and gated on a route
/// this mode registers — the `applies_when` half is invisible from the tool
/// list alone.
#[test]
fn the_fetch_images_skill_is_served_where_the_route_is() {
    let vault = golden_vault_copy();
    let mut server = Server::boot(&["--vault", &vault.path().to_string_lossy()]);
    let prompts = server.prompt_names();
    assert!(
        prompts.iter().any(|name| name == "fetch_images"),
        "the bundled skill is active wherever the route is enabled: {prompts:?}"
    );
}
