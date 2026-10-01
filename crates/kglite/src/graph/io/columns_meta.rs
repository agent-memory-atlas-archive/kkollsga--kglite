//! The `columns_meta` sidecar of a disk graph's `columns.bin`: its envelope,
//! its two encodings, and where a reader looks for it.
//!
//! Until 0.19.0 the sidecar was a bare array of [`ColumnTypeMeta`] (JSON, or
//! postcard in `columns_meta.bin.zst`). A reader that meets a column type it
//! does not know maps it to a string column and later panics inside a fixed-width
//! read, so any change to the layout `columns.bin` describes has to be refused
//! *before* a column is mapped. The envelope does that: an older reader cannot
//! deserialise an object where it expects an array, and fails with an ordinary
//! error rather than a panic.
//!
//! ```text
//! columns_meta.json          {"format": 2, "types": [ColumnTypeMeta, ...],
//!                             "files": {"<type name>": "type_columns/<hex>.bin", ...},
//!                             "sidecars": {"<type name>": "columns/<hex>", ...}}
//! columns_meta.v2.bin.zst    zstd(postcard-framed {format, types, files, sidecars})
//! ```
//!
//! **Where a type's bytes live.** A generation written by a save keeps one
//! immutable file per node type under `type_columns/`, named in `files` and
//! addressed relative to the directory holding the sidecar; the regions in that
//! type's [`ColumnTypeMeta`] are offsets into *its* file. A type with no `files`
//! entry keeps its regions in the shared `columns.bin` beside the sidecar, which
//! is how the RDF builder and every 0.19.0 directory are laid out. The names are
//! derived from the interned type key, never the type name, so a user-chosen
//! name cannot reach the filesystem, and they are recorded here rather than
//! recomputed so a reader trusts the sidecar alone.
//!
//! **Where a type's zstd sidecar lives.** A type the column files cannot hold
//! (a `Mixed` column) is written to `columns/<hex>/columns.zst` under the
//! generation root, and `sidecars` maps its name to that directory. Until 0.19.0
//! the directory was named by the raw type name, which let a type called
//! `../../x` write outside the generation; those directories are still read,
//! by the name they carry, because no `sidecars` entry covers them.
//!
//! The binary form moved to a new file name for the same reason the JSON form
//! changed shape: a reader that prefers `columns_meta.bin.zst` when it exists
//! would otherwise decode the new bytes as the old bare array. Readers here
//! still accept both legacy spellings, so a directory 0.19.0 wrote opens.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::graph::io::file::{decode_disk_serde, encode_disk_serde, io_context};
use crate::graph::io::ntriples::ColumnTypeMeta;

/// The `format` this build writes and the newest it reads.
///
/// Equal to the disk layout's `disk_format` today (both were introduced
/// together); they are separate numbers because the envelope versions the
/// sidecar's schema, while `disk_format` versions everything else a directory
/// holds.
pub(crate) const COLUMNS_META_FORMAT: u32 = 2;

/// Written by 0.19.0 and earlier (`ntriples` builds only); read, never written.
const LEGACY_BIN: &str = "columns_meta.bin.zst";
const CURRENT_BIN: &str = "columns_meta.v2.bin.zst";
const JSON: &str = "columns_meta.json";

/// Directory (relative to the sidecar's) that holds the per-type column files.
pub(crate) const TYPE_FILES_DIR: &str = "type_columns";

/// Directory (relative to the generation root) that holds the zstd sidecars.
pub(crate) const SIDECAR_DIR: &str = "columns";

/// A parsed sidecar: every type's region layout, and where its bytes live.
#[derive(Default)]
pub(crate) struct ColumnsMeta {
    pub(crate) types: Vec<ColumnTypeMeta>,
    /// Type name -> file relative to the sidecar's directory. A type absent
    /// here keeps its regions in the shared `columns.bin`.
    pub(crate) files: BTreeMap<String, String>,
    /// Type name -> zstd sidecar directory, relative to the *generation root*
    /// (`columns/<hex>`). A `columns/` subdirectory no entry names is a 0.19.0
    /// layout whose directory name is the type name.
    pub(crate) sidecars: BTreeMap<String, String>,
}

