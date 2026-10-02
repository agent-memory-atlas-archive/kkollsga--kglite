use super::*;

#[test]
fn durations_use_one_sign_and_split_years() {
    assert_eq!(
        duration_lexical(18, 2, 10800).as_deref(),
        Some("P1Y6M2DT10800S")
    );
    assert_eq!(duration_lexical(0, 3, 0).as_deref(), Some("P3D"));
    assert_eq!(duration_lexical(0, 0, 0).as_deref(), Some("PT0S"));
    assert_eq!(duration_lexical(0, -2, 0).as_deref(), Some("-P2D"));
    assert_eq!(duration_lexical(0, 0, -90).as_deref(), Some("-PT90S"));
    assert_eq!(duration_lexical(1, -1, 0), None);
}

#[test]
fn formats_parse_from_names_and_extensions() {
    assert_eq!(RdfFormat::parse("NQ"), Some(RdfFormat::NQuads));
    assert_eq!(RdfFormat::from_path("a/b.trig"), Some(RdfFormat::TriG));
    assert_eq!(RdfFormat::from_path("a/b.ttl"), None);
}

#[test]
fn a_base_must_be_absolute_slash_terminated_and_outside_well_known_namespaces() {
    assert!(validate_base(DEFAULT_BASE).is_ok());
    assert!(validate_base("https://e.org/ns").is_err());
    assert!(validate_base("http://xmlns.com/foaf/0.1/").is_err());
    assert!(validate_base("not an iri/").is_err());
}
