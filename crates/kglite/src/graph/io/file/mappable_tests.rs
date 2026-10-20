//! The v9 container machinery: section table, aligned writer, reader handles,
//! and the guarantee that a graph with no mappable section still writes v8.

use super::mappable::*;
use super::save_sections::write_kgl_container;
use super::*;
use crate::datatypes::{DataFrame, Value};
use crate::graph::io::file::container_test_support::*;
use std::borrow::Cow;

fn graph(n: i64) -> Arc<DirGraph> {
    let mut g = DirGraph::new();
    let rows: Vec<Vec<Value>> = (1..=n)
        .map(|i| vec![Value::Int64(i), Value::String(format!("t{i}"))])
        .collect();
    let df =
        DataFrame::from_cypher_rows(vec!["id".to_string(), "title".to_string()], rows).unwrap();
    crate::graph::mutation::maintain::add_nodes(
        &mut g,
        df,
        "Doc".to_string(),
        "id".to_string(),
        Some("title".to_string()),
        None,
    )
    .unwrap();
    let mut arc = Arc::new(g);
    prepare_save(&mut arc);
    Arc::make_mut(&mut arc).enable_columnar();
    arc
}

fn section(name: &str, dtype: SectionDtype, data: Vec<u8>) -> MappableSection<'static> {
    let width = dtype.width() as usize;
    assert_eq!(data.len() % width, 0);
    MappableSection {
        name: name.to_string(),
        owner: "test".to_string(),
        kind: 0,
        dtype,
        elem_count: (data.len() / width) as u64,
        flags: 0,
        data: Cow::Owned(data),
    }
}

fn f32_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Sections of several dtypes and awkward lengths, so padding is exercised.
fn synthetic_sections() -> Vec<MappableSection<'static>> {
    let mut text = section(
        "str:bytes",
        SectionDtype::U8,
        b"hello \xc3\xa5 world".to_vec(),
    );
    text.flags = FLAG_UTF8_VALIDATED;
    vec![
        section(
            "emb:Doc:v:data",
            SectionDtype::F32,
            f32_bytes(&[1.0, 2.0, 3.0, 4.5, -0.25]),
        ),
        section("emb:Doc:v:q", SectionDtype::I8, vec![1, 255, 3]),
        section(
            "col:Doc:n:data",
            SectionDtype::I64,
            (0..9i64).flat_map(|v| v.to_le_bytes()).collect(),
        ),
        text,
        section("bits", SectionDtype::BitPacked, vec![0b1010_0101; 7]),
    ]
}

fn v9_bytes(sections: Vec<MappableSection<'_>>) -> Vec<u8> {
    let mut out = Vec::new();
    write_kgl_container(&graph(4), &mut out, None, SaveLayout::Auto, sections).unwrap();
    out
}

fn head(bytes: &[u8]) -> (FileMetadata, u64) {
    let metadata = metadata_head::read_metadata_head(bytes, "test").unwrap();
    let metadata_end = 13 + u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
    let base = data_region_base(&metadata, metadata_end, bytes.len() as u64).unwrap();
    (metadata, base)
}

fn heap_table(bytes: &[u8], verify: Verify) -> io::Result<MappableTable> {
    let (metadata, base) = head(bytes);
    MappableTable::open(&metadata.mappable, bytes, base, None, verify, |_| false)
}

#[test]
fn a_graph_with_no_mappable_section_still_writes_v8_bytes() {
    let g = graph(6);
    let mut default = Vec::new();
    write_kgl_to(&g, &mut default).unwrap();
    assert_eq!(&default[..4], &V8_MAGIC);

    // Compact collects nothing and so emits the identical container.
    let mut compact = Vec::new();
    write_kgl_container(
        &g,
        &mut compact,
        None,
        SaveLayout::Compact,
        collect_mappable_sections(&g, SaveLayout::Compact).unwrap(),
    )
    .unwrap();
    assert_eq!(default, compact);

    // The additive key is absent, not merely empty: nothing in the metadata
    // differs from what the writer produced before the table existed.
    let json = std::str::from_utf8(metadata_json(&default)).unwrap();
    assert!(!json.contains("mappable"), "{json}");
    // And it is deterministic across writes, the property the golden digests need.
    let mut again = Vec::new();
    write_kgl_to(&g, &mut again).unwrap();
    assert_eq!(default, again);
}

