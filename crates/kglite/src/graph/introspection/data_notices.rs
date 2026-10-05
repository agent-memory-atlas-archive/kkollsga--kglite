//! Header lines of `describe()` that say something about the loaded data
//! rather than its schema: the load advisories, and a valid-time default that
//! is not the built-in `today`.

use super::describe::xml_escape;
use crate::graph::dir_graph::DirGraph;
use crate::graph::features::temporal::ValidTimeDefault;

/// One `<data-advisory>` line per advisory the load raised, so an agent
/// reading the overview learns the data may carry an older build's defect
/// before it trusts a count; and a `<valid-time-default>` line when an undated
/// read on a graph that declares validity is not as of today. Nothing for a
/// graph with neither.
pub(super) fn write_data_notices(xml: &mut String, graph: &DirGraph) {
    for advisory in &graph.advisories {
        xml.push_str(&format!(
            "  <data-advisory code=\"{}\" writer=\"{}\">{}</data-advisory>\n",
            xml_escape(&advisory.code),
            xml_escape(&advisory.writer),
            xml_escape(&advisory.message)
        ));
    }
    let (effective, stored) = (graph.valid_time_default, graph.stored_valid_time_default);
    if !graph.temporal.is_empty()
        && (effective != ValidTimeDefault::Today || stored != ValidTimeDefault::Today)
    {
        xml.push_str(&format!(
            "  <valid-time-default effective=\"{effective}\" stored=\"{stored}\">Undated \
             reads on this graph use the effective default; FOR VALID_TIME AS OF / ALL names \
             another instant.</valid-time-default>\n"
        ));
    }
}