impl ColumnsMeta {
    /// A sidecar whose every type lives in the shared `columns.bin`.
    pub(crate) fn shared(types: Vec<ColumnTypeMeta>) -> Self {
        Self {
            types,
            files: BTreeMap::new(),
            sidecars: BTreeMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: u32,
    types: Vec<ColumnTypeMeta>,
    #[serde(default)]
    files: BTreeMap<String, String>,
    #[serde(default)]
    sidecars: BTreeMap<String, String>,
}

/// Borrowing twin of [`Envelope`] so a writer does not clone every type's metadata.
#[derive(Serialize)]
struct EnvelopeRef<'a> {
    format: u32,
    types: &'a [ColumnTypeMeta],
    files: &'a BTreeMap<String, String>,
    sidecars: &'a BTreeMap<String, String>,
}

impl Envelope {
    fn into_meta(self, what: &str) -> io::Result<ColumnsMeta> {
        if self.format > COLUMNS_META_FORMAT {
            return Err(newer_format_error(self.format, what));
        }
        Ok(ColumnsMeta {
            types: self.types,
            files: self.files,
            sidecars: self.sidecars,
        })
    }
}

/// A path-safe name for `type_name` built from its interned key alone.
///
/// Stable across generations, independent of what else was written, and
/// collision-free through `used` in the (astronomically unlikely) event that two
/// names share a key.
fn keyed_name(
    type_name: &str,
    used: &mut HashSet<String>,
    spell: impl Fn(u64, Option<u32>) -> String,
) -> String {
    let key = crate::graph::schema::InternedKey::from_str(type_name).as_u64();
    let mut name = spell(key, None);
    let mut n = 1u32;
    while !used.insert(name.clone()) {
        name = spell(key, Some(n));
        n += 1;
    }
    name
}

/// The file name (relative to the sidecar's directory) a save gives `type_name`.
pub(crate) fn type_file_name(type_name: &str, used: &mut HashSet<String>) -> String {
    keyed_name(type_name, used, |key, n| match n {
        None => format!("{TYPE_FILES_DIR}/{key:016x}.bin"),
        Some(n) => format!("{TYPE_FILES_DIR}/{key:016x}-{n}.bin"),
    })
}

/// The zstd sidecar directory (relative to the generation root) a save gives
/// `type_name`; see [`type_file_name`] for why it is keyed and not named.
pub(crate) fn sidecar_dir_name(type_name: &str, used: &mut HashSet<String>) -> String {
    keyed_name(type_name, used, |key, n| match n {
        None => format!("{SIDECAR_DIR}/{key:016x}"),
        Some(n) => format!("{SIDECAR_DIR}/{key:016x}-{n}"),
    })
}

/// Resolve a `files` or `sidecars` entry against a directory, refusing anything
/// that is not a plain relative path (an absolute path or a `..` component in a
/// hostile or corrupt sidecar would otherwise escape the generation).
pub(crate) fn resolve_type_file(dir: &Path, relative: &str) -> io::Result<PathBuf> {
    let path = Path::new(relative);
    let plain = !relative.is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !plain {
        return Err(invalid_data(format!(
            "column metadata names a file outside its directory: '{relative}'"
        )));
    }
    Ok(dir.join(path))
}

/// Resolve a `sidecars` entry: a plain relative path that stays under
/// `columns/`, which is where the writer puts every sidecar.
pub(crate) fn resolve_sidecar_dir(dir: &Path, relative: &str) -> io::Result<PathBuf> {
    let path = resolve_type_file(dir, relative)?;
    if !Path::new(relative).starts_with(SIDECAR_DIR) || Path::new(relative).components().count() < 2
    {
        return Err(invalid_data(format!(
            "column metadata names a sidecar outside '{SIDECAR_DIR}/': '{relative}'"
        )));
    }
    Ok(path)
}

fn invalid_data(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn newer_format_error(found: u32, what: &str) -> io::Error {
    invalid_data(format!(
        "{what} uses columns metadata format {found}, but this library only supports up to \
         format {COLUMNS_META_FORMAT}. Please upgrade kglite."
    ))
}

fn envelope(meta: &ColumnsMeta) -> EnvelopeRef<'_> {
    EnvelopeRef {
        format: COLUMNS_META_FORMAT,
        types: &meta.types,
        files: &meta.files,
        sidecars: &meta.sidecars,
    }
}

/// Serialise `meta` as the JSON envelope.
pub(crate) fn to_json(meta: &ColumnsMeta, pretty: bool) -> io::Result<String> {
    let json = if pretty {
        serde_json::to_string_pretty(&envelope(meta))
    } else {
        serde_json::to_string(&envelope(meta))
    };
    json.map_err(io::Error::other)
}

/// Serialise `meta` as the framed, compressed binary envelope.
pub(crate) fn to_bin(meta: &ColumnsMeta) -> io::Result<Vec<u8>> {
    let bytes = encode_disk_serde(&envelope(meta))?;
    zstd::encode_all(bytes.as_slice(), 3)
}

/// Parse the JSON form: the envelope, or the bare array 0.19.0 and earlier wrote.
///
/// The first non-blank byte picks the shape, so neither reads the document
/// twice — this file is hundreds of megabytes on a Wikidata-scale graph.
pub(crate) fn parse_json(json: &str, what: &str) -> io::Result<ColumnsMeta> {
    let bad = |e: serde_json::Error| invalid_data(format!("{what} is not valid JSON: {e}"));
    if json.trim_start().starts_with('[') {
        return serde_json::from_str(json)
            .map(ColumnsMeta::shared)
            .map_err(bad);
    }
    serde_json::from_str::<Envelope>(json)
        .map_err(bad)?
        .into_meta(what)
}

fn parse_bin(compressed: &[u8], legacy: bool, what: &str) -> io::Result<ColumnsMeta> {
    let bytes = zstd::decode_all(compressed)
        .map_err(|e| invalid_data(format!("{what} could not be decompressed: {e}")))?;
    if legacy {
        return decode_disk_serde(&bytes, bytes.capacity() as u64).map(ColumnsMeta::shared);
    }
    decode_disk_serde::<Envelope>(&bytes, bytes.capacity() as u64)?.into_meta(what)
}

/// The sidecar a reader should use for the directory `dir`, if any.
///
/// The segmented location (`seg_000/`) is searched before the legacy flat root.
/// Within one location the compact binary form wins over JSON, which is the
/// slow path.
pub(crate) fn locate(dir: &Path) -> Option<PathBuf> {
    [Path::new("seg_000"), Path::new("")]
        .into_iter()
        .flat_map(|sub| [CURRENT_BIN, LEGACY_BIN, JSON].map(|name| dir.join(sub).join(name)))
        .find(|path| path.exists())
}

/// Read the sidecar `locate` returned.
pub(crate) fn read(path: &Path) -> io::Result<ColumnsMeta> {
    let what = format!("disk graph column metadata '{}'", path.display());
    let name = path.file_name().and_then(|n| n.to_str());
    if name == Some(JSON) {
        let json = std::fs::read_to_string(path).map_err(|e| io_context("reading", path, e))?;
        return parse_json(&json, &what);
    }
    let compressed = std::fs::read(path).map_err(|e| io_context("reading", path, e))?;
    parse_bin(&compressed, name == Some(LEGACY_BIN), &what)
}

/// Publish the sidecar of a shared-`columns.bin` layout (every type without a
/// file) into `data_dir` next to its `columns.bin`.
///
/// Writes both encodings: the JSON envelope is what makes an older reader stop
/// (see the module header), and the binary form is what this build's loader
/// prefers on a large graph.
pub(crate) fn publish(data_dir: &Path, types: &[ColumnTypeMeta]) -> io::Result<PathBuf> {
    let meta = ColumnsMeta::shared(types.to_vec());
    let json_path = data_dir.join(JSON);
    std::fs::write(&json_path, to_json(&meta, false)?)?;
    std::fs::write(data_dir.join(CURRENT_BIN), to_bin(&meta)?)?;
    Ok(json_path)
}

/// Write only the JSON envelope, durably, into `seg0`.
pub(crate) fn publish_json_synced(seg0: &Path, meta: &ColumnsMeta) -> io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(seg0.join(JSON))?;
    file.write_all(to_json(meta, true)?.as_bytes())?;
    file.sync_all()
}