#[test]
fn v9_roundtrips_every_section_with_alignment_dtype_and_crc() {
    let sections = synthetic_sections();
    let expected: Vec<(String, SectionDtype, Vec<u8>)> = sections
        .iter()
        .map(|s| (s.name.clone(), s.dtype, s.data.to_vec()))
        .collect();
    let bytes = v9_bytes(sections);
    assert_eq!(&bytes[..4], &V9_MAGIC);

    let (metadata, base) = head(&bytes);
    assert_eq!(base % SECTION_ALIGNMENT, 0);
    assert_eq!(metadata.mappable.len(), expected.len());
    for entry in &metadata.mappable {
        assert_eq!(
            (base + entry.offset) % SECTION_ALIGNMENT,
            0,
            "{}",
            entry.name
        );
        assert_ne!(entry.flags & FLAG_LITTLE_ENDIAN, 0);
    }

    let table = heap_table(&bytes, Verify::Eager).unwrap();
    assert_eq!(table.len(), expected.len());
    for (name, dtype, data) in &expected {
        let handle = table.get(name).unwrap();
        assert_eq!(handle.dtype, *dtype, "{name}");
        assert_eq!(handle.as_bytes(), data.as_slice(), "{name}");
        assert_eq!(handle.elem_count, data.len() as u64 / dtype.width());
        assert!(!handle.is_mapped());
        handle.verify().unwrap();
    }
    table.verify_all().unwrap();

    // The compressed half is an ordinary container: the graph loads, with the
    // data region ignored until a consumer asks for it.
    let loaded = load_kgl_bytes(&bytes).unwrap();
    assert_eq!(loaded.graph.node_count(), 4);
}

#[test]
fn mapped_and_heap_handles_yield_the_same_bytes_and_alignment() {
    let bytes = v9_bytes(synthetic_sections());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    std::fs::write(&path, &bytes).unwrap();

    let file = MappedFile::open(&path).unwrap();
    let (metadata, base) = head(file.bytes());
    let mapped = MappableTable::open(
        &metadata.mappable,
        file.bytes(),
        base,
        Some(&file),
        Verify::Lazy,
        |_| true,
    )
    .unwrap();
    let heap = heap_table(&bytes, Verify::Lazy).unwrap();
    for handle in mapped.iter() {
        assert!(handle.is_mapped());
        assert_eq!(
            handle.as_bytes(),
            heap.get(&handle.name).unwrap().as_bytes()
        );
        assert_eq!(
            handle.as_bytes().as_ptr() as usize % SECTION_ALIGNMENT as usize,
            0
        );
        handle.verify().unwrap();
    }
}

#[test]
fn a_flipped_payload_byte_fails_verify_but_not_lazy_open() {
    let mut bytes = v9_bytes(synthetic_sections());
    let (metadata, base) = head(&bytes);
    let victim = metadata
        .mappable
        .iter()
        .find(|e| e.name == "col:Doc:n:data")
        .unwrap();
    bytes[(base + victim.offset) as usize + 3] ^= 0x40;

    let table = heap_table(&bytes, Verify::Lazy).unwrap();
    let error = table.get("col:Doc:n:data").unwrap().verify().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("col:Doc:n:data"), "{error}");
    assert!(table.get("emb:Doc:v:data").unwrap().verify().is_ok());
    assert!(table.verify_all().is_err());
    assert!(heap_table(&bytes, Verify::Eager).is_err());
}

#[test]
fn a_string_section_is_checked_at_open_even_when_lazy() {
    let mut bytes = v9_bytes(synthetic_sections());
    let (metadata, base) = head(&bytes);
    let text = metadata
        .mappable
        .iter()
        .find(|e| e.name == "str:bytes")
        .unwrap();
    bytes[(base + text.offset) as usize] ^= 0x01;
    let error = heap_table(&bytes, Verify::Lazy).unwrap_err();
    assert!(error.to_string().contains("str:bytes"), "{error}");
}

