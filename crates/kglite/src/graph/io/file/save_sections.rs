//! The `.kgl` section encoders and stream writer behind [`write_kgl_to`]
//! and the stamped backup path, split from `file.rs` (source-quality
//! ceilings). One serializer body serves both: the stamp is a parameter.

use super::*;

/// Compressed optional sections, each `None` when the graph has nothing for it.
struct OptionalSections {
    embeddings: Option<Vec<u8>>,
    edge_embeddings: Option<Vec<u8>>,
    timeseries: Option<Vec<u8>>,
    secondary_labels: Option<Vec<u8>>,
    vector_index: Option<Vec<u8>>,
    text_index: Option<Vec<u8>>,
    edge_vector_index: Option<Vec<u8>>,
    edge_text_index: Option<Vec<u8>>,
    core_version: u32,
}

impl OptionalSections {
    /// In file order: the relationship BM25 section is last so a reader that
    /// predates it stops before those bytes.
    fn in_file_order(&self) -> [Option<&[u8]>; 8] {
        [
            self.embeddings.as_deref(),
            self.edge_embeddings.as_deref(),
            self.timeseries.as_deref(),
            self.secondary_labels.as_deref(),
            self.vector_index.as_deref(),
            self.text_index.as_deref(),
            self.edge_vector_index.as_deref(),
            self.edge_text_index.as_deref(),
        ]
    }
}

fn compress_some(payload: Option<Vec<u8>>) -> io::Result<Option<Vec<u8>>> {
    payload.map(|p| zstd_compress(&p)).transpose()
}

/// Topology with node properties stripped into the column sections.
fn encode_topology(graph: &DirGraph, codec: serde_codec::CodecVersion) -> io::Result<Vec<u8>> {
    let topology_raw = {
        let _strip = StripPropertiesGuard::new();
        let _guard = SerdeSerializeGuard::new(&graph.interner);
        codec_ser(codec, &graph.graph)?
    };
    zstd_compress(&topology_raw)
}

/// Column sections, one per node type, sorted by type_name: the backend's
/// map is a HashMap whose per-instance RandomState would otherwise vary the
/// section order across processes, breaking the byte-level reproducibility
/// the `test_phase4_parity` golden-hash test relies on. Sorting is free
/// (type_name count is small) and doesn't affect the format: each section
/// is self-describing and the decoder iterates column_sections_meta in order.
fn encode_column_sections(
    graph: &DirGraph,
    codec: serde_codec::CodecVersion,
) -> io::Result<(Vec<PortableColumnSection>, Vec<Vec<u8>>)> {
    let mut meta_out = Vec::new();
    let mut data_out = Vec::new();
    let mut stores: Vec<(&str, &Arc<ColumnStore>)> = graph.column_stores_by_name();
    stores.sort_by(|a, b| a.0.cmp(b.0));
    for (type_name, store) in stores {
        // A writer overlay's store can carry a heap tail; the sections are
        // written from its columns, so they must hold every row.
        let store = store.with_heap_tail_folded();
        let packed = store.write_packed_with_codec(
            &graph.interner,
            codec,
            // v6: integer columns pick their smaller encoding per column.
            crate::graph::storage::packed_codec::IntColumnEncoding::Auto,
        )?;
        let compressed = zstd_compress(&packed)?;
        drop(packed); // free uncompressed before next type

        let mut cols = HashMap::new();
        for (slot, ik) in store.schema().iter() {
            let prop_name = graph.interner.resolve(ik);
            if let Some(col) = store.column(slot as usize) {
                // The *logical* column type. The per-column encoding actually
                // used lives in the section itself (a v6 `Int64` column may be
                // written delta-varint); the loader reads the section's tag and
                // uses these entries only for their key set.
                cols.insert(prop_name.to_string(), col.type_tag().to_string());
            }
        }

        meta_out.push(PortableColumnSection {
            type_name: type_name.to_string(),
            compressed_size: compressed.len() as u64,
            row_count: store.row_count(),
            columns: cols,
        });
        data_out.push(compressed);
    }
    Ok((meta_out, data_out))
}

/// `graph.embeddings` and `graph.timeseries_store` are HashMaps whose
/// per-process RandomState would randomize entry order; serializing through
/// a BTreeMap view keeps the bytes reproducible (same wire shape).
fn encode_ordered_map<K: Ord + Serialize, V: Serialize>(
    map: impl Iterator<Item = (K, V)>,
    is_empty: bool,
    codec: serde_codec::CodecVersion,
) -> io::Result<Option<Vec<u8>>> {
    if is_empty {
        return Ok(None);
    }
    let ordered: BTreeMap<K, V> = map.collect();
    let raw = codec_ser(codec, &ordered)?;
    Ok(Some(zstd_compress(&raw)?))
}

