//! The `kg:` RDF vocabulary — the one place the RDF importer and the RDF
//! exporter agree on IRIs.
//!
//! An RDF export carries its [`ExportManifest`](super::ExportManifest) as a
//! single statement, `<base>meta kg:manifest "<manifest JSON>"^^kg:json`, in
//! the named graph `<base>meta`. The importer recognises it by predicate, in
//! whichever graph it appears, and never folds a `kg:` statement into the
//! property graph.
//!
//! Data IRIs follow one layout under `<base>` — `node/<Type>/<id>`,
//! `type/<Type>`, `prop/<name>`, `rel/<TYPE>` — with each variable segment
//! percent-encoded by [`encode_segment`]. The layout is read back through the
//! importer's fragment fallback (the last `/`-separated segment names a type,
//! property or relationship), so an export must declare no prefixes whose
//! namespace would compact those IRIs to `prefix__name`.
//!
//! A relationship's own properties travel on a reifier: `r rdf:reifies <<( s p
//! o )>>` plus `r <prop> value`. Emit the plain `s p o` statement once per
//! edge and a reifier only for edges that carry properties; the importer
//! creates `max(plain statements, reifiers)` edges per `(s, p, o)`.

/// Namespace of the `kg:` vocabulary.
pub const KG_NS: &str = "https://kglite.readthedocs.io/ns/kg#";

/// Predicate whose object is the manifest JSON.
pub const KG_MANIFEST: &str = "https://kglite.readthedocs.io/ns/kg#manifest";

/// Datatype of a literal holding JSON: a `List` or `Map` value, or the manifest.
pub const KG_JSON: &str = "https://kglite.readthedocs.io/ns/kg#json";

// Exporter half of the layout: the importer reads only node ids and the manifest.
#[allow(dead_code)]
/// Path segment, after `<base>`, of the named graph holding the manifest.
pub const META_GRAPH: &str = "meta";

pub const NODE_PATH: &str = "node/";
// Exporter half of the layout: the importer does not read it.
#[allow(dead_code)]
pub const TYPE_PATH: &str = "type/";
// Exporter half of the layout: the importer does not read it.
#[allow(dead_code)]
pub const PROP_PATH: &str = "prop/";
// Exporter half of the layout: the importer does not read it.
#[allow(dead_code)]
pub const REL_PATH: &str = "rel/";

/// `rdf:reifies`, the predicate tying a reifier to its triple term.
pub const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";

// Exporter half of the layout: the importer does not read it.
#[allow(dead_code)]
/// Percent-encode everything outside the RFC 3986 unreserved set, so a
/// segment never contains the `/` the layout splits on.
pub fn encode_segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte))
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Reverse of [`encode_segment`]; `None` when the escapes are malformed or
/// the bytes are not UTF-8.
pub fn decode_segment(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The decoded `<id>` of a node IRI `<base>node/<Type>/<id>` of `node_type`.
pub fn node_id_segment(iri: &str, node_type: &str) -> Option<String> {
    let (head, id) = iri.rsplit_once('/')?;
    let (head, ty) = head.rsplit_once('/')?;
    if !head.ends_with(NODE_PATH.trim_end_matches('/')) || decode_segment(ty)? != node_type {
        return None;
    }
    decode_segment(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_round_trip() {
        for text in ["plain", "a/b c", "雪 snow", "100%", ""] {
            assert_eq!(decode_segment(&encode_segment(text)).as_deref(), Some(text));
        }
        assert!(!encode_segment("a/b").contains('/'));
        assert_eq!(decode_segment("%zz"), None);
    }

    #[test]
    fn node_id_reads_only_the_matching_layout() {
        let iri = "http://e.org/node/Person/42";
        assert_eq!(node_id_segment(iri, "Person").as_deref(), Some("42"));
        assert_eq!(node_id_segment(iri, "Department"), None);
        assert_eq!(node_id_segment("http://e.org/other/Person/42", "Person"), None);
    }
}
