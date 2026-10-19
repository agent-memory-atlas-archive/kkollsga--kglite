//! Hand-written trait impls for `NodeData` and `EdgeData`: the serde pairs
//! that pin their `.kgl` wire shapes independently of the in-memory layout.

use super::*;

// Stable wire shape: the positional `(id, title, node_type, properties)` tuple
// every `.kgl` file carries. `ident` is flattened back into the two leading
// fields (the sentinel for `None`), so files written before the boxed layout
// load unchanged, including ones holding a real inline id/title.
impl Serialize for NodeData {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("NodeData", 4)?;
        s.serialize_field("id", self.id_ref())?;
        s.serialize_field("title", self.title_ref())?;
        s.serialize_field("node_type", &self.node_type)?;
        s.serialize_field("properties", &self.properties)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for NodeData {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename = "NodeData")]
        struct NodeDataHelper {
            id: Value,
            title: Value,
            node_type: InternedKey,
            properties: PropertyStorage,
        }
        let h = NodeDataHelper::deserialize(deserializer)?;
        Ok(NodeData {
            ident: NodeData::pack_ident(h.id, h.title),
            node_type: h.node_type,
            properties: h.properties,
        })
    }
}

// Stable Serde struct shape: connection_type as an InternedKey (auto-resolves
// to a string), properties as a map (backward-compatible with the old format).
impl Serialize for EdgeData {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("EdgeData", 2)?;
        s.serialize_field("connection_type", &self.connection_type)?;
        // Serde maps have the same backward-compatible wire shape regardless
        // of their Rust container. Sort by the persisted InternedKey value so
        // equivalent property vectors always produce identical `.kgl` bytes.
        let props_map: BTreeMap<&InternedKey, &Value> =
            self.properties.iter().map(|(k, v)| (k, v)).collect();
        s.serialize_field("properties", &props_map)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for EdgeData {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct EdgeDataHelper {
            connection_type: InternedKey,
            #[serde(default)]
            properties: HashMap<InternedKey, Value>,
        }
        let helper = EdgeDataHelper::deserialize(deserializer)?;
        Ok(EdgeData {
            connection_type: helper.connection_type,
            properties: helper.properties.into_iter().collect(),
        })
    }
}

impl std::fmt::Debug for EdgeData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeData")
            .field("connection_type", &self.connection_type)
            .field("properties", &self.properties)
            .finish()
    }
}

impl Clone for EdgeData {
    fn clone(&self) -> Self {
        EdgeData {
            connection_type: self.connection_type,
            properties: self.properties.clone(),
        }
    }
}
