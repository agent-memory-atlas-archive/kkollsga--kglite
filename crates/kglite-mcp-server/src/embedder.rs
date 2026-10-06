//! Embedding-backend selection from `extensions.embedder`, and the
//! wheel-supplied Python factory type the standalone binary lacks.

use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use mcp_methods::server::Manifest;

/// Builds a graph embedder from the manifest's `extensions.embedder` config
/// (passed as a JSON string), on demand.
///
/// This is the seam that lets the **pip-hosted** server use *any* Python
/// embedding library (`extensions.embedder.library: sentence-transformers`,
/// `fastembed`, or a `factory:` escape) without the libpython-free library
/// knowing anything about Python: the kglite-py wrapper hands the config JSON
/// to a Python factory (`kglite._mcp_embed`) which picks the library, builds
/// the model, and wraps it in a `PyEmbedderAdapter` (GIL re-acquired only for
/// the embed call). The standalone cargo binary passes no factory, so a Python
/// library errors there with a clear message; it uses `library: fastembed-rs`
/// (the Rust `FastEmbedAdapter`) instead.
///
/// The argument is the whole `extensions.embedder` JSON object, so new fields
/// (library / model / factory / kwargs / …) flow through to Python without any
/// Rust change. `Send` because `run_with_embedder_factory` may move it into the
/// tokio runtime's future.
pub type PyEmbedderFactory =
    Box<dyn Fn(&str) -> Result<Arc<dyn kglite::api::Embedder>, String> + Send>;

/// The factory as the lazy path holds it: shared with the embedder that calls
/// it on first use, and behind a `Mutex` because the factory is `Send` but not
/// `Sync`.
pub(crate) type SharedEmbedderFactory = Arc<Mutex<PyEmbedderFactory>>;

pub(crate) fn share_factory(factory: Option<PyEmbedderFactory>) -> Option<SharedEmbedderFactory> {
    factory.map(|f| Arc::new(Mutex::new(f)))
}

/// When `extensions.embedder` builds its model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadMode {
    /// On the first `embed()` / `dimension()` / `model_id()` / `load()` call.
    Lazy,
    /// At boot.
    Eager,
}

fn parse_load_mode(obj: &serde_json::Map<String, serde_json::Value>) -> Result<LoadMode> {
    match obj.get("load") {
        None => Ok(LoadMode::Lazy),
        Some(v) => match v.as_str() {
            Some("lazy") => Ok(LoadMode::Lazy),
            Some("eager") => Ok(LoadMode::Eager),
            _ => anyhow::bail!("extensions.embedder.load must be \"lazy\" or \"eager\" (got: {v})"),
        },
    }
}

type Builder = Box<dyn Fn() -> Result<Arc<dyn kglite::api::Embedder>, String> + Send>;

/// An embedder whose model is built by the first call that needs it.
///
/// `builder` is the only owner of the deferred construction; holding its lock
/// across the build is what makes concurrent first calls build once. A failed
/// build is not cached: the next call runs the builder again.
struct LazyEmbedder {
    label: String,
    builder: Mutex<Builder>,
    inner: OnceLock<Arc<dyn kglite::api::Embedder>>,
}

impl LazyEmbedder {
    fn new(label: String, builder: Builder) -> Self {
        Self {
            label,
            builder: Mutex::new(builder),
            inner: OnceLock::new(),
        }
    }

    fn get(&self) -> Result<&Arc<dyn kglite::api::Embedder>, String> {
        if let Some(built) = self.inner.get() {
            return Ok(built);
        }
        let builder = self.builder.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(built) = self.inner.get() {
            return Ok(built);
        }
        let started = std::time::Instant::now();
        match builder() {
            Ok(built) => {
                tracing::info!(
                    embedder = %self.label,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "built embedder on first use"
                );
                Ok(self.inner.get_or_init(|| built))
            }
            Err(e) => {
                let msg = format!("embedder construction failed ({}): {e}", self.label);
                tracing::error!("{msg}");
                Err(msg)
            }
        }
    }
}