#[test]
fn a_truncated_v9_file_is_a_clear_error_not_a_fault() {
    let bytes = v9_bytes(synthetic_sections());
    for cut in [1usize, 5, 40] {
        let error = load_kgl_bytes(&bytes[..bytes.len() - cut]).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{error}");
        assert!(
            error.to_string().contains("truncated"),
            "cut {cut}: {error}"
        );
    }
}

#[test]
fn truncating_a_mapped_file_is_reported_before_any_page_is_touched() {
    let bytes = v9_bytes(synthetic_sections());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    std::fs::write(&path, &bytes).unwrap();
    let file = MappedFile::open(&path).unwrap();
    let (metadata, base) = head(file.bytes());
    let table = MappableTable::open(
        &metadata.mappable,
        file.bytes(),
        base,
        Some(&file),
        Verify::Lazy,
        |_| true,
    )
    .unwrap();
    file.check_intact().unwrap();

    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(bytes.len() as u64 / 2)
        .unwrap();

    let error = file.check_intact().unwrap_err();
    assert!(
        error.to_string().contains("truncated or replaced"),
        "{error}"
    );
    let handle = table.get("emb:Doc:v:data").unwrap();
    assert!(handle.verify().is_err());
    assert!(handle.checked_bytes().is_err());
}

#[cfg(unix)]
#[test]
fn a_handle_outlives_its_graph_and_a_save_over_the_same_path() {
    let sections = synthetic_sections();
    let first = v9_bytes(sections.clone());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    std::fs::write(&path, &first).unwrap();

    let handle = {
        let file = MappedFile::open(&path).unwrap();
        let (metadata, base) = head(file.bytes());
        let table = MappableTable::open(
            &metadata.mappable,
            file.bytes(),
            base,
            Some(&file),
            Verify::Lazy,
            |_| true,
        )
        .unwrap();
        table.get("emb:Doc:v:data").unwrap().clone()
        // `file` and `table` drop here; only the handle's Arc keeps the map.
    };

    // Replace the path the way `save()` does: temp file, rename over it.
    let replacement = dir.path().join("tmp");
    std::fs::write(
        &replacement,
        v9_bytes(vec![section("x", SectionDtype::U8, vec![9; 4])]),
    )
    .unwrap();
    std::fs::rename(&replacement, &path).unwrap();

    assert!(handle.is_mapped());
    assert_eq!(
        handle.as_bytes(),
        f32_bytes(&[1.0, 2.0, 3.0, 4.5, -0.25]).as_slice()
    );
    handle.verify().unwrap();
}

#[test]
fn the_table_refuses_every_malformed_entry() {
    let base = 64u64;
    let good = MapEntry {
        name: "a".into(),
        owner: "o".into(),
        kind: 0,
        dtype: SectionDtype::F32.tag(),
        elem_count: 4,
        offset: 0,
        len: 16,
        flags: FLAG_LITTLE_ENDIAN,
        crc32: 0,
    };
    let validate =
        |entries: &[MapEntry], file_len: u64| MappableTable::validate(entries, file_len, base);
    assert!(validate(std::slice::from_ref(&good), 80).is_ok());

    let mut cases: Vec<(&str, Vec<MapEntry>, u64, &str)> = Vec::new();
    cases.push(("past end", vec![good.clone()], 79, "truncated"));
    let mut e = good.clone();
    e.offset = 8;
    cases.push(("misaligned", vec![e], 200, "aligned"));
    let mut e = good.clone();
    e.len = 12;
    cases.push(("len mismatch", vec![e], 200, "declares"));
    let mut e = good.clone();
    e.dtype = 99;
    cases.push(("unknown dtype", vec![e], 200, "dtype"));
    let mut e = good.clone();
    e.flags = FLAG_LITTLE_ENDIAN | (1 << 9);
    cases.push(("unknown flag", vec![e], 200, "flags"));
    let mut e = good.clone();
    e.flags = 0;
    cases.push(("no LE flag", vec![e], 200, "flags"));
    let mut second = good.clone();
    second.name = "b".into();
    second.offset = 0;
    cases.push(("overlap", vec![good.clone(), second], 200, "overlaps"));
    let mut second = good.clone();
    second.offset = 64;
    cases.push(("duplicate name", vec![good.clone(), second], 300, "twice"));
    let mut e = good.clone();
    e.offset = u64::MAX - 7;
    cases.push(("offset overflow", vec![e], 200, "aligned"));
    let mut e = good.clone();
    e.elem_count = u64::MAX;
    cases.push(("count overflow", vec![e], 200, "declares"));

    for (label, entries, file_len, needle) in cases {
        let error = validate(&entries, file_len).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{label}");
        assert!(error.to_string().contains(needle), "{label}: {error}");
    }
}

