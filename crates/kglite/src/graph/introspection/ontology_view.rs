//! The `<ontology>` section of `describe()`: the declared semantic layer
//! rendered as XML, split from `describe.rs` (file ceiling).

use super::describe::xml_escape;
use crate::graph::schema::DirGraph;

/// The declared semantic layer, rendered when one exists. `focus` narrows
/// to the classes touching the given types (the class itself, its
/// ancestors/descendants, and relationships whose endpoints land in that
/// set); `None` renders the whole store. Ontology-free graphs emit nothing
/// — the section's absence IS the "no ontology" signal, so no mode or
/// parameter changes for graphs that never declare one.
pub(super) fn write_ontology(xml: &mut String, graph: &DirGraph, focus: Option<&[String]>) {
    let store = &graph.ontology;
    if store.is_empty() {
        return;
    }
    let in_focus = |name: &str| -> bool {
        let Some(types) = focus else { return true };
        types.iter().any(|t| {
            t == name
                || store.ancestors(t).iter().any(|a| a == name)
                || store.ancestors(name).iter().any(|a| a == t)
        })
    };
    xml.push_str(&format!(
        "  <ontology classes=\"{}\" relationships=\"{}\" note=\"declared semantic layer \
         (annotations, not axioms — SKOS in spirit): no-arg rule procedures and CALL \
         ontology_audit() read these declarations; distinct from the parent_types \
         ownership tiering\"{}>\n",
        store.classes.len(),
        store.relationships.len(),
        store
            .store_summary()
            .map_or(String::new(), |summary| format!(
                " store=\"{}\"",
                xml_escape(&summary)
            ))
    ));
    for (name, decl) in &store.classes {
        if !in_focus(name) {
            continue;
        }
        let mut attrs = format!("name=\"{}\"", xml_escape(name));
        if decl.is_abstract {
            attrs.push_str(" abstract=\"true\"");
        }
        if let Some(parent) = &decl.is_a {
            attrs.push_str(&format!(" is_a=\"{}\"", xml_escape(parent)));
        }
        if let Some(nodes) = graph.type_indices.get(name) {
            attrs.push_str(&format!(" count=\"{}\"", nodes.len()));
        }
        if let Some(by) = &decl.by {
            attrs.push_str(&format!(
                " by=\"{} (unenforced discriminator)\"",
                xml_escape(by)
            ));
        }
        if let Some(d) = &decl.description {
            attrs.push_str(&format!(" desc=\"{}\"", xml_escape(d)));
        }
        append_property_contract(&mut attrs, &decl.required_properties, &decl.property_types);
        append_value_contracts(&mut attrs, &decl.property_values);
        attrs.push_str(&format!(" enforcement=\"{}\"", decl.enforcement_summary()));
        xml.push_str(&format!("    <class {attrs}/>\n"));
    }
    for (name, decl) in &store.relationships {
        let touches_focus = focus.is_none()
            || [&decl.domain, &decl.range]
                .into_iter()
                .flatten()
                .any(|endpoint| in_focus(endpoint));
        if !touches_focus {
            continue;
        }
        let mut attrs = format!("name=\"{}\"", xml_escape(name));
        if let Some(d) = &decl.domain {
            attrs.push_str(&format!(" from=\"{}\"", xml_escape(d)));
        }
        if let Some(r) = &decl.range {
            attrs.push_str(&format!(" to=\"{}\"", xml_escape(r)));
        }
        if let Some(inv) = &decl.inverse_name {
            attrs.push_str(&format!(" inverse=\"{}\"", xml_escape(inv)));
        }
        if decl.required {
            attrs.push_str(" required=\"true\"");
        }
        if let Some(card) = &decl.cardinality {
            attrs.push_str(&format!(
                " cardinality=\"{}..{}\"",
                card.min.map_or(String::new(), |v| v.to_string()),
                card.max.map_or(String::new(), |v| v.to_string())
            ));
        }
        if decl.transitive {
            attrs.push_str(" transitive=\"true\"");
        }
        // Carries the idiom, matching the `by="… (unenforced discriminator)"`
        // hint style: the whole value of the annotation to a reader is
        // knowing how to traverse it.
        if decl.ancestry {
            attrs.push_str(" ancestry=\"true (walk with *1..)\"");
        }
        if decl.symmetric {
            attrs.push_str(" symmetric=\"true\"");
        }
        append_property_contract(&mut attrs, &decl.required_properties, &decl.property_types);
        append_value_contracts(&mut attrs, &decl.property_values);
        if let Some(by_source) = decl.by_source_summary() {
            attrs.push_str(&format!(" by_source=\"{}\"", xml_escape(&by_source)));
        }
        if let Some(distinct) = &decl.distinct {
            attrs.push_str(&format!(
                " distinct=\"{}\"",
                xml_escape(&distinct.summary())
            ));
        }
        attrs.push_str(&format!(" enforcement=\"{}\"", decl.enforcement_summary()));
        if let Some(exempt) = decl.exempt_summary() {
            attrs.push_str(&format!(" exempt=\"{}\"", xml_escape(&exempt)));
        }
        if let Some(d) = &decl.description {
            attrs.push_str(&format!(" desc=\"{}\"", xml_escape(d)));
        }
        xml.push_str(&format!("    <relationship {attrs}/>\n"));
    }
    xml.push_str("  </ontology>\n");
}

fn append_value_contracts(
    attrs: &mut String,
    contracts: &std::collections::BTreeMap<String, crate::graph::ontology::ValueContract>,
) {
    if !contracts.is_empty() {
        let fields: Vec<_> = contracts
            .iter()
            .map(|(k, v)| format!("{k}: {}", v.summary()))
            .collect();
        attrs.push_str(&format!(
            " property_values=\"{}\"",
            xml_escape(&fields.join(" | "))
        ));
    }
}

fn append_property_contract(
    attrs: &mut String,
    required: &[String],
    types: &std::collections::BTreeMap<String, String>,
) {
    if !required.is_empty() {
        attrs.push_str(&format!(
            " required_properties=\"{}\"",
            xml_escape(&required.join(", "))
        ));
    }
    if !types.is_empty() {
        let fields: Vec<_> = types.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        attrs.push_str(&format!(
            " property_types=\"{}\"",
            xml_escape(&fields.join(", "))
        ));
    }
}