/// Record which zstd sidecar directory holds each type the column files do not.
///
/// Rewrites the envelope a save just wrote in `dir` (adding the `sidecars`
/// entries), or writes a types-less one when the stage has no column files at
/// all, so a reader always learns a sidecar's type from the sidecar and never
/// from a directory name. Both encodings are kept in step: the binary form wins
/// at load when it exists.
pub(crate) fn record_sidecars(dir: &Path, sidecars: BTreeMap<String, String>) -> io::Result<()> {
    if sidecars.is_empty() {
        return Ok(());
    }
    let (mut meta, seg0) = match locate(dir) {
        Some(path) => {
            let seg0 = path.parent().unwrap_or(dir).to_path_buf();
            (read(&path)?, seg0)
        }
        None => {
            let seg0 = dir.join("seg_000");
            std::fs::create_dir_all(&seg0)?;
            (ColumnsMeta::default(), seg0)
        }
    };
    meta.sidecars = sidecars;
    publish_json_synced(&seg0, &meta)?;
    if seg0.join(CURRENT_BIN).exists() {
        std::fs::write(seg0.join(CURRENT_BIN), to_bin(&meta)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::io::ntriples::{ColMapEntry, RegionMeta};

    fn region(offset: usize, len: usize) -> RegionMeta {
        RegionMeta { offset, len }
    }

    fn sample(name: &str) -> ColumnTypeMeta {
        ColumnTypeMeta {
            type_name: name.to_string(),
            row_count: 3,
            id_is_string: false,
            id_data: region(0, 24),
            id_nulls: region(24, 3),
            id_str_data: region(0, 0),
            id_str_offsets: region(0, 0),
            title_data: region(27, 9),
            title_offsets: region(36, 24),
            title_nulls: region(60, 3),
            col_map: vec![ColMapEntry {
                key_u64: 7,
                col_type_str: "int64".into(),
                idx: 0,
            }],
            fixed_cols: Vec::new(),
            str_cols: Vec::new(),
            overflow_offsets: region(0, 0),
            overflow_data: region(0, 0),
            has_overflow: false,
        }
    }

    fn shared(types: Vec<ColumnTypeMeta>) -> ColumnsMeta {
        ColumnsMeta::shared(types)
    }

    fn names(meta: ColumnsMeta) -> Vec<String> {
        meta.types.into_iter().map(|m| m.type_name).collect()
    }

    /// The forward guard itself: what an older reader does with the new file.
    /// 0.19.0 deserialises `Vec<ColumnTypeMeta>` from this text; if the writer
    /// ever went back to a bare array this would parse and the guard would be gone.
    #[test]
    fn the_json_an_older_reader_would_parse_as_an_array_is_not_an_array() {
        let json = to_json(&shared(vec![sample("Employment")]), false).unwrap();
        assert!(
            serde_json::from_str::<Vec<ColumnTypeMeta>>(&json).is_err(),
            "an older reader must fail to deserialise the new sidecar: {json}"
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["format"], COLUMNS_META_FORMAT);
        assert_eq!(value["types"][0]["type_name"], "Employment");
    }

    #[test]
    fn envelope_and_legacy_bare_array_both_parse() {
        let types = vec![sample("A"), sample("B")];
        let envelope = parse_json(&to_json(&shared(types.clone()), true).unwrap(), "test").unwrap();
        assert_eq!(names(envelope), ["A", "B"]);
        let legacy = serde_json::to_string(&types).unwrap();
        assert_eq!(names(parse_json(&legacy, "test").unwrap()), ["A", "B"]);
        let padded = format!("\n  {legacy}");
        assert_eq!(names(parse_json(&padded, "test").unwrap()), ["A", "B"]);
        assert!(parse_json(&legacy, "test").unwrap().files.is_empty());
    }

    #[test]
    fn a_newer_envelope_format_is_refused_with_an_upgrade_message() {
        let json = r#"{"format": 3, "types": []}"#;
        let error = parse_json(json, "sidecar").err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let message = error.to_string();
        assert!(
            message.contains("format 3")
                && message.contains("up to format 2")
                && message.contains("Please upgrade kglite"),
            "{message}"
        );
    }

    #[test]
    fn a_legacy_bin_and_a_current_bin_each_read_through_their_own_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let types = [sample("A")];

        let legacy =
            zstd::encode_all(encode_disk_serde(&types[..]).unwrap().as_slice(), 3).unwrap();
        std::fs::write(dir.path().join(LEGACY_BIN), legacy).unwrap();
        let found = locate(dir.path()).unwrap();
        assert!(found.ends_with(LEGACY_BIN));
        assert_eq!(read(&found).unwrap().types[0].type_name, "A");

        // Publishing writes the envelope forms; the current bin outranks a
        // legacy one left beside it, and the JSON stays for older readers to trip on.
        let json_path = publish(dir.path(), &types).unwrap();
        assert!(json_path.ends_with(JSON));
        let found = locate(dir.path()).unwrap();
        assert!(found.ends_with(CURRENT_BIN), "{found:?}");
        assert_eq!(read(&found).unwrap().types[0].type_name, "A");
        assert_eq!(read(&json_path).unwrap().types[0].type_name, "A");
        assert!(serde_json::from_str::<Vec<ColumnTypeMeta>>(
            &std::fs::read_to_string(json_path).unwrap()
        )
        .is_err());
    }

    /// An older reader prefers `columns_meta.bin.zst` whenever it exists, so
    /// the new binary sidecar must not be spelled that way.
    #[test]
    fn the_current_binary_sidecar_does_not_reuse_the_legacy_file_name() {
        let dir = tempfile::tempdir().unwrap();
        publish(dir.path(), &[sample("A")]).unwrap();
        assert!(!dir.path().join(LEGACY_BIN).exists());
        assert!(dir.path().join(CURRENT_BIN).exists());
    }

    #[test]
    fn the_segmented_location_is_searched_before_the_flat_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("seg_000")).unwrap();
        std::fs::write(dir.path().join(JSON), "[]").unwrap();
        std::fs::write(dir.path().join("seg_000").join(JSON), "[]").unwrap();
        assert!(locate(dir.path())
            .unwrap()
            .starts_with(dir.path().join("seg_000")));
    }

    #[test]
    fn the_file_map_survives_both_encodings() {
        let mut meta = shared(vec![sample("A"), sample("B")]);
        meta.files
            .insert("A".into(), "type_columns/0000000000000001.bin".into());
        let json = parse_json(&to_json(&meta, false).unwrap(), "test").unwrap();
        assert_eq!(json.files, meta.files);
        let bin = parse_bin(&to_bin(&meta).unwrap(), false, "test").unwrap();
        assert_eq!(bin.files, meta.files);
        assert_eq!(names(bin), ["A", "B"]);
    }

    #[test]
    fn a_file_name_is_path_safe_stable_and_collision_free() {
        let mut used = HashSet::new();
        let hostile = "../../etc/passwd";
        let first = type_file_name(hostile, &mut used);
        assert!(
            first.starts_with("type_columns/") && first.ends_with(".bin"),
            "{first}"
        );
        assert!(!first.contains(".."), "{first}");
        assert_eq!(
            type_file_name(hostile, &mut HashSet::new()),
            first,
            "the name must not depend on what else was written"
        );
        // The same key asked for twice must not hand out the same file.
        let second = type_file_name(hostile, &mut used);
        assert_ne!(first, second);
        assert!(resolve_type_file(Path::new("seg"), &first).is_ok());
        assert!(resolve_type_file(Path::new("seg"), &second).is_ok());
    }

    #[test]
    fn a_sidecar_naming_a_file_outside_its_directory_is_refused() {
        for bad in [
            "",
            "/etc/passwd",
            "../x.bin",
            "type_columns/../../x.bin",
            "./",
        ] {
            let error = resolve_type_file(Path::new("seg"), bad).err();
            assert!(
                error.is_some_and(|e| e.kind() == io::ErrorKind::InvalidData),
                "{bad:?} must be refused"
            );
        }
    }
}