impl kglite::api::Embedder for LazyEmbedder {
    fn dimension(&self) -> usize {
        // The trait cannot report failure here. `load()` runs first on every
        // path that sizes a store from this value and carries the error.
        self.get().map(|e| e.dimension()).unwrap_or(0)
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.get()?.embed(texts)
    }

    fn model_id(&self) -> Option<String> {
        self.get().ok().and_then(|e| e.model_id())
    }

    fn load(&self) -> Result<(), String> {
        self.get()?.load()
    }

    fn unload(&self) {
        if let Some(built) = self.inner.get() {
            built.unload();
        }
    }
}

/// Read `manifest.extensions.embedder.{library, model, …}` and produce the
/// corresponding [`kglite::api::Embedder`]. Returns `Ok(None)` when no
/// `embedder:` is declared, `Err` on validation failures.
///
/// The `library` field names the embedding engine; the host (Rust vs Python)
/// is inferred from it:
/// - `fastembed-rs` — the Rust-native fastembed-rs adapter (cargo
///   `--features fastembed`; the only option on the standalone binary).
/// - any other value, or a `factory:` escape — a Python embedding library
///   (`fastembed`, `sentence-transformers`, …) built by `py_embedder_factory`
///   (supplied only by the pip-hosted server). The whole config object is
///   handed to Python as JSON, `load` included (`kglite._mcp_embed` ignores
///   keys it does not know), so the library set + its options live entirely
///   on the Python side — adding a library never touches this function.
///
/// `load: lazy` (the default) validates everything here but defers building
/// the model to the first call that needs it; `load: eager` builds it now.
pub(crate) fn build_embedder_from_manifest(
    manifest: &Manifest,
    py_embedder_factory: Option<&SharedEmbedderFactory>,
) -> Result<Option<Arc<dyn kglite::api::Embedder>>> {
    let Some(raw) = manifest.extensions.get("embedder") else {
        return Ok(None);
    };
    if !manifest.trust.allow_embedder {
        anyhow::bail!(
            "extensions.embedder is disabled unless the manifest explicitly sets \
             trust.allow_embedder: true"
        );
    }
    let obj = raw
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("extensions.embedder must be a mapping (got: {raw:?})"))?;
    let mode = parse_load_mode(obj)?;
    // `fastembed-rs` is the only Rust-hosted engine; everything else (and any
    // `factory:`) is a Python library hosted by the wheel. Default to a Python
    // library so the common pip case needs only `library: + model:`.
    let library = obj.get("library").and_then(|v| v.as_str());
    let is_rust = library == Some("fastembed-rs");

    let (label, builder): (String, Builder) = if is_rust {
        let model = obj.get("model").and_then(|v| v.as_str()).ok_or_else(|| {
            anyhow::anyhow!("extensions.embedder.model is required for library: fastembed-rs")
        })?;
        require_fastembed_feature()?;
        let model = model.to_string();
        (
            format!("fastembed-rs {model}"),
            Box::new(move || build_rust_embedder(&model)),
        )
    } else {
        // Python-hosted: hand the whole config object to the Python factory,
        // which picks the library, builds the model, and wraps it. The cargo
        // binary supplies no factory.
        let factory = py_embedder_factory.ok_or_else(|| {
            let lib = library.unwrap_or("<a Python library>");
            anyhow::anyhow!(
                "extensions.embedder.library = {lib:?} is a Python embedding library, but the \
                 standalone `cargo install kglite-mcp-server` binary has no Python interpreter to \
                 host it. Either run the server from the kglite wheel (`pip install kglite`, then \
                 `pip install {lib}`), or use `library: fastembed-rs` with `cargo install \
                 kglite-mcp-server --features fastembed`."
            )
        })?;
        let config_json = serde_json::to_string(raw)
            .map_err(|e| anyhow::anyhow!("serializing extensions.embedder failed: {e}"))?;
        let factory = Arc::clone(factory);
        (
            format!("python {}", library.unwrap_or("factory")),
            Box::new(move || {
                let factory = factory.lock().unwrap_or_else(|e| e.into_inner());
                factory(&config_json)
                    .map_err(|e| format!("python embedder construction failed: {e}"))
            }),
        )
    };

    let lazy = Arc::new(LazyEmbedder::new(label.clone(), builder));
    match mode {
        LoadMode::Lazy => {
            tracing::info!(embedder = %label, "registered embedder (loads on first use)");
        }
        LoadMode::Eager => {
            lazy.get().map_err(|e| anyhow::anyhow!("{e}"))?;
            tracing::info!(embedder = %label, "registered embedder (loaded at boot)");
        }
    }
    Ok(Some(lazy))
}

