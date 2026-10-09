//! Map kglite's typed [`KgError`] / [`KgErrorCode`] onto Bolt FAILURE
//! status codes (`Neo.{Class}.{Category}.{Title}` strings).
//!
//! The same typed hierarchy backs the Python boundary
//! (`kglite.CypherSyntaxError`, `kglite.CypherTimeoutError`, etc.);
//! this module wires it to the Bolt wire so the neo4j Python driver
//! raises the matching driver-side exception class
//! (`CypherSyntaxError` instead of generic `ClientError`).
//!
//! ## Mapping table
//!
//! One row per [`KgErrorCode`](kglite::api::KgErrorCode) variant. The status
//! strings are owned by `KgErrorCode::neo4j_status_code`; the
//! `table_documents_every_code_and_its_status` test fails if a variant or a
//! status is missing from, or disagrees with, this table. The class prefix is
//! what a driver routes on: a `TransientError` is retried by a managed
//! transaction, a `ClientError` is the caller's to fix, a `DatabaseError` is
//! the server's.
//!
//! | `KgErrorCode`             | Neo4j status code                                   | Class          |
//! |---------------------------|-----------------------------------------------------|----------------|
//! | `CypherSyntax`            | `Neo.ClientError.Statement.SyntaxError`             | ClientError    |
//! | `CypherTimeout`           | `Neo.ClientError.Transaction.TransactionTimedOut`   | ClientError    |
//! | `CypherExecution`         | `Neo.ClientError.Statement.ArgumentError`           | ClientError    |
//! | `CypherTypeMismatch`      | `Neo.ClientError.Statement.TypeError`               | ClientError    |
//! | `Cancelled`               | `Neo.ClientError.Transaction.Terminated`            | ClientError    |
//! | `Schema`                  | `Neo.ClientError.Schema.ConstraintValidationFailed` | ClientError    |
//! | `Validation`              | `Neo.ClientError.Statement.ArgumentError`           | ClientError    |
//! | `Expr`                    | `Neo.ClientError.Statement.ArgumentError`           | ClientError    |
//! | `ConstraintViolation`     | `Neo.ClientError.Schema.ConstraintValidationFailed` | ClientError    |
//! | `ConstraintCreationFailed`| `Neo.ClientError.Schema.ConstraintCreationFailed`   | ClientError    |
//! | `OntologyViolation`       | `Neo.ClientError.Schema.ConstraintValidationFailed` | ClientError    |
//! | `TransactionConflict`     | `Neo.TransientError.Transaction.Outdated`           | TransientError |
//! | `DurabilityFailed`        | `Neo.DatabaseError.General.UnknownError`            | DatabaseError  |
//! | `WriterLeaseHeld`         | `Neo.TransientError.General.DatabaseUnavailable`    | TransientError |
//! | `ReadOnly`                | `Neo.ClientError.General.ReadOnly`                  | ClientError    |
//! | `NodeNotFound`            | `Neo.ClientError.Statement.EntityNotFound`          | ClientError    |
//! | `ConnectionNotFound`      | `Neo.ClientError.Statement.EntityNotFound`          | ClientError    |
//! | `PropertyNotFound`        | `Neo.ClientError.Statement.EntityNotFound`          | ClientError    |
//! | `FileNotFound`            | `Neo.DatabaseError.General.UnknownError`            | DatabaseError  |
//! | `FileFormat`              | `Neo.DatabaseError.General.UnknownError`            | DatabaseError  |
//! | `FileIo`                  | `Neo.DatabaseError.General.UnknownError`            | DatabaseError  |
//! | `LoadMemoryLimit`         | `Neo.TransientError.General.OutOfMemoryError`       | TransientError |
//! | `InvalidArgument`         | `Neo.ClientError.Statement.ArgumentError`           | ClientError    |
//! | `MissingArgument`         | `Neo.ClientError.Statement.ParameterMissing`        | ClientError    |
//! | `Internal`                | `Neo.DatabaseError.General.UnknownError`            | DatabaseError  |
//!
//! `OntologyViolation` shares `ConstraintViolation`'s status so a driver treats
//! both as a constraint failure, which a status string alone cannot tell
//! apart. A FAILURE carries only `code` and `message`, so the message starts
//! with a stable structured prefix:
//!
//! ```text
//! [kglite.OntologyViolation rule=<rule> entity=<node|relationship> type="<label or type>" property=<"name"|null>] <message>
//! ```
//!
//! `rule` is `required_property`, `property_type`, `closed_labels`, `domain`,
//! `range` (or `declaration` for a refused declaration with no entries);
//! `type` and `property` are JSON-quoted strings. The prefix is present on
//! every `OntologyViolation` FAILURE and on no other; a refused declaration's
//! per-rule breakdown stays in the readable message after it.
//!
//! `ReadOnly` is `General.ReadOnly`, not `General.ForbiddenOnReadOnlyDatabase`:
//! the drivers class the latter as a *transient* routing signal and retry a
//! managed transaction against it, so a write to a `--readonly` server would
//! loop for the driver's retry window instead of failing once.
//! `General.ReadOnly` is a permanent `ClientError` (the drivers' `Forbidden`).
//!
//! ## The server's own policy refusals
//!
//! Not every FAILURE comes from a `KgError`: the backend refuses some requests
//! before the engine sees them, using `BoltError` variants directly (boltr's
//! `to_failure_metadata` owns those codes). The split is deliberate — a driver
//! routes on the class, so a refusal a client can fix must never arrive as a
//! `DatabaseError`.
//!
//! | Refusal                                                        | `BoltError`  | Neo4j status code                       | Class         |
//! |----------------------------------------------------------------|--------------|------------------------------------------|---------------|
//! | `--readonly` write, transaction or `db.checkpoint()`            | `Query` via [`read_only_refusal`] | `Neo.ClientError.General.ReadOnly` | ClientError |
//! | Permission: disk-mode `db.checkpoint()`, `db.backup()` refusals  | `Forbidden`  | `Neo.ClientError.Security.Forbidden`     | ClientError   |
//! | Request shape: `tx_timeout`, zoned params                        | `Session` / `Protocol` | `Neo.ClientError.Request.Invalid` | ClientError   |
//! | Server fault: a commit the WAL rejected, an unreachable outcome | `Backend`    | `Neo.DatabaseError.General.UnknownError` | DatabaseError |
//!
//! ## Startup
//!
//! A second writable server on a leased graph does not reach the wire: it exits
//! at startup with a `KgError::WriterLeaseHeld` (class `WriterLeaseHeld`, naming
//! the holder) as the root cause of the error chain.

