//! `fetch_images` — the one route that returns bytes instead of text.
//!
//! Query results keep returning image *paths and ids* (`Image` nodes carry
//! both, and under the vault model they are the same string). An agent that
//! decides it needs to look at one asks for it here, so the token cost of an
//! image is always an explicit choice rather than a side effect of a `MATCH`.
//!
//! Three things make that safe enough to register by default:
//!
//! 1. **The same sandbox the source tools use.** Resolution goes through
//!    mcp-methods' [`resolve_under_roots`], so a path is canonicalised and
//!    required to land under a bound source root — symlink escapes included.
//!    Absolute paths and `..` climbs are refused *before* that call so the
//!    error names the contract rather than reporting a miss.
//! 2. **Caps, never truncation.** An over-cap image is refused with its byte
//!    count named. Resizing would need an image decoder in the server, and a
//!    silently shrunk image is a wrong answer wearing a helpful face.
//! 3. **A four-type allowlist.** png, jpeg, gif and webp — what the Claude API
//!    accepts as image input. Everything else, SVG included, is refused with
//!    its type named.
//!
//! The result carries one [`ContentBlock::image`] per delivered file plus one
//! text block listing what was delivered and what was refused, so a partial
//! success is still a success the agent can act on.

use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::Engine as _;
use mcp_methods::server::source::{resolve_under_roots, SourceRootsProvider};
use mcp_methods::server::McpServer;
use rmcp::handler::server::router::tool::ToolRoute;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{CallToolResponse, CallToolResult, ContentBlock, Tool};
use rmcp::ErrorData as McpError;
use serde_json::{json, Map, Value};

type DynFut<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Why the route is registered-but-disabled on a server with no source root.
///
/// Registered rather than skipped because [`crate::bundled_overrides`]
/// hard-fails the boot on a manifest override naming a route that is absent
/// from `router.map` — `disable_route` keeps the entry resolvable while
/// rejecting every call. The string is the operator-facing half of that
/// decision and is logged at boot.
pub(crate) const NO_ROOT_REASON: &str =
    "no source root: images are served from the vault root or `source_root`";

/// The four types delivered as image blocks, by lowercased extension.
///
/// Not [`kglite::okf::model::mime_for_extension`]: that one is `pub(crate)` to
/// the engine, and widening the engine's public API to share a five-line table
/// would move the Rust API baseline for no caller outside this file.
const DELIVERABLE: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

/// Types common enough in a vault that a refusal should name them rather than
/// say "unknown". Refusal text only — nothing here is ever delivered.
const NAMED_REFUSALS: &[(&str, &str)] = &[
    ("svg", "image/svg+xml"),
    ("pdf", "application/pdf"),
    ("bmp", "image/bmp"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("avif", "image/avif"),
    ("heic", "image/heic"),
    ("ico", "image/vnd.microsoft.icon"),
];

/// Per-call ceilings, from `extensions.fetch_images` or the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImageCaps {
    pub max_items: usize,
    pub max_bytes_per_image: u64,
    pub max_total_bytes: u64,
}

impl Default for ImageCaps {
    fn default() -> Self {
        Self {
            max_items: 4,
            max_bytes_per_image: 4 * 1024 * 1024,
            max_total_bytes: 12 * 1024 * 1024,
        }
    }
}

impl ImageCaps {
    /// `extensions.fetch_images: { max_items, max_bytes_per_image, max_total_bytes }`.
    ///
    /// A malformed block is a boot error rather than a dropped key, for the
    /// reason every other `extensions.` reader in this crate fails that way:
    /// the silent direction of a typo is a cap the operator thinks they set.
    pub(crate) fn from_manifest_value(raw: Option<&Value>) -> Result<Self> {
        let Some(raw) = raw else {
            return Ok(Self::default());
        };
        let map = raw
            .as_object()
            .context("extensions.fetch_images must be a mapping of cap names to integers")?;
        let mut caps = Self::default();
        for (key, value) in map {
            let n = value.as_u64().filter(|n| *n > 0).with_context(|| {
                format!("extensions.fetch_images.{key} must be a positive integer; found {value}")
            })?;
            match key.as_str() {
                "max_items" => caps.max_items = n as usize,
                "max_bytes_per_image" => caps.max_bytes_per_image = n,
                "max_total_bytes" => caps.max_total_bytes = n,
                other => anyhow::bail!(
                    "extensions.fetch_images: unknown key `{other}` \
                     (max_items, max_bytes_per_image, max_total_bytes)"
                ),
            }
        }
        Ok(caps)
    }
}

/// One requested item's fate, in request order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Delivered {
        path: String,
        mime: &'static str,
        bytes: Vec<u8>,
    },
    Refused {
        path: String,
        reason: String,
    },
}

impl Outcome {
    fn refused(path: &str, reason: impl Into<String>) -> Self {
        Outcome::Refused {
            path: path.to_string(),
            reason: reason.into(),
        }
    }
}