/// Fail the boot, not the first query, when the Rust engine is not compiled in.
fn require_fastembed_feature() -> Result<()> {
    if cfg!(feature = "fastembed") {
        return Ok(());
    }
    anyhow::bail!(
        "extensions.embedder.library = \"fastembed-rs\" requires this binary to be built with \
         the `fastembed` feature: `cargo install kglite-mcp-server --features fastembed`. The \
         default build excludes it because its ort-sys dependency has a flaky upstream binary \
         download. (If you are running the pip wheel, use a Python library instead — e.g. \
         `library: sentence-transformers` with `pip install sentence-transformers`.)"
    )
}

/// Build the Rust-native fastembed-rs embedder (`library: fastembed-rs`).
#[cfg(feature = "fastembed")]
fn build_rust_embedder(model: &str) -> Result<Arc<dyn kglite::api::Embedder>, String> {
    let adapter = kglite::api::FastEmbedAdapter::new(model)
        .map_err(|e| format!("fastembed-rs init failed: {e}"))?;
    Ok(Arc::new(adapter))
}

#[cfg(not(feature = "fastembed"))]
fn build_rust_embedder(_model: &str) -> Result<Arc<dyn kglite::api::Embedder>, String> {
    Err("fastembed-rs is not compiled into this binary".to_string())
}

#[cfg(test)]
mod embedder_tests {
    use super::*;

    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mcp_methods::server::Manifest;