use boltr::error::BoltError;
use kglite::api::KgError;

/// Map a [`KgError`] to a [`BoltError::Query`] with the right
/// `Neo.{Class}.{Category}.{Title}` code. boltr's
/// `BoltError::to_failure_metadata` passes the code+message through
/// to the wire FAILURE response, where the driver routes by class
/// prefix (ClientError vs DatabaseError vs TransientError).
///
/// The Neo4j status-code dispatch table itself lives on
/// [`kglite::api::KgErrorCode::neo4j_status_code`] (lifted from
/// this module in 2026-05-25 so any future Neo4j-wire-compatible
/// binding shares the canonical mapping). This wrapper just bolts
/// the code into the protocol-level `BoltError::Query` shape.
pub fn kg_to_bolt(err: KgError) -> BoltError {
    BoltError::Query {
        code: err.code().neo4j_status_code().into(),
        message: wire_message(&err),
    }
}

/// A `--readonly` refusal, published under the same status as every other
/// read-only refusal (`KgErrorCode::ReadOnly`).
pub fn read_only_refusal(message: &str) -> BoltError {
    kg_to_bolt(KgError::read_only(message))
}

/// The FAILURE message: the error's own text, led by the structured prefix
/// documented in the module docs for an `OntologyViolation`.
fn wire_message(err: &KgError) -> String {
    match err {
        KgError::OntologyViolation {
            rule,
            entity,
            entity_type,
            property,
            message,
            ..
        } => format!(
            "[kglite.OntologyViolation rule={rule} entity={entity} type={} property={}] {message}",
            json_string(entity_type),
            property.as_deref().map_or("null".to_string(), json_string),
        ),
        other => other.to_string(),
    }
}