/// Register the route, disabling it when this boot has no source root.
///
/// Returns the disable reason when it disabled the route, so the caller can
/// log it and a test can assert the pair without a live router.
pub(crate) fn register(
    server: &mut McpServer,
    source_roots: Option<SourceRootsProvider>,
    caps: ImageCaps,
) -> Result<Option<&'static str>> {
    let schema: Map<String, Value> = json!({
        "type": "object",
        "properties": {
            "items": {
                "type": "array",
                "minItems": 1,
                "items": {"type": "string"},
                "description": "Vault-relative image paths or `Image` node ids \
                                (identical under the vault model), e.g. \
                                'img/faults.png'. Absolute paths and `..` \
                                segments are refused."
            },
            "max_bytes": {
                "type": ["integer", "null"],
                "minimum": 1,
                "description": "Lower the per-image byte cap for this call. \
                                Cannot raise the server's configured cap."
            }
        },
        "required": ["items"]
    })
    .as_object()
    .cloned()
    .ok_or_else(|| anyhow::anyhow!("schema construction failed"))?;

    let attr = Tool::new_with_raw(
        "fetch_images",
        Some(std::borrow::Cow::Owned(format!(
            "Fetch image files as image content blocks. Takes vault-relative \
             paths or `Image` node ids — query `Image` nodes with \
             `cypher_query` first and fetch only the ones you need. At most \
             {} per call, {} MiB each, {} MiB total; png, jpeg, gif and webp \
             only. Over-cap or non-image items are refused with the reason \
             named, never resized or truncated.",
            caps.max_items,
            caps.max_bytes_per_image / (1024 * 1024),
            caps.max_total_bytes / (1024 * 1024),
        ))),
        Arc::new(schema),
    );

    let roots_provider = source_roots;
    let handler_roots = roots_provider.clone();
    server.tool_router_mut().add_route(ToolRoute::new_dyn(
        attr,
        move |ctx: ToolCallContext<'_, McpServer>| -> DynFut<'_, Result<CallToolResponse, McpError>> {
            let roots_provider = handler_roots.clone();
            let arguments = ctx.arguments.clone();
            Box::pin(async move {
                let args: Map<String, Value> = arguments.unwrap_or_default();
                let roots: Vec<String> = roots_provider.map(|p| p()).unwrap_or_default();
                Ok(match run(&roots, &caps, &args) {
                    Ok(outcomes) => result_of(&outcomes),
                    Err(message) => {
                        CallToolResult::error(vec![ContentBlock::text(message)])
                    }
                }
                .into())
            })
        },
    ));

    // A boot-time snapshot, like `explore` / `read_code_source`'s gate: a tool
    // handler cannot reach the router, so nothing could re-enable the route
    // later anyway, and every mode that has roots at all binds them before
    // registration.
    let has_root = roots_provider.map(|p| !p().is_empty()).unwrap_or(false);
    if has_root {
        return Ok(None);
    }
    server.tool_router_mut().disable_route("fetch_images");
    Ok(Some(NO_ROOT_REASON))
}

/// The whole tool, minus the protocol. `Err` is a malformed call (or one whose
/// every item was refused); `Ok` is a success with per-item outcomes.
pub(crate) fn run(
    roots: &[String],
    caps: &ImageCaps,
    args: &Map<String, Value>,
) -> Result<Vec<Outcome>, String> {
    let items = args
        .get("items")
        .and_then(Value::as_array)
        .ok_or("fetch_images: `items` must be a list of vault-relative paths or `Image` ids")?;
    if items.is_empty() {
        return Err("fetch_images: `items` must name at least one image".to_string());
    }
    let mut requested = Vec::with_capacity(items.len());
    for item in items {
        let path = item.as_str().ok_or_else(|| {
            format!("fetch_images: `items` entries must be strings; found {item}")
        })?;
        requested.push(path);
    }

    // `max_bytes` narrows, never widens: an agent cannot argue its way past
    // the operator's cap, only below it.
    let per_image = match args.get("max_bytes") {
        None | Some(Value::Null) => caps.max_bytes_per_image,
        Some(value) => {
            let n = value.as_u64().filter(|n| *n > 0).ok_or_else(|| {
                format!("fetch_images: `max_bytes` must be a positive integer; found {value}")
            })?;
            n.min(caps.max_bytes_per_image)
        }
    };

    let mut outcomes = Vec::with_capacity(requested.len());
    let mut total: u64 = 0;
    for (index, path) in requested.iter().enumerate() {
        if index >= caps.max_items {
            outcomes.push(Outcome::refused(
                path,
                format!(
                    "over the per-call limit of {} images; request it in another call",
                    caps.max_items
                ),
            ));
            continue;
        }
        outcomes.push(fetch_one(
            roots,
            path,
            per_image,
            caps.max_total_bytes,
            &mut total,
        ));
    }

    if outcomes
        .iter()
        .all(|o| matches!(o, Outcome::Refused { .. }))
    {
        return Err(format!(
            "fetch_images: no image was delivered.\n{}",
            render_refusals(&outcomes)
        ));
    }
    Ok(outcomes)
}

