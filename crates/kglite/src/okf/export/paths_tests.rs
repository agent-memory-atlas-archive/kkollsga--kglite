//! Filename sanitisation (VAULT.md §10.2).

use super::sanitize_segment;

#[test]
fn every_forbidden_character_becomes_a_dash() {
    assert_eq!(
        sanitize_segment("a/b\\c:d*e?f\"g<h>i|j"),
        "a-b-c-d-e-f-g-h-i-j"
    );
    assert_eq!(sanitize_segment("tab\there"), "tab-here");
}

#[test]
fn the_names_a_directory_entry_cannot_be_are_replaced() {
    // Trailing dots and spaces are stripped by Windows on write, so a name
    // ending in one would not be the name the manifest recorded.
    assert_eq!(sanitize_segment("."), "untitled");
    assert_eq!(sanitize_segment(".."), "untitled");
    assert_eq!(sanitize_segment(""), "untitled");
    assert_eq!(sanitize_segment("name. "), "name");
    assert_eq!(sanitize_segment("  lead"), "lead");
}

#[test]
fn an_ordinary_name_is_untouched() {
    assert_eq!(
        sanitize_segment("Seismic interpretation"),
        "Seismic interpretation"
    );
    assert_eq!(sanitize_segment("a.b.c"), "a.b.c");
}