#[test]
fn the_writer_refuses_inconsistent_or_duplicate_sections() {
    let mut bad = section("a", SectionDtype::F32, vec![0; 8]);
    bad.elem_count = 3;
    assert!(plan_data_region(&[bad]).is_err());
    let a = section("a", SectionDtype::U8, vec![1]);
    assert!(plan_data_region(&[a.clone(), a]).is_err());
}

#[test]
fn offsets_are_aligned_for_any_length_mix() {
    for lens in [
        vec![1usize, 1, 1],
        vec![63, 64, 65],
        vec![0, 7, 0, 128],
        vec![200],
    ] {
        let sections: Vec<_> = lens
            .iter()
            .enumerate()
            .map(|(i, n)| section(&format!("s{i}"), SectionDtype::U8, vec![i as u8; *n]))
            .collect();
        let (entries, total) = plan_data_region(&sections).unwrap();
        let mut prev_end = 0;
        for entry in &entries {
            assert_eq!(entry.offset % SECTION_ALIGNMENT, 0);
            assert!(entry.offset >= prev_end);
            prev_end = entry.offset + entry.len;
        }
        assert_eq!(total, prev_end);
    }
}

#[test]
fn a_v8_container_cannot_carry_a_section_table() {
    let v9 = v9_bytes(synthetic_sections());
    // Re-label a v9 body as v8: its metadata still declares a table.
    let mut v8_labelled = v9.clone();
    v8_labelled[3] = V8_MAGIC[3];
    let json = {
        let len = u32::from_le_bytes(v8_labelled[9..13].try_into().unwrap()) as usize;
        v8_labelled[13..13 + len - 4].to_vec()
    };
    let resealed = with_metadata_json_any(&v8_labelled, &json);
    let error = load_kgl_bytes(&resealed).err().unwrap();
    assert!(error.to_string().contains("only v9"), "{error}");
}

/// `with_metadata_json` for a container the helper's v8-only asserts would
/// refuse: the header and trailing sections are taken as they are.
fn with_metadata_json_any(bytes: &[u8], json: &[u8]) -> Vec<u8> {
    let len = u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
    let mut out = sealed_metadata(&bytes[..9], json);
    out.extend_from_slice(&bytes[13 + len..]);
    out
}

#[test]
fn v8_and_older_files_load_unchanged() {
    let g = graph(5);
    let mut v8 = Vec::new();
    write_kgl_to(&g, &mut v8).unwrap();
    for magic in [V5_MAGIC, V6_MAGIC, V7_MAGIC] {
        assert_eq!(
            load_kgl_bytes(&as_legacy_container(&v8, magic))
                .unwrap()
                .graph
                .node_count(),
            5
        );
    }
    assert_eq!(load_kgl_bytes(&v8).unwrap().graph.node_count(), 5);
}

#[test]
fn an_unknown_future_container_version_is_refused_with_the_upgrade_message() {
    let mut bytes = v9_bytes(synthetic_sections());
    bytes[3] = 10;
    let error = load_kgl_bytes(&bytes).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        error.to_string(),
        "File uses .kgl container version 10, but this library only supports up to version 9. Please upgrade kglite."
    );
}