/// `s` as a JSON string literal, so a label containing a space, bracket or
/// quote cannot break the prefix's grammar.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kglite::api::KgErrorCode;

    #[test]
    fn syntax_error_maps_to_neo_clienterror_statement_syntaxerror() {
        let err = KgError::CypherSyntax {
            message: "unexpected token 'NOT'".into(),
            line: Some(1),
            col: Some(7),
        };
        let bolt = kg_to_bolt(err);
        match bolt {
            BoltError::Query { code, .. } => {
                assert_eq!(code, "Neo.ClientError.Statement.SyntaxError");
            }
            other => panic!("expected Query, got {other:?}"),
        }
    }

    #[test]
    fn an_execution_failure_is_published_as_a_client_error() {
        // A statement that failed on its inputs (a malformed `valid_at` date,
        // an undeclared type) is the client's to fix; a `DatabaseError` class
        // told drivers and retry logic the server broke.
        let bolt = kg_to_bolt(KgError::CypherExecution {
            message: "valid_at(): the date argument 'garbage' is not a date".into(),
            position: None,
        });
        match bolt {
            BoltError::Query { code, .. } => {
                assert_eq!(code, "Neo.ClientError.Statement.ArgumentError");
            }
            other => panic!("expected Query, got {other:?}"),
        }
    }

    #[test]
    fn transaction_conflict_is_published_in_the_retriable_class() {
        // Neo4j drivers decide whether to retry a managed transaction from
        // the *class prefix* alone (`Neo.TransientError.*`), not the title —
        // so the prefix, not the exact string, is the contract that makes
        // `session.execute_write` re-run a unit of work that lost an OCC
        // race. A `ClientError`-class code here silently turns every
        // conflict into a caller-visible failure with a lost write.
        let code = KgErrorCode::TransactionConflict.neo4j_status_code();
        assert!(
            code.starts_with("Neo.TransientError."),
            "OCC conflicts must be published in the driver-retriable class, got: {code}"
        );
    }

    #[test]
    fn every_code_has_a_neo4j_string_starting_with_neo_dot() {
        for &code in KgErrorCode::ALL {
            let s = code.neo4j_status_code();
            assert!(
                s.starts_with("Neo."),
                "code {:?} mapped to non-Neo.* string: {}",
                code,
                s
            );
            // All Neo4j codes have exactly 4 dotted segments.
            assert_eq!(
                s.split('.').count(),
                4,
                "code {:?} mapped to wrong-shaped string: {} (want 4 dotted segments)",
                code,
                s
            );
        }
    }

    /// The module-doc table is the user-facing statement of the mapping, so it
    /// is held to the match arms: every variant of `KgErrorCode::ALL` has a row
    /// naming it, and that row carries exactly the status the code maps to and
    /// the class prefix of that status. A new code that is mapped but not
    /// documented, or documented under the wrong status, fails here.
    #[test]
    fn table_documents_every_code_and_its_status() {
        let source = include_str!("error_map.rs");
        let rows: Vec<&str> = source
            .lines()
            .filter(|l| l.starts_with("//! | `"))
            .collect();
        for &code in KgErrorCode::ALL {
            let status = code.neo4j_status_code();
            let class = status.split('.').nth(1).unwrap();
            let name = format!("`{}`", code.as_str());
            let row = rows
                .iter()
                .find(|row| {
                    row.split('|')
                        .nth(1)
                        .is_some_and(|first| first.trim() == name)
                })
                .unwrap_or_else(|| panic!("no table row for {name}"));
            assert!(
                row.contains(&format!("`{status}`")),
                "{name}: the table row does not carry its status {status}: {row}"
            );
            assert!(
                row.trim_end()
                    .trim_end_matches('|')
                    .trim_end()
                    .ends_with(class),
                "{name}: the table row does not end with its class {class}: {row}"
            );
        }
    }

    fn ontology_sample() -> KgError {
        KgError::OntologyViolation {
            rule: "required_property",
            entity: "node",
            entity_type: "T".into(),
            property: Some("p".into()),
            message: "m".into(),
            report: Vec::new(),
        }
    }

    /// `kg_to_bolt` publishes the core status for the codes this crate
    /// produces itself and for the two new identities.
    #[test]
    fn kg_to_bolt_publishes_the_core_status_for_the_lease_and_read_only_codes() {
        let lease = KgError::WriterLeaseHeld {
            message: "held".into(),
            holder: kglite::api::io::LeaseHolder::default(),
        };
        for (err, status) in [
            (lease, "Neo.TransientError.General.DatabaseUnavailable"),
            (KgError::read_only("ro"), "Neo.ClientError.General.ReadOnly"),
            (
                KgError::DurabilityFailed {
                    message: "m".into(),
                },
                "Neo.DatabaseError.General.UnknownError",
            ),
        ] {
            let BoltError::Query { code, .. } = kg_to_bolt(err) else {
                panic!("Query");
            };
            assert_eq!(code, status);
        }
    }

    #[test]
    fn an_ontology_violation_is_told_from_a_constraint_violation_by_its_prefix() {
        let ontology = kg_to_bolt(ontology_sample());
        let constraint = kg_to_bolt(KgError::ConstraintViolation {
            kind: "UNIQUE",
            node_type: "T".into(),
            properties: vec!["p".into()],
            descriptor: "T.p".into(),
            message: "m".into(),
        });
        let (
            BoltError::Query {
                code: oc,
                message: om,
            },
            BoltError::Query {
                code: cc,
                message: cm,
            },
        ) = (ontology, constraint)
        else {
            panic!("both map to Query");
        };
        assert_eq!(oc, cc, "they share the Neo4j status");
        assert_eq!(
            om,
            "[kglite.OntologyViolation rule=required_property entity=node type=\"T\" property=\"p\"] m"
        );
        assert!(!cm.starts_with("[kglite."), "{cm}");
    }

    #[test]
    fn the_prefix_quotes_awkward_labels_and_null_properties() {
        let err = KgError::OntologyViolation {
            rule: "domain",
            entity: "relationship",
            entity_type: "has \"] space".into(),
            property: None,
            message: "m".into(),
            report: Vec::new(),
        };
        let BoltError::Query { message, .. } = kg_to_bolt(err) else {
            panic!("Query");
        };
        assert_eq!(
            message,
            "[kglite.OntologyViolation rule=domain entity=relationship \
             type=\"has \\\"] space\" property=null] m"
        );
    }

    #[test]
    fn read_only_refusal_is_the_core_read_only_status() {
        let BoltError::Query { code, message } = read_only_refusal("nope") else {
            panic!("Query");
        };
        assert_eq!(code, "Neo.ClientError.General.ReadOnly");
        assert_eq!(message, "nope");
    }
}
