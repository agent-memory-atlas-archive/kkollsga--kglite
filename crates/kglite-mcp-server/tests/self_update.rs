//! A replaced server binary is reported on the next tool result.

mod support;

use serde_json::json;
use support::*;

#[test]
fn replaced_binary_adds_a_stale_notice_to_tool_results() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("a.txt"), "hello").unwrap();
    let manifest = bare_workspace(&work);
    let installed = tmp.path().join("kglite-mcp-server");
    install_binary(server_binary().as_ref(), &installed);

    let mut session = Session::spawn(
        &installed,
        &["--mcp-config".as_ref(), manifest.as_os_str()],
        &[],
        Some(&work),
    );
    let init = session.initialize();
    assert_eq!(
        init["result"]["serverInfo"]["version"],
        json!(env!("CARGO_PKG_VERSION"))
    );

    let before = result_text(&session.call(1, "list_source", json!({"path": "."})));
    assert!(before.contains("a.txt"), "{before}");
    assert!(
        !before.contains("stale:"),
        "fresh build must not warn: {before}"
    );

    replace_binary(server_binary().as_ref(), &installed);

    let after_ok = session.call(2, "list_source", json!({"path": "."}));
    let text = result_text(&after_ok);
    assert!(text.contains("a.txt"), "{text}");
    assert!(
        text.contains("stale: kglite-mcp-server") && text.contains("running"),
        "{text}"
    );
    assert!(
        text.contains(&format!("{} installed", env!("CARGO_PKG_VERSION"))),
        "the binary's `--version` supplies the installed version: {text}"
    );

    let unknown = session.call(3, "no_such_tool", json!({}));
    assert!(
        unknown.get("error").is_some() && !unknown.to_string().contains("stale:"),
        "a failed call carries no notice: {unknown}"
    );

    let ping = session.call(4, "ping", json!({}));
    let ping = result_text(&ping);
    assert!(ping.contains("(stale"), "ping reports the latch: {ping}");
}
