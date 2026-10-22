//! Neo4j's positional `CALL` form: `CALL proc(a, b, c)`.
//!
//! Every procedure here takes one config map (`CALL proc({key: value})`);
//! the procedures that mirror a Neo4j signature additionally declare their
//! positional parameter order in [`SIGNATURES`], and the parser rewrites a
//! positional call into the same keyed map the executor already reads. A
//! procedure with no entry refuses positional arguments naming the ones that
//! accept them, so the map form stays the single spelling everywhere else.

use crate::graph::languages::cypher::ast::Expression;

/// One positional parameter: the config key it binds to and its Neo4j type.
pub(crate) struct PositionalParam {
    pub key: &'static str,
    pub type_name: &'static str,
}

/// A procedure's positional signature. The first `required` parameters must
/// be supplied; the rest may be left off, like Neo4j's `options = {}`.
pub(crate) struct PositionalSignature {
    pub name: &'static str,
    pub params: &'static [PositionalParam],
    pub required: usize,
}

const fn param(key: &'static str, type_name: &'static str) -> PositionalParam {
    PositionalParam { key, type_name }
}

const FULLTEXT_PARAMS: &[PositionalParam] = &[
    param("indexName", "STRING"),
    param("queryString", "STRING"),
    param("options", "MAP"),
];

const VECTOR_PARAMS: &[PositionalParam] = &[
    param("indexName", "STRING"),
    param("numberOfNearestNeighbours", "INTEGER"),
    param("query", "LIST<FLOAT>"),
];

pub(crate) const SIGNATURES: &[PositionalSignature] = &[
    PositionalSignature {
        name: "db.index.fulltext.queryNodes",
        params: FULLTEXT_PARAMS,
        required: 2,
    },
    PositionalSignature {
        name: "db.index.fulltext.queryRelationships",
        params: FULLTEXT_PARAMS,
        required: 2,
    },
    PositionalSignature {
        name: "db.index.vector.queryNodes",
        params: VECTOR_PARAMS,
        required: 3,
    },
    PositionalSignature {
        name: "db.index.vector.queryRelationships",
        params: VECTOR_PARAMS,
        required: 3,
    },
];

/// The positional signature of `name` (any case, optional `kglite.` prefix).
pub(crate) fn positional_signature(name: &str) -> Option<&'static PositionalSignature> {
    let name = name
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("kglite."))
        .map_or(name, |_| &name[7..]);
    SIGNATURES
        .iter()
        .find(|signature| signature.name.eq_ignore_ascii_case(name))
}

impl PositionalSignature {
    /// `name(indexName :: STRING, ..., options = {} :: MAP)`, the input half
    /// of a `SHOW PROCEDURES` signature.
    pub(crate) fn input_text(&self) -> String {
        let params: Vec<String> = self
            .params
            .iter()
            .enumerate()
            .map(|(position, p)| {
                if position < self.required {
                    format!("{} :: {}", p.key, p.type_name)
                } else {
                    format!("{} = {{}} :: {}", p.key, p.type_name)
                }
            })
            .collect();
        format!("{}({})", self.name, params.join(", "))
    }
}

/// Bind positional arguments to their config keys, or explain why they cannot
/// be. Called by the parser once it has read `CALL name(` and seen an
/// argument that does not open a map.
pub(crate) fn bind_positional_arguments(
    procedure_name: &str,
    args: Vec<Expression>,
) -> Result<Vec<(String, Expression)>, String> {
    let Some(signature) = positional_signature(procedure_name) else {
        let accepting: Vec<&str> = SIGNATURES.iter().map(|s| s.name).collect();
        return Err(format!(
            "CALL parameters must use map syntax: CALL {procedure_name}({{key: value, ...}}). \
             Positional arguments are accepted only by {}.",
            accepting.join(", ")
        ));
    };
    if args.len() < signature.required || args.len() > signature.params.len() {
        return Err(format!(
            "CALL {procedure_name} takes {} but {} argument{} given. Signature: {}",
            if signature.required == signature.params.len() {
                format!("{} arguments", signature.required)
            } else {
                format!(
                    "{} to {} arguments",
                    signature.required,
                    signature.params.len()
                )
            },
            args.len(),
            if args.len() == 1 { " was" } else { "s were" },
            signature.input_text()
        ));
    }
    Ok(signature
        .params
        .iter()
        .zip(args)
        .map(|(p, expr)| (p.key.to_string(), expr))
        .collect())
}
