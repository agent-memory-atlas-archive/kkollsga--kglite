//! Unit cover for `fetch_images`: the addressing contract, the three caps,
//! the MIME allowlist, the mixed-result shape and the no-root disable.
//!
//! Everything below drives [`run`] against a real temp directory rather than
//! a mocked filesystem — the sandbox is the feature, and a fake root would
//! test the mock.

use super::*;
use serde_json::json;

fn args(value: Value) -> Map<String, Value> {
    value.as_object().cloned().expect("object arguments")
}

/// A root holding `img/faults.png` (a real PNG header), `img/handbook.pdf`
/// and `notes/logo.svg`.
fn root() -> (tempfile::TempDir, Vec<String>) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("img")).expect("mkdir");
    std::fs::create_dir_all(dir.path().join("notes")).expect("mkdir");
    std::fs::write(dir.path().join("img/faults.png"), PNG).expect("png");
    std::fs::write(dir.path().join("img/handbook.pdf"), b"%PDF-1.4\n").expect("pdf");
    std::fs::write(dir.path().join("notes/logo.svg"), b"<svg/>").expect("svg");
    let roots = vec![dir.path().to_string_lossy().into_owned()];
    (dir, roots)
}

/// The eight bytes of a PNG signature plus a byte that is not valid UTF-8 on
/// its own — so a base64 round-trip that silently went through a string
/// cannot pass.
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0xff];

