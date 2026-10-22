//! The `CYPHER` statement prefix at the Bolt layer: looked through for the
//! server's own verbs.

use super::intercepts::{
    components_pipeline_rewrite, parse_checkpoint_call, parse_server_facts_call,
    strip_cypher_prefix,
};

/// A `CYPHER 5` / `CYPHER 25` prefix (and `name=value` options) is looked
/// through for the verb recognizers; anything else is returned untouched.
#[test]
fn strip_cypher_prefix_exposes_the_statement_to_the_recognizers() {
    for (input, rest) in [
        ("CYPHER 5 CALL dbms.components()", "CALL dbms.components()"),
        ("cypher 25 SHOW DATABASES", "SHOW DATABASES"),
        (
            "CYPHER 25 runtime=slotted  CALL db.checkpoint()",
            "CALL db.checkpoint()",
        ),
        ("CYPHER planner=cost RETURN 1", "RETURN 1"),
        ("CALL dbms.components()", "CALL dbms.components()"),
        ("CYPHERS 5 RETURN 1", "CYPHERS 5 RETURN 1"),
    ] {
        assert_eq!(strip_cypher_prefix(input), rest, "{input:?}");
    }
    assert!(
        parse_server_facts_call(strip_cypher_prefix("CYPHER 5 CALL dbms.components()")).is_some()
    );
    assert!(parse_checkpoint_call(strip_cypher_prefix("CYPHER 25 CALL db.checkpoint()")).is_some());
    assert!(components_pipeline_rewrite(
        strip_cypher_prefix("CYPHER 25 CALL dbms.components() YIELD name RETURN name"),
        &[("N".to_string(), vec!["1.0".to_string()])],
        "community"
    )
    .is_some());
}