fn encode_optional_sections(
    graph: &DirGraph,
    codec: serde_codec::CodecVersion,
) -> io::Result<OptionalSections> {
    let embeddings =
        encode_ordered_map(graph.embeddings.iter(), graph.embeddings.is_empty(), codec)?;
    let (edge_embeddings, core_version) = encode_portable_edge_embeddings(graph, codec)?;
    let timeseries = encode_ordered_map(
        graph.timeseries_store.iter(),
        graph.timeseries_store.is_empty(),
        codec,
    )?;
    // Hand-rolled binary format — InternedKey doesn't derive serde, and the
    // same layout is reused by the disk sidecar. Single-label graphs skip it.
    let secondary_labels = compress_some(encode_secondary_label_index(graph))?;
    let vector_index = compress_some(encode_vector_indexes(graph)?)?;
    let text_index = compress_some(encode_text_indexes(graph)?)?;
    Ok(OptionalSections {
        embeddings,
        edge_embeddings,
        timeseries,
        secondary_labels,
        vector_index,
        text_index,
        edge_vector_index: encode_edge_vector_index_section(graph)?,
        edge_text_index: encode_edge_text_index_section(graph)?,
        core_version,
    })
}

/// Canonical metadata JSON: round-trips through `serde_json::Value`, whose
/// object map is a BTreeMap, so every nested `HashMap<String, T>` emits with
/// sorted keys. Prevents per-process HashMap randomization from producing
/// different save bytes for the same graph — the byte-level tripwire in
/// `tests/test_phase4_parity.py` depends on this.
fn build_metadata_json(
    graph: &DirGraph,
    checkpoint_lsn: Option<u64>,
    topology: &[u8],
    column_meta: Vec<PortableColumnSection>,
    column_data: &[Vec<u8>],
    optional: &OptionalSections,
) -> io::Result<Vec<u8>> {
    let keys = [
        EMBEDDINGS_SECTION,
        EDGE_EMBEDDINGS_SECTION,
        TIMESERIES_SECTION,
        SECONDARY_LABELS_SECTION,
        VECTOR_INDEX_SECTION,
        TEXT_INDEX_SECTION,
        EDGE_VECTOR_INDEX_SECTION,
        EDGE_TEXT_INDEX_SECTION,
    ];
    let present = optional.in_file_order();
    let digests = build_section_digests(
        topology,
        &column_meta,
        column_data,
        std::array::from_fn(|i| (keys[i], present[i])),
    );
    let mut metadata = FileMetadata::from_graph_version(graph, optional.core_version);
    if let Some(lsn) = checkpoint_lsn {
        metadata.checkpoint_lsn = lsn;
    }
    metadata.section_digests = digests;
    metadata.topology_compressed_size = topology.len() as u64;
    metadata.column_sections = column_meta;
    metadata.embeddings_compressed_size = compressed_len(&optional.embeddings);
    metadata.edge_embeddings_compressed_size = compressed_len(&optional.edge_embeddings);
    metadata.timeseries_compressed_size = compressed_len(&optional.timeseries);
    metadata.secondary_labels_compressed_size = compressed_len(&optional.secondary_labels);
    metadata.vector_index_compressed_size = compressed_len(&optional.vector_index);
    metadata.text_index_compressed_size = compressed_len(&optional.text_index);
    metadata.edge_vector_index_compressed_size = compressed_len(&optional.edge_vector_index);
    metadata.edge_text_index_compressed_size = compressed_len(&optional.edge_text_index);
    let value = serde_json::to_value(&metadata).map_err(io::Error::other)?;
    serde_json::to_vec(&value).map_err(io::Error::other)
}

/// [`write_kgl_to`] with the metadata's `checkpoint_lsn` overridden (`None`
/// keeps the graph's own stamp). Reads the graph only.
pub fn write_kgl_to_stamped<W: Write>(
    graph: &DirGraph,
    writer: &mut W,
    checkpoint_lsn: Option<u64>,
) -> io::Result<()> {
    validate_column_keys_registered(graph)?;
    let codec = serde_codec::CodecVersion::PostcardV1;
    let topology = encode_topology(graph, codec)?;
    let (column_meta, column_data) = encode_column_sections(graph, codec)?;
    let optional = encode_optional_sections(graph, codec)?;
    let core_version = optional.core_version;
    let metadata_json = build_metadata_json(
        graph,
        checkpoint_lsn,
        &topology,
        column_meta,
        &column_data,
        &optional,
    )?;

    // Header: magic (4B) + codec (1B) + core_data_version (4B) +
    // metadata_length (4B). The codec byte prevents implicit byte sniffing.
    writer.write_all(&V7_MAGIC)?;
    writer.write_all(&[codec.tag()])?;
    writer.write_all(&core_version.to_le_bytes())?;
    writer.write_all(&(metadata_json.len() as u32).to_le_bytes())?;
    writer.write_all(&metadata_json)?;
    writer.write_all(&topology)?;
    for section_data in &column_data {
        writer.write_all(section_data)?;
    }
    for section in optional.in_file_order() {
        write_optional_section(writer, section)?;
    }
    // Flush the writer's own buffer; the atomic-save wrapper additionally
    // fsyncs the file. A harmless no-op for an in-memory `Vec<u8>` writer.
    writer.flush()?;
    Ok(())
}
