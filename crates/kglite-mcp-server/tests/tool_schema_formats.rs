//! `tools/list` from the real binary carries only JSON Schema 2020-12 `format`
//! values. schemars labels Rust integers `uint`, `uint64`, `int64` and so on;
//! clients log them as unknown and an Ajv strict-mode validator rejects the
//! schema.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const STANDARD_FORMATS: &[&str] = &[
    "date-time",
    "date",
    "time",
    "duration",
    "email",
    "idn-email",
    "hostname",
    "idn-hostname",
    "ipv4",
    "ipv6",
    "uri",
    "uri-reference",
    "iri",
    "iri-reference",
    "uuid",
    "uri-template",
    "json-pointer",
    "relative-json-pointer",
    "regex",
];

/// Tools whose schemas the mcp-methods framework generates. Remove an entry
/// once the framework emits portable formats.
const FRAMEWORK_OWNED: &[&str] = &["grep", "list_source", "read_source"];

fn collect_formats(value: &Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            if let Some(format) = map.get("format").and_then(Value::as_str) {
                out.push((path.to_string(), format.to_string()));
            }
            for (key, child) in map {
                collect_formats(child, &format!("{path}/{key}"), out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_formats(child, &format!("{path}/{index}"), out);
            }
        }
        _ => {}
    }
}

fn tools_list(args: &[&str]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kglite-mcp-server"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    for frame in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2024-11-05","capabilities":{},
            "clientInfo":{"name":"formats","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    ] {
        writeln!(stdin, "{frame}").expect("write");
    }
    stdin.flush().expect("flush");
    let mut line = String::new();
    let tools = loop {
        line.clear();
        assert!(
            stdout.read_line(&mut line).expect("read") > 0,
            "server closed"
        );
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message["id"] == json!(2) {
            break message["result"]["tools"]
                .as_array()
                .expect("tools")
                .clone();
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    tools
}

fn assert_portable(args: &[&str]) {
    let tools = tools_list(args);
    assert!(
        tools.iter().any(|t| t["name"] == "cypher_query"),
        "cypher_query is listed"
    );
    let mut offenders = Vec::new();
    for tool in &tools {
        let name = tool["name"].as_str().expect("name");
        if FRAMEWORK_OWNED.contains(&name) {
            continue;
        }
        let mut found = Vec::new();
        collect_formats(&tool["inputSchema"], "inputSchema", &mut found);
        collect_formats(&tool["outputSchema"], "outputSchema", &mut found);
        for (path, format) in found {
            if !STANDARD_FORMATS.contains(&format.as_str()) {
                offenders.push(format!("{name}: {path} = {format}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "non-standard formats:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn a_vault_servers_tool_schemas_use_only_standard_formats() {
    let vault = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/okf/golden/vault")
        .canonicalize()
        .expect("fixture");
    let copy = tempfile::tempdir().expect("tempdir");
    fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).expect("mkdir");
        for entry in std::fs::read_dir(from).expect("read_dir").flatten() {
            let target = to.join(entry.file_name());
            if entry.file_type().expect("type").is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).expect("copy");
            }
        }
    }
    copy_tree(&vault, copy.path());
    assert_portable(&["--vault", &copy.path().to_string_lossy()]);
}