fn refusal(outcomes: &[Outcome], path: &str) -> String {
    outcomes
        .iter()
        .find_map(|o| match o {
            Outcome::Refused { path: p, reason } if p == path => Some(reason.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("`{path}` was not refused: {outcomes:?}"))
}

#[test]
fn absolute_paths_and_climbs_are_refused_before_the_sandbox_is_consulted() {
    let (dir, _) = root();
    let inside = dir
        .path()
        .join("img/faults.png")
        .to_string_lossy()
        .into_owned();
    // The roots are deliberately empty: every refusal below must come from the
    // addressing rules, not from a resolution miss, or the agent is told the
    // file is absent when the real answer is that the address is illegal.
    let empty: Vec<String> = Vec::new();
    for (path, expected) in [
        (inside.as_str(), "absolute paths are refused"),
        ("/etc/passwd", "absolute paths are refused"),
        ("~/secrets/key.png", "absolute paths are refused"),
        ("C:\\Windows\\win.png", "absolute paths are refused"),
        ("\\\\server\\share\\x.png", "absolute paths are refused"),
        ("file:///etc/passwd", "`file:` URLs are refused"),
        ("../../etc/passwd.png", "`..` path segments are refused"),
        ("img/../../outside.png", "`..` path segments are refused"),
        ("img\\..\\..\\outside.png", "`..` path segments are refused"),
    ] {
        let error = run(
            &empty,
            &ImageCaps::default(),
            &args(json!({"items": [path]})),
        )
        .expect_err("a single refused item is an error result");
        assert!(
            error.contains(expected),
            "`{path}` must be refused with `{expected}`: {error}"
        );
        assert!(
            error.contains("vault-relative paths or `Image` node ids"),
            "the refusal names the contract: {error}"
        );
    }
}

#[test]
fn only_the_four_deliverable_types_are_served_and_the_rest_name_their_type() {
    let (_dir, roots) = root();
    let caps = ImageCaps::default();

    let svg = run(&roots, &caps, &args(json!({"items": ["notes/logo.svg"]})))
        .expect_err("svg is never delivered");
    assert!(
        svg.contains("`image/svg+xml` is not delivered"),
        "the refusal names the MIME type: {svg}"
    );

    let pdf = run(&roots, &caps, &args(json!({"items": ["img/handbook.pdf"]})))
        .expect_err("a pdf is not an image");
    assert!(
        pdf.contains("`application/pdf` is not delivered"),
        "the refusal names the MIME type: {pdf}"
    );

    let unknown = run(&roots, &caps, &args(json!({"items": ["img/thing.qoi"]})))
        .expect_err("an unknown extension is refused");
    assert!(
        unknown.contains("`.qoi` is not a delivered image type"),
        "{unknown}"
    );

    // …and the type check happens before resolution, so the four deliverable
    // extensions are the only ones that ever reach the filesystem.
    let outcomes = run(&roots, &caps, &args(json!({"items": ["img/faults.png"]})))
        .expect("the png is delivered");
    assert!(matches!(
        outcomes.as_slice(),
        [Outcome::Delivered {
            mime: "image/png",
            ..
        }]
    ));
}

#[test]
fn a_delivered_image_round_trips_its_exact_bytes_through_base64() {
    let (_dir, roots) = root();
    let outcomes = run(
        &roots,
        &ImageCaps::default(),
        &args(json!({"items": ["img/faults.png"]})),
    )
    .expect("delivered");
    let Outcome::Delivered { bytes, .. } = &outcomes[0] else {
        panic!("delivered: {outcomes:?}");
    };
    assert_eq!(bytes.as_slice(), PNG, "the file's bytes, unaltered");

    let result = result_of(&outcomes);
    let encoded = result.content[0]
        .as_image()
        .expect("an image content block, not a text preview");
    assert_eq!(encoded.mime_type, "image/png");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&encoded.data)
        .expect("valid base64");
    assert_eq!(decoded, PNG, "base64 must round-trip the file bytes");
}

#[test]
fn the_result_carries_one_image_block_per_delivery_then_one_summary_in_request_order() {
    let (_dir, roots) = root();
    let outcomes = run(
        &roots,
        &ImageCaps::default(),
        &args(json!({"items": ["img/handbook.pdf", "img/faults.png", "img/absent.png"]})),
    )
    .expect("one delivery makes the call a success");

    let paths: Vec<&str> = outcomes
        .iter()
        .map(|o| match o {
            Outcome::Delivered { path, .. } | Outcome::Refused { path, .. } => path.as_str(),
        })
        .collect();
    assert_eq!(
        paths,
        vec!["img/handbook.pdf", "img/faults.png", "img/absent.png"],
        "outcomes stay in request order"
    );
    assert!(
        refusal(&outcomes, "img/absent.png").contains("not found under the server's source root"),
        "a missing file is refused, not an error"
    );

    let result = result_of(&outcomes);
    assert_eq!(
        result.is_error,
        Some(false),
        "a partial success is a success"
    );
    assert_eq!(result.content.len(), 2, "one image block plus one summary");
    assert!(result.content[0].as_image().is_some());
    let summary = result.content[1]
        .as_text()
        .expect("summary text")
        .text
        .clone();
    assert!(
        summary.starts_with("fetch_images: 1 delivered, 2 refused."),
        "{summary}"
    );
    assert!(
        summary.contains("- delivered `img/faults.png` (image/png, 9 bytes)"),
        "{summary}"
    );
    assert!(
        summary.contains("- refused `img/handbook.pdf`:"),
        "{summary}"
    );
    assert!(summary.contains("- refused `img/absent.png`:"), "{summary}");
}

#[test]
fn every_item_refused_is_an_error_result_listing_the_reasons() {
    let (_dir, roots) = root();
    let error = run(
        &roots,
        &ImageCaps::default(),
        &args(json!({"items": ["notes/logo.svg", "img/absent.png"]})),
    )
    .expect_err("nothing was delivered");
    assert!(error.contains("no image was delivered"), "{error}");
    assert!(
        error.contains("notes/logo.svg") && error.contains("img/absent.png"),
        "{error}"
    );
}

#[test]
fn malformed_input_is_an_error_not_a_refusal() {
    let (_dir, roots) = root();
    let caps = ImageCaps::default();
    for (payload, expected) in [
        (json!({}), "`items` must be a list"),
        (json!({"items": "img/faults.png"}), "`items` must be a list"),
        (json!({"items": []}), "must name at least one image"),
        (json!({"items": [7]}), "entries must be strings"),
        (
            json!({"items": ["img/faults.png"], "max_bytes": 0}),
            "`max_bytes` must be a positive integer",
        ),
        (
            json!({"items": ["img/faults.png"], "max_bytes": "4mb"}),
            "`max_bytes` must be a positive integer",
        ),
    ] {
        let error =
            run(&roots, &caps, &args(payload.clone())).expect_err(&format!("malformed: {payload}"));
        assert!(error.contains(expected), "{payload} -> {error}");
    }
}

#[test]
fn the_per_image_cap_refuses_with_the_byte_size_named() {
    let (_dir, roots) = root();
    let caps = ImageCaps {
        max_bytes_per_image: 4,
        ..ImageCaps::default()
    };
    let error = run(&roots, &caps, &args(json!({"items": ["img/faults.png"]})))
        .expect_err("over the per-image cap");
    assert!(
        error.contains("9 bytes exceeds the per-image cap of 4 bytes"),
        "the size and the cap are both named: {error}"
    );
}

#[test]
fn max_bytes_lowers_the_per_image_cap_and_can_never_raise_it() {
    let (_dir, roots) = root();

    let lowered = run(
        &roots,
        &ImageCaps::default(),
        &args(json!({"items": ["img/faults.png"], "max_bytes": 4})),
    )
    .expect_err("the call asked for a smaller ceiling than the file");
    assert!(lowered.contains("per-image cap of 4 bytes"), "{lowered}");

    // The operator's cap is a ceiling the agent argues under, never over.
    let caps = ImageCaps {
        max_bytes_per_image: 4,
        ..ImageCaps::default()
    };
    let raised = run(
        &roots,
        &caps,
        &args(json!({"items": ["img/faults.png"], "max_bytes": 1024 * 1024})),
    )
    .expect_err("`max_bytes` cannot raise the configured cap");
    assert!(
        raised.contains("per-image cap of 4 bytes"),
        "the configured cap still decides: {raised}"
    );
}

#[test]
fn the_total_cap_stops_the_call_once_the_budget_is_spent() {
    let (dir, roots) = root();
    std::fs::write(dir.path().join("img/second.png"), PNG).expect("png");
    let caps = ImageCaps {
        max_total_bytes: 12,
        ..ImageCaps::default()
    };
    let outcomes = run(
        &roots,
        &caps,
        &args(json!({"items": ["img/faults.png", "img/second.png"]})),
    )
    .expect("the first image fits");
    assert!(matches!(outcomes[0], Outcome::Delivered { .. }));
    assert!(
        refusal(&outcomes, "img/second.png")
            .contains("9 bytes would take this call past the 12-byte total cap"),
        "the refusal names the size and the cap: {outcomes:?}"
    );
}

#[test]
fn the_item_cap_refuses_the_extras_and_delivers_the_rest() {
    let (dir, roots) = root();
    for n in 0..3 {
        std::fs::write(dir.path().join(format!("img/n{n}.png")), PNG).expect("png");
    }
    let caps = ImageCaps {
        max_items: 2,
        ..ImageCaps::default()
    };
    let outcomes = run(
        &roots,
        &caps,
        &args(json!({"items": ["img/n0.png", "img/n1.png", "img/n2.png"]})),
    )
    .expect("the first two fit");
    assert!(matches!(outcomes[0], Outcome::Delivered { .. }));
    assert!(matches!(outcomes[1], Outcome::Delivered { .. }));
    assert!(
        refusal(&outcomes, "img/n2.png").contains("over the per-call limit of 2 images"),
        "{outcomes:?}"
    );
}

#[test]
fn the_caps_block_parses_its_three_keys_and_refuses_anything_else() {
    assert_eq!(
        ImageCaps::from_manifest_value(None).expect("absent block"),
        ImageCaps {
            max_items: 4,
            max_bytes_per_image: 4 * 1024 * 1024,
            max_total_bytes: 12 * 1024 * 1024,
        },
        "the documented defaults"
    );

    let configured = ImageCaps::from_manifest_value(Some(&json!({
        "max_items": 2,
        "max_bytes_per_image": 1024,
        "max_total_bytes": 2048,
    })))
    .expect("a full block");
    assert_eq!(
        configured,
        ImageCaps {
            max_items: 2,
            max_bytes_per_image: 1024,
            max_total_bytes: 2048
        }
    );

    // A partial block keeps the defaults for the keys it omits.
    let partial = ImageCaps::from_manifest_value(Some(&json!({"max_items": 1}))).expect("partial");
    assert_eq!(partial.max_bytes_per_image, 4 * 1024 * 1024);

    for bad in [
        json!([]),
        json!({"max_items": 0}),
        json!({"max_items": "four"}),
        json!({"max_item": 4}),
    ] {
        assert!(
            ImageCaps::from_manifest_value(Some(&bad)).is_err(),
            "a malformed caps block must fail the boot, not fall back to a cap the \
             operator did not choose: {bad}"
        );
    }
}

#[test]
fn a_server_with_no_source_root_registers_the_route_disabled_with_a_reason() {
    let mut server = bare_server();
    let reason = register(&mut server, None, ImageCaps::default()).expect("registration");
    assert_eq!(
        reason,
        Some(NO_ROOT_REASON),
        "the route is disabled and the caller is handed the reason to log"
    );
    let router = server.tool_router_mut();
    assert!(
        router.map.contains_key("fetch_images"),
        "registered, not skipped: `apply_bundled_tool_overrides` hard-fails on an absent name"
    );
    assert!(router.is_disabled("fetch_images"), "and disabled");

    // A bound root leaves it enabled.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_string_lossy().into_owned();
    let provider: mcp_methods::server::source::SourceRootsProvider =
        Arc::new(move || vec![root.clone()]);
    let mut live = bare_server();
    assert_eq!(
        register(&mut live, Some(provider), ImageCaps::default()).expect("registration"),
        None
    );
    assert!(!live.tool_router_mut().is_disabled("fetch_images"));
}

/// The smallest `McpServer` the registration path accepts.
fn bare_server() -> McpServer {
    McpServer::new(mcp_methods::server::ServerOptions::default())
}