#[test]
fn mapping_policy_truth_table() {
    let base = MapContext {
        file_backed: true,
        regular_file: true,
        local_fs: true,
        windows: false,
        big_endian: false,
        section_len: 100,
        env_disabled: false,
    };
    let at = |f: fn(&mut MapContext)| {
        let mut c = base;
        f(&mut c);
        c
    };
    // Auto: size floor and platform.
    assert!(resolve_mapping(MapPolicy::Auto, 100, &base));
    assert!(!resolve_mapping(MapPolicy::Auto, 101, &base));
    assert!(!resolve_mapping(
        MapPolicy::Auto,
        0,
        &at(|c| c.windows = true)
    ));
    assert!(!resolve_mapping(
        MapPolicy::Auto,
        0,
        &at(|c| c.local_fs = false)
    ));
    // Explicit Mapped ignores size, Windows and a network filesystem.
    assert!(resolve_mapping(
        MapPolicy::Mapped,
        0,
        &at(|c| c.local_fs = false)
    ));
    assert!(resolve_mapping(
        MapPolicy::Mapped,
        u64::MAX,
        &at(|c| c.windows = true)
    ));
    assert!(!resolve_mapping(MapPolicy::Memory, 0, &base));
    // Nothing maps a byte buffer, a non-regular file, a big-endian host, or
    // a process with the kill switch set.
    for blocked in [
        at(|c| c.file_backed = false),
        at(|c| c.regular_file = false),
        at(|c| c.big_endian = true),
        at(|c| c.env_disabled = true),
    ] {
        for policy in [MapPolicy::Auto, MapPolicy::Mapped, MapPolicy::Memory] {
            assert!(!resolve_mapping(policy, 0, &blocked));
        }
    }
}

#[test]
fn save_over_a_held_mapping_must_materialize_only_on_windows() {
    let dest = Path::new("/data/g.kgl");
    let other = Path::new("/data/other.kgl");
    assert!(must_materialize_before_save(true, dest, &[dest]));
    assert!(!must_materialize_before_save(true, dest, &[other]));
    assert!(!must_materialize_before_save(true, dest, &[]));
    assert!(!must_materialize_before_save(false, dest, &[dest]));
}

#[test]
fn load_options_mapping_fields_default_to_auto_and_chain() {
    let options = LoadOptions::new();
    assert_eq!(options.embeddings, MapPolicy::Auto);
    assert_eq!(options.columns, MapPolicy::Auto);
    let tuned = LoadOptions::new()
        .with_embeddings(MapPolicy::Mapped)
        .with_columns(MapPolicy::Memory)
        .with_verify(Verify::Eager);
    assert_eq!(tuned.embeddings, MapPolicy::Mapped);
    assert_eq!(tuned.columns, MapPolicy::Memory);
    assert_eq!(tuned.verify, Verify::Eager);
    // Inert for a v8 file: the options change nothing about what loads.
    let mut v8 = Vec::new();
    write_kgl_to(&graph(3), &mut v8).unwrap();
    assert_eq!(
        load_kgl_bytes_with(&v8, &tuned).unwrap().graph.node_count(),
        3
    );
}

#[test]
fn the_host_context_describes_a_regular_mapped_file_and_the_kill_switch_parses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    std::fs::write(&path, v9_bytes(synthetic_sections())).unwrap();
    let file = MappedFile::open(&path).unwrap();
    assert_eq!(file.path(), path.as_path());
    assert_eq!(file.len(), file.bytes().len() as u64);
    assert!(!file.modified_since_open().unwrap());
    let ctx = MapContext::host(Some(&file), 10);
    assert!(ctx.file_backed && ctx.regular_file);
    assert!(!MapContext::host(None, 10).file_backed);

    assert!(mmap_off(Some("0")) && mmap_off(Some(" Off ")) && mmap_off(Some("false")));
    assert!(!mmap_off(Some("1")) && !mmap_off(Some("")) && !mmap_off(None));
}

#[test]
fn a_loaded_section_exposes_its_table_row() {
    let mut sections = synthetic_sections();
    sections[0].kind = 7;
    sections[0].owner = "embeddings.zst".to_string();
    let table = heap_table(&v9_bytes(sections), Verify::Lazy).unwrap();
    let handle = table.get("emb:Doc:v:data").unwrap();
    assert_eq!((handle.kind, handle.owner.as_str()), (7, "embeddings.zst"));
    assert!(table.len() > 0);
    let source = LoadSource {
        bytes: &[],
        file: None,
    };
    assert!(source.file.is_none());
}
