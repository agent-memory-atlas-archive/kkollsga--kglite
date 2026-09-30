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
//! columns_meta.json          {"format": 2, "types": [ColumnTypeMeta, ...]}
//! columns_meta.v2.bin.zst    zstd(postcard-framed {format: u32, types: [...]})
//! ```
//!
//! The binary form moved to a new file name for the same reason the JSON form
//! changed shape: a reader that prefers `columns_meta.bin.zst` when it exists
//! would otherwise decode the new bytes as the old bare array. Readers here
//! still accept both legacy spellings, so a directory 0.19.0 wrote opens.

use std::io;
use std::path::{Path, PathBuf};

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

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: u32,
    types: Vec<ColumnTypeMeta>,
}

/// Borrowing twin of [`Envelope`] so a writer does not clone every type's metadata.
#[derive(Serialize)]
struct EnvelopeRef<'a> {
    format: u32,
    types: &'a [ColumnTypeMeta],
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

/// Serialise `types` as the JSON envelope.
pub(crate) fn to_json(types: &[ColumnTypeMeta], pretty: bool) -> io::Result<String> {
    let envelope = EnvelopeRef {
        format: COLUMNS_META_FORMAT,
        types,
    };
    let json = if pretty {
        serde_json::to_string_pretty(&envelope)
    } else {
        serde_json::to_string(&envelope)
    };
    json.map_err(io::Error::other)
}

/// Serialise `types` as the framed, compressed binary envelope.
pub(crate) fn to_bin(types: &[ColumnTypeMeta]) -> io::Result<Vec<u8>> {
    let bytes = encode_disk_serde(&EnvelopeRef {
        format: COLUMNS_META_FORMAT,
        types,
    })?;
    zstd::encode_all(bytes.as_slice(), 3)
}

/// Parse the JSON form: the envelope, or the bare array 0.19.0 and earlier wrote.
///
/// The first non-blank byte picks the shape, so neither reads the document
/// twice — this file is hundreds of megabytes on a Wikidata-scale graph.
pub(crate) fn parse_json(json: &str, what: &str) -> io::Result<Vec<ColumnTypeMeta>> {
    let bad = |e: serde_json::Error| invalid_data(format!("{what} is not valid JSON: {e}"));
    if json.trim_start().starts_with('[') {
        return serde_json::from_str(json).map_err(bad);
    }
    let envelope: Envelope = serde_json::from_str(json).map_err(bad)?;
    if envelope.format > COLUMNS_META_FORMAT {
        return Err(newer_format_error(envelope.format, what));
    }
    Ok(envelope.types)
}

fn parse_bin(compressed: &[u8], legacy: bool, what: &str) -> io::Result<Vec<ColumnTypeMeta>> {
    let bytes = zstd::decode_all(compressed)
        .map_err(|e| invalid_data(format!("{what} could not be decompressed: {e}")))?;
    if legacy {
        return decode_disk_serde(&bytes, bytes.capacity() as u64);
    }
    let envelope: Envelope = decode_disk_serde(&bytes, bytes.capacity() as u64)?;
    if envelope.format > COLUMNS_META_FORMAT {
        return Err(newer_format_error(envelope.format, what));
    }
    Ok(envelope.types)
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
pub(crate) fn read(path: &Path) -> io::Result<Vec<ColumnTypeMeta>> {
    let what = format!("disk graph column metadata '{}'", path.display());
    let name = path.file_name().and_then(|n| n.to_str());
    if name == Some(JSON) {
        let json = std::fs::read_to_string(path).map_err(|e| io_context("reading", path, e))?;
        return parse_json(&json, &what);
    }
    let compressed = std::fs::read(path).map_err(|e| io_context("reading", path, e))?;
    parse_bin(&compressed, name == Some(LEGACY_BIN), &what)
}

/// Publish the sidecar into `data_dir` next to its `columns.bin`.
///
/// Writes both encodings: the JSON envelope is what makes an older reader stop
/// (see the module header), and the binary form is what this build's loader
/// prefers on a large graph.
pub(crate) fn publish(data_dir: &Path, types: &[ColumnTypeMeta]) -> io::Result<PathBuf> {
    let json_path = data_dir.join(JSON);
    std::fs::write(&json_path, to_json(types, false)?)?;
    std::fs::write(data_dir.join(CURRENT_BIN), to_bin(types)?)?;
    Ok(json_path)
}

/// Write only the JSON envelope, durably, into `seg_000/`.
pub(crate) fn publish_json_synced(seg0: &Path, types: &[ColumnTypeMeta]) -> io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(seg0.join(JSON))?;
    file.write_all(to_json(types, true)?.as_bytes())?;
    file.sync_all()
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

    /// The forward guard itself: what an older reader does with the new file.
    /// 0.19.0 deserialises `Vec<ColumnTypeMeta>` from this text; if the writer
    /// ever went back to a bare array this would parse and the guard would be gone.
    #[test]
    fn the_json_an_older_reader_would_parse_as_an_array_is_not_an_array() {
        let json = to_json(&[sample("Pand")], false).unwrap();
        assert!(
            serde_json::from_str::<Vec<ColumnTypeMeta>>(&json).is_err(),
            "an older reader must fail to deserialise the new sidecar: {json}"
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["format"], COLUMNS_META_FORMAT);
        assert_eq!(value["types"][0]["type_name"], "Pand");
    }

    #[test]
    fn envelope_and_legacy_bare_array_both_parse() {
        let types = [sample("A"), sample("B")];
        let names = |metas: Vec<ColumnTypeMeta>| -> Vec<String> {
            metas.into_iter().map(|m| m.type_name).collect()
        };
        let envelope = parse_json(&to_json(&types, true).unwrap(), "test").unwrap();
        assert_eq!(names(envelope), ["A", "B"]);
        let legacy = serde_json::to_string(&types).unwrap();
        assert_eq!(names(parse_json(&legacy, "test").unwrap()), ["A", "B"]);
        let padded = format!("\n  {legacy}");
        assert_eq!(names(parse_json(&padded, "test").unwrap()), ["A", "B"]);
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
        assert_eq!(read(&found).unwrap()[0].type_name, "A");

        // Publishing writes the envelope forms; the current bin outranks a
        // legacy one left beside it, and the JSON stays for older readers to trip on.
        let json_path = publish(dir.path(), &types).unwrap();
        assert!(json_path.ends_with(JSON));
        let found = locate(dir.path()).unwrap();
        assert!(found.ends_with(CURRENT_BIN), "{found:?}");
        assert_eq!(read(&found).unwrap()[0].type_name, "A");
        assert_eq!(read(&json_path).unwrap()[0].type_name, "A");
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
}
