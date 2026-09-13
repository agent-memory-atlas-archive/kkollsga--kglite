use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn relative_sandbox_initializes_when_launched_from_another_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    let launcher = tmp.path().join("launcher");
    std::fs::create_dir_all(&fixture).expect("mkdir fixture");
    std::fs::create_dir_all(&launcher).expect("mkdir launcher");
    std::fs::write(
        fixture.join("manifest.yaml"),
        "name: Relative sandbox subprocess\nworkspace:\n  kind: local\n  root: .\n  sandbox_root: .\n",
    )
    .expect("write manifest");

    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_kglite-mcp-server"))
            .args(["--mcp-config", "../fixture/manifest.yaml"])
            .current_dir(&launcher)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let stdout = child.0.stdout.take().expect("stdout");
    let stderr = child.0.stderr.take().expect("stderr");
    let (line_tx, line_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        let _ = line_tx.send(lines.next().transpose());
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

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "sandbox-regression", "version": "1"}
        }
    });
    writeln!(child.0.stdin.as_mut().expect("stdin"), "{request}").expect("send initialize");
    child
        .0
        .stdin
        .as_mut()
        .expect("stdin")
        .flush()
        .expect("flush initialize");

    let response = match line_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(Some(line))) => line,
        other => {
            let status = child.0.try_wait().expect("process status");
            drop(child);
            let stderr = stderr_rx
                .recv_timeout(Duration::from_secs(2))
                .ok()
                .and_then(Result::ok)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_else(|| "<stderr unavailable>".to_string());
            panic!("initialize response missing: {other:?}; status={status:?}; stderr={stderr}");
        }
    };
    let response: serde_json::Value = serde_json::from_str(&response).expect("JSON response");
    assert_eq!(response["id"], 1);
    assert!(
        response.get("result").is_some(),
        "initialize failed: {response}"
    );
}