    struct Stub;
    impl kglite::api::Embedder for Stub {
        fn dimension(&self) -> usize {
            7
        }
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            Ok(texts.iter().map(|_| vec![0.0; 7]).collect())
        }
        fn model_id(&self) -> Option<String> {
            Some("stub/model".into())
        }
    }

    fn manifest_with(allow_embedder: bool, extra: &str) -> (tempfile::TempDir, Manifest) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mcp.yaml");
        fs::write(
            &path,
            format!(
                "name: embedder-test\ntrust:\n  allow_embedder: {allow_embedder}\n\
                 extensions:\n  embedder:\n    library: test\n    model: test\n{extra}"
            ),
        )
        .expect("write manifest");
        let manifest = mcp_methods::server::load_manifest(&path).expect("load manifest");
        (dir, manifest)
    }

    /// A factory that counts invocations and records the config it was given.
    fn counting_factory(
        fail_first: usize,
    ) -> (SharedEmbedderFactory, Arc<AtomicUsize>, Arc<Mutex<String>>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(String::new()));
        let (c, s) = (calls.clone(), seen.clone());
        let factory: PyEmbedderFactory = Box::new(move |cfg| {
            let n = c.fetch_add(1, Ordering::SeqCst);
            *s.lock().unwrap() = cfg.to_string();
            if n < fail_first {
                return Err("factory sentinel".to_string());
            }
            // Widen the race window so concurrent first calls overlap.
            std::thread::sleep(std::time::Duration::from_millis(30));
            Ok(Arc::new(Stub) as Arc<dyn kglite::api::Embedder>)
        });
        (share_factory(Some(factory)).unwrap(), calls, seen)
    }

    fn build(
        extra: &str,
        factory: &SharedEmbedderFactory,
    ) -> Result<Option<Arc<dyn kglite::api::Embedder>>> {
        let (_dir, manifest) = manifest_with(true, extra);
        build_embedder_from_manifest(&manifest, Some(factory))
    }

    #[test]
    fn untrusted_embedder_never_invokes_factory() {
        let (_dir, manifest) = manifest_with(false, "    load: eager\n");
        let (factory, calls, _) = counting_factory(0);

        let error = match build_embedder_from_manifest(&manifest, Some(&factory)) {
            Ok(_) => panic!("untrusted embedder must be rejected"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("trust.allow_embedder: true"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn trusted_eager_embedder_reaches_factory_at_boot() {
        let (factory, calls, seen) = counting_factory(1);
        let error = match build("    load: eager\n", &factory) {
            Ok(_) => panic!("sentinel factory must fail the boot"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("factory sentinel"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(seen.lock().unwrap().contains("\"library\":\"test\""));
    }

    #[test]
    fn lazy_is_the_default_and_defers_the_factory() {
        let (factory, calls, _) = counting_factory(0);
        let e = build("", &factory).unwrap().expect("embedder declared");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "boot must not build");
        assert_eq!(e.dimension(), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(e.model_id().as_deref(), Some("stub/model"));
        assert_eq!(e.embed(&["x".into()]).unwrap().len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "built exactly once");
    }

    #[test]
    fn explicit_lazy_defers_and_every_entry_point_builds() {
        for call in 0..4 {
            let (factory, calls, _) = counting_factory(0);
            let e = build("    load: lazy\n", &factory).unwrap().unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            match call {
                0 => drop(e.embed(&["x".into()])),
                1 => drop(e.dimension()),
                2 => drop(e.model_id()),
                _ => e.load().unwrap(),
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1, "entry point {call}");
        }
    }

    #[test]
    fn eager_builds_at_boot_and_only_once() {
        let (factory, calls, _) = counting_factory(0);
        let e = build("    load: eager\n", &factory).unwrap().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(e.dimension(), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn invalid_load_value_fails_boot_without_building() {
        let (factory, calls, _) = counting_factory(0);
        for bad in ["    load: sometimes\n", "    load: 3\n", "    load: true\n"] {
            let error = match build(bad, &factory) {
                Ok(_) => panic!("{bad:?} must fail the boot"),
                Err(error) => error,
            };
            assert!(error
                .to_string()
                .contains("extensions.embedder.load must be"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn lazy_validation_still_fails_at_boot() {
        // No factory (standalone binary) and a Python library: boot error,
        // not a deferred first-query error.
        let (_dir, manifest) = manifest_with(true, "");
        let error = match build_embedder_from_manifest(&manifest, None) {
            Ok(_) => panic!("a Python library with no factory must fail the boot"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("standalone"));
    }

    #[test]
    fn concurrent_first_calls_build_once() {
        let (factory, calls, _) = counting_factory(0);
        let e = build("", &factory).unwrap().unwrap();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let e = Arc::clone(&e);
                std::thread::spawn(move || e.embed(&["x".into()]).unwrap().len())
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), 1);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_first_build_is_reported_and_retried() {
        let (factory, calls, _) = counting_factory(1);
        let e = build("", &factory).unwrap().unwrap();
        let err = e.embed(&["x".into()]).unwrap_err();
        assert!(err.contains("factory sentinel"), "got {err:?}");
        assert!(e.load().is_ok(), "second call retries and succeeds");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(e.embed(&["x".into()]).is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 2, "success is cached");
    }

    #[test]
    fn unload_before_first_use_does_not_build() {
        let (factory, calls, _) = counting_factory(0);
        let e = build("", &factory).unwrap().unwrap();
        e.unload();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[cfg(not(feature = "fastembed"))]
    #[test]
    fn fastembed_rs_without_the_feature_fails_boot_even_when_lazy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.yaml");
        fs::write(
            &path,
            "name: t\ntrust:\n  allow_embedder: true\nextensions:\n  embedder:\n    \
             library: fastembed-rs\n    model: BAAI/bge-small-en-v1.5\n",
        )
        .unwrap();
        let manifest = mcp_methods::server::load_manifest(&path).unwrap();
        let error = match build_embedder_from_manifest(&manifest, None) {
            Ok(_) => panic!("must fail the boot"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("`fastembed` feature"));
    }
}
