//! Reading the manifest back (VAULT.md §10.7).

use super::*;

#[test]
fn a_well_formed_manifest_parses() {
    let files = parse("{\"kglite_vault\": 1, \"files\": {\"a.md\": \"ab\"}}").unwrap();
    assert_eq!(files.get("a.md").map(String::as_str), Some("ab"));
}

#[test]
fn every_malformed_manifest_names_what_is_wrong() {
    for (text, expected) in [
        ("not json", "not valid JSON"),
        ("{\"files\": {}}", "no `kglite_vault` version"),
        ("{\"kglite_vault\": 2, \"files\": {}}", "kglite_vault: 2"),
        ("{\"kglite_vault\": 1}", "no `files` object"),
        (
            "{\"kglite_vault\": 1, \"files\": {\"a.md\": 3}}",
            "`files.a.md` is not a hash string",
        ),
    ] {
        let error = parse(text).unwrap_err();
        assert!(error.contains(expected), "{error} does not name {expected}");
    }
}

#[test]
fn the_rendered_manifest_is_sorted_and_newline_terminated() {
    let files = BTreeMap::from([
        ("z.md".to_string(), "1".to_string()),
        ("a.md".to_string(), "2".to_string()),
    ]);
    let text = render(&files);
    assert!(text.ends_with("}\n"), "{text}");
    assert!(text.find("a.md") < text.find("z.md"), "{text}");
    assert_eq!(parse(&text).unwrap(), files);
}
