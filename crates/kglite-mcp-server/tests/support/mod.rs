//! Real stdio sessions against copies of the built server binary.
#![allow(dead_code)] // each test file uses a different part of the harness

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};

/// Serialises every binary copy and spawn in a test process: executing a file
/// another thread still holds open for writing fails with ETXTBSY on Linux.
static BINARY_LOCK: Mutex<()> = Mutex::new(());

pub fn server_binary() -> &'static str {
    env!("CARGO_BIN_EXE_kglite-mcp-server")
}

/// Copy `from` to `to` as an executable.
pub fn install_binary(from: &Path, to: &Path) {
    let _guard = BINARY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::copy(from, to).expect("copy binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

/// Replace `target` the way a package manager does: a new file renamed over it.
pub fn replace_binary(from: &Path, target: &Path) {
    let staged = target.with_extension("new");
    install_binary(from, &staged);
    let _guard = BINARY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::rename(&staged, target).expect("rename over");
}

pub struct Session {
    pub child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    pub stderr_log: std::sync::Arc<Mutex<String>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Session {
    pub fn spawn<S: AsRef<OsStr>>(
        program: &Path,
        args: &[S],
        envs: &[(&str, &Path)],
        cwd: Option<&Path>,
    ) -> Session {
        let _guard = BINARY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in envs {
            command.env(key, value);
        }
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().expect("spawn server");
        drop(_guard);
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if tx.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        let stderr_log = std::sync::Arc::new(Mutex::new(String::new()));
        let log = stderr_log.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
                log.push_str(&line);
                log.push('\n');
            }
        });
        Session {
            child,
            stdin,
            rx,
            stderr_log,
        }
    }

    pub fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").expect("write request");
        self.stdin.flush().expect("flush request");
    }

    /// The response with this id; other messages (notifications) are skipped.
    pub fn recv_id(&mut self, id: i64, timeout: Duration) -> Value {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(message) if message["id"] == json!(id) => return message,
                Ok(_) => continue,
                Err(_) => panic!(
                    "no response to id {id}; stderr:\n{}",
                    self.stderr_log.lock().unwrap()
                ),
            }
        }
    }

    pub fn initialize(&mut self) -> Value {
        self.send(
            &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion":"2024-11-05","capabilities":{},
            "clientInfo":{"name":"harness","version":"1"}}}),
        );
        let response = self.recv_id(0, Duration::from_secs(20));
        self.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        response
    }

    pub fn request(&mut self, id: i64, method: &str, params: Value) {
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
    }

    pub fn call(&mut self, id: i64, tool: &str, arguments: Value) -> Value {
        self.request(
            id,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        );
        self.recv_id(id, Duration::from_secs(30))
    }
}

/// All text blocks of a tool result joined.
pub fn result_text(response: &Value) -> String {
    response["result"]["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

pub fn bare_workspace(dir: &Path) -> PathBuf {
    let manifest = dir.join("manifest.yaml");
    std::fs::write(
        &manifest,
        "name: Self update fixture\nworkspace:\n  kind: local\n  root: .\n  sandbox_root: .\n",
    )
    .expect("write manifest");
    manifest
}