fn fetch_one(
    roots: &[String],
    path: &str,
    per_image: u64,
    max_total: u64,
    total: &mut u64,
) -> Outcome {
    if let Some(reason) = addressing_refusal(path) {
        return Outcome::refused(path, reason);
    }
    let ext = extension_of(path);
    let Some(mime) = deliverable_mime(&ext) else {
        return Outcome::refused(path, non_image_reason(&ext));
    };
    let Some(resolved) = resolve_under_roots(path, roots) else {
        return Outcome::refused(
            path,
            "not found under the server's source root (paths are vault-relative)",
        );
    };
    let size = match std::fs::metadata(&resolved) {
        Ok(meta) => meta.len(),
        Err(e) => return Outcome::refused(path, format!("cannot be read: {e}")),
    };
    if size > per_image {
        return Outcome::refused(
            path,
            format!("{size} bytes exceeds the per-image cap of {per_image} bytes"),
        );
    }
    if *total + size > max_total {
        return Outcome::refused(
            path,
            format!(
                "{size} bytes would take this call past the {max_total}-byte total cap \
                 ({} bytes already delivered)",
                *total
            ),
        );
    }
    let bytes = match std::fs::read(&resolved) {
        Ok(bytes) => bytes,
        Err(e) => return Outcome::refused(path, format!("cannot be read: {e}")),
    };
    *total += bytes.len() as u64;
    // J8: one line per delivered image. This route moves more bytes per call
    // than any other, and the operator's only view of that is stderr.
    tracing::info!(path, bytes = bytes.len(), "fetch_images delivered");
    Outcome::Delivered {
        path: path.to_string(),
        mime,
        bytes,
    }
}

/// Addressing rules checked *before* the sandbox, so the refusal names the
/// contract instead of reporting a resolution miss the agent cannot act on.
fn addressing_refusal(path: &str) -> Option<String> {
    const CONTRACT: &str = "`fetch_images` takes vault-relative paths or `Image` node ids";
    if path.trim().is_empty() {
        return Some(format!("empty path: {CONTRACT}"));
    }
    if path.starts_with("file:") {
        return Some(format!("`file:` URLs are refused: {CONTRACT}"));
    }
    let bytes = path.as_bytes();
    let windows_drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\');
    if path.starts_with('/') || path.starts_with('\\') || path.starts_with('~') || windows_drive {
        return Some(format!("absolute paths are refused: {CONTRACT}"));
    }
    if path.split(['/', '\\']).any(|segment| segment == "..") {
        return Some(format!("`..` path segments are refused: {CONTRACT}"));
    }
    None
}

fn extension_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn deliverable_mime(ext: &str) -> Option<&'static str> {
    DELIVERABLE
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, mime)| *mime)
}

fn non_image_reason(ext: &str) -> String {
    const DELIVERED: &str = "only png, jpeg, gif and webp are delivered";
    match NAMED_REFUSALS.iter().find(|(e, _)| *e == ext) {
        Some((_, mime)) => format!("`{mime}` is not delivered: {DELIVERED}"),
        None if ext.is_empty() => {
            format!("no file extension, so the type is unknown: {DELIVERED}")
        }
        None => format!("`.{ext}` is not a delivered image type: {DELIVERED}"),
    }
}

/// One image block per delivered item, then one text block covering both
/// lists — a partial success is still a success.
fn result_of(outcomes: &[Outcome]) -> CallToolResult {
    let mut blocks: Vec<ContentBlock> = outcomes
        .iter()
        .filter_map(|o| match o {
            Outcome::Delivered { mime, bytes, .. } => Some(ContentBlock::image(
                base64::engine::general_purpose::STANDARD.encode(bytes),
                *mime,
            )),
            Outcome::Refused { .. } => None,
        })
        .collect();
    blocks.push(ContentBlock::text(render_summary(outcomes)));
    CallToolResult::success(blocks)
}

fn render_summary(outcomes: &[Outcome]) -> String {
    let delivered = outcomes
        .iter()
        .filter(|o| matches!(o, Outcome::Delivered { .. }))
        .count();
    let refused = outcomes.len() - delivered;
    let mut out = format!("fetch_images: {delivered} delivered, {refused} refused.\n");
    for outcome in outcomes {
        match outcome {
            Outcome::Delivered { path, mime, bytes } => {
                out.push_str(&format!(
                    "- delivered `{path}` ({mime}, {} bytes)\n",
                    bytes.len()
                ));
            }
            Outcome::Refused { path, reason } => {
                out.push_str(&format!("- refused `{path}`: {reason}\n"));
            }
        }
    }
    out
}

fn render_refusals(outcomes: &[Outcome]) -> String {
    outcomes
        .iter()
        .filter_map(|o| match o {
            Outcome::Refused { path, reason } => Some(format!("- refused `{path}`: {reason}")),
            Outcome::Delivered { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "fetch_images_tests.rs"]
mod fetch_images_tests;
