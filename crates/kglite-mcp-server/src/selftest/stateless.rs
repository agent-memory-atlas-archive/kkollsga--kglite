//! The stateless half of `--selftest`: a client on protocol 2026-07-28 never
//! sends `initialize`. It opens with `server/discover`, repeats its identity in
//! every request's `_meta`, and hears list changes only on an open
//! `subscriptions/listen` stream. A server that passes the handshake pass can
//! still reject every call from such a client, so this pass reports its own
//! lines.

use std::ffi::OsString;

use anyhow::Result;
use serde_json::{json, Value};

use super::{check_hydration, registered, snippet, successful_call_text, Check, Mode, Rpc};

/// The revision a stateless client names in `_meta`.
const STATELESS_VERSION: &str = "2026-07-28";

/// The `_meta` a stateless client attaches to every request.
fn client_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": STATELESS_VERSION,
        "io.modelcontextprotocol/clientInfo": {
            "name": "kglite-selftest",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

/// Spawn a second child and drive it with no `initialize`, pushing one line per
/// step. A step that cannot run because an earlier one failed is not pushed:
/// the earlier red line already says why.
pub(super) fn run(
    child_args: &[OsString],
    mode: &Mode,
    checks: &mut Vec<(&'static str, Check)>,
) -> Result<()> {
    let mut rpc = Rpc::spawn(child_args)?;
    rpc.meta = Some(client_meta());

    match rpc.request("server/discover", json!({})) {
        Ok(result) => {
            let supports = result
                .get("supportedVersions")
                .and_then(Value::as_array)
                .is_some_and(|v| v.iter().any(|v| v.as_str() == Some(STATELESS_VERSION)));
            if !supports {
                checks.push((
                    "stateless discover",
                    Check::Fail(format!(
                        "server/discover does not list {STATELESS_VERSION}: {}",
                        snippet(&result.to_string())
                    )),
                ));
                return Ok(());
            }
            let name = result
                .pointer("/_meta/io.modelcontextprotocol~1serverInfo/name")
                .and_then(Value::as_str)
                .unwrap_or("(unnamed)");
            checks.push((
                "stateless discover",
                Check::Pass(format!(
                    "server/discover → {name} speaks {STATELESS_VERSION}"
                )),
            ));
        }
        Err(e) => {
            checks.push(("stateless discover", Check::Fail(e.to_string())));
            return Ok(());
        }
    }

    let names = match rpc.request("tools/list", json!({})) {
        Ok(result) => result
            .get("tools")
            .and_then(Value::as_array)
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        Err(e) => {
            checks.push(("stateless tools/list", Check::Fail(e.to_string())));
            return Ok(());
        }
    };
    checks.push((
        "stateless tools/list",
        Check::Pass(format!("{} tools, no initialize", names.len())),
    ));

    checks.push(("stateless tools/call", call_check(&mut rpc, &names)));
    // A graph query where the mode has a graph at boot, so the call exercises
    // the same path as the handshake pass's hydration check.
    if matches!(
        mode,
        Mode::Graph { .. } | Mode::Watch { .. } | Mode::Vault { .. }
    ) {
        checks.push((
            "stateless graph query",
            check_hydration(&mut rpc, mode, false)?,
        ));
    }

    checks.push(("stateless list-changed stream", listen_check(&mut rpc)?));
    Ok(())
}

/// A real `tools/call` that works in every mode: `ping`, the framework's
/// always-present liveness route. Without it the graph query line is the
/// `tools/call` evidence.
fn call_check(rpc: &mut Rpc, names: &[String]) -> Check {
    if registered(names, "ping") {
        return match rpc.request("tools/call", json!({"name": "ping", "arguments": {}})) {
            Ok(result) => match successful_call_text(&result) {
                Ok(text) => Check::Pass(format!("ping → {}", snippet(&text))),
                Err(detail) => Check::Fail(detail),
            },
            Err(e) => Check::Fail(e.to_string()),
        };
    }
    Check::Skip("no `ping` route registered; the graph query line covers tools/call".into())
}

/// `subscriptions/listen` must be accepted and acknowledged; the stream then
/// stays open for the life of the child, so there is no response to wait for.
fn listen_check(rpc: &mut Rpc) -> Result<Check> {
    let id = rpc.send_request(
        "subscriptions/listen",
        json!({"notifications": {"toolsListChanged": true, "promptsListChanged": true}}),
    )?;
    loop {
        let frame = rpc.recv_frame("subscriptions/listen")?;
        if frame.get("id").and_then(Value::as_i64) == Some(id) {
            let detail = frame
                .get("error")
                .map_or_else(|| frame.to_string(), Value::to_string);
            return Ok(Check::Fail(format!(
                "subscriptions/listen was rejected or ended: {detail}"
            )));
        }
        if frame.get("method").and_then(Value::as_str)
            == Some("notifications/subscriptions/acknowledged")
        {
            return Ok(Check::Pass(
                "subscriptions/listen acknowledged; list-changed notifications reach this client"
                    .into(),
            ));
        }
    }
}
