//! The Cypher value-type vocabulary behind `x IS :: TYPE` and `valueType(x)`.
//!
//! [`TypeExpr`] is the *expression* counterpart of the constraint vocabulary in
//! [`crate::graph::property_types`]: every scalar name the two share resolves
//! and matches through [`DeclaredType`], so `REQUIRE n.p IS :: INTEGER` and
//! `n.p IS :: INTEGER` cannot disagree about what an integer is. The
//! expression form additionally knows the structural types (`LIST<T>`, `MAP`,
//! `NODE`, `RELATIONSHIP`, `PATH`), the top and bottom types (`ANY`,
//! `PROPERTY VALUE`, `NOTHING`, `NULL`), unions (`A | B`) and the `NOT NULL`
//! suffix. Types are nullable unless `NOT NULL` is written, so
//! `null IS :: INTEGER` is true and `null IS :: INTEGER NOT NULL` is false.
//!
//! A [`TypeExpr`] round-trips through its [`Display`](fmt::Display) form: the
//! parser validates the words written after `IS ::` once and carries the
//! canonical spelling in the query tree, and the executor re-reads it per row.

use std::collections::BTreeSet;
use std::fmt;

use crate::datatypes::values::Value;
use crate::graph::property_types::DeclaredType;

/// One alternative of a type: a kind plus whether `NULL` is excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeTerm {
    kind: TypeKind,
    not_null: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TypeKind {
    Any,
    PropertyValue,
    Nothing,
    Null,
    Declared(DeclaredType),
    Map,
    Node,
    Relationship,
    Path,
    List(Box<TypeExpr>),
}

/// A parsed type: the union of its terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeExpr(Vec<TypeTerm>);

/// Words that spell a type on their own, beyond what
/// [`DeclaredType::resolve`] accepts. Neo4j's synonyms are included.
fn base_kind(phrase: &str) -> Option<TypeKind> {
    if let Some(declared) = DeclaredType::resolve(phrase) {
        return Some(TypeKind::Declared(declared));
    }
    Some(match phrase {
        "INT" | "SIGNED INTEGER" => TypeKind::Declared(DeclaredType::Integer),
        "BOOL" => TypeKind::Declared(DeclaredType::Boolean),
        "TIME WITHOUT TIME ZONE" => TypeKind::Declared(DeclaredType::LocalTime),
        "TIME WITH TIME ZONE" => TypeKind::Declared(DeclaredType::ZonedTime),
        "TIMESTAMP WITHOUT TIME ZONE" => TypeKind::Declared(DeclaredType::LocalDateTime),
        "TIMESTAMP WITH TIME ZONE" => TypeKind::Declared(DeclaredType::ZonedDateTime),
        "MAP" => TypeKind::Map,
        "NODE" | "VERTEX" => TypeKind::Node,
        "RELATIONSHIP" | "EDGE" => TypeKind::Relationship,
        "PATH" => TypeKind::Path,
        "ANY" | "ANY VALUE" => TypeKind::Any,
        "PROPERTY VALUE" | "ANY PROPERTY VALUE" => TypeKind::PropertyValue,
        "NOTHING" => TypeKind::Nothing,
        "NULL" => TypeKind::Null,
        _ => return None,
    })
}

/// Longest phrase `base_kind` may consume (`TIMESTAMP WITHOUT TIME ZONE`).
const MAX_PHRASE_WORDS: usize = 5;

/// Spellings listed in the "unknown type" message.
const TYPE_NAMES: &str = "BOOLEAN, STRING, INTEGER, FLOAT, DATE, LOCAL TIME, ZONED TIME, \
LOCAL DATETIME, ZONED DATETIME, DURATION, POINT, LIST<T>, MAP, NODE, RELATIONSHIP, PATH, ANY, \
PROPERTY VALUE, NOTHING, NULL";

impl TypeExpr {
    /// Parse the words of a type expression: `INTEGER`, `LIST<STRING NOT NULL>`,
    /// `INTEGER | FLOAT`, `local datetime`. Case and spacing are free.
    pub fn parse(text: &str) -> Result<Self, String> {
        let tokens = lex(text);
        let mut at = 0;
        let expr = parse_union(&tokens, &mut at, 0)?;
        match tokens.get(at) {
            None => Ok(expr),
            Some(extra) => Err(format!(
                "unexpected '{extra}' in type expression '{}'",
                text.trim()
            )),
        }
    }

    /// Whether `value` is of this type.
    pub fn matches(&self, value: &Value) -> bool {
        self.0.iter().any(|term| term.matches(value))
    }

    /// Neo4j's `valueType()` string for a value.
    pub fn of_value(value: &Value) -> Self {
        match value {
            Value::Null => TypeExpr(vec![TypeTerm {
                kind: TypeKind::Null,
                not_null: false,
            }]),
            Value::List(items) => {
                let inner = element_type(items);
                TypeExpr(vec![TypeTerm {
                    kind: TypeKind::List(Box::new(inner)),
                    not_null: true,
                }])
            }
            other => TypeExpr(vec![TypeTerm {
                kind: scalar_kind(other),
                not_null: true,
            }]),
        }
    }
}

fn scalar_kind(value: &Value) -> TypeKind {
    match value {
        Value::Boolean(_) => TypeKind::Declared(DeclaredType::Boolean),
        Value::String(_) => TypeKind::Declared(DeclaredType::String),
        Value::Int64(_) | Value::UniqueId(_) => TypeKind::Declared(DeclaredType::Integer),
        Value::Float64(_) => TypeKind::Declared(DeclaredType::Float),
        Value::DateTime(_) => TypeKind::Declared(DeclaredType::Date),
        Value::Timestamp(_) => TypeKind::Declared(DeclaredType::LocalDateTime),
        Value::Duration { .. } => TypeKind::Declared(DeclaredType::Duration),
        Value::Point { .. } => TypeKind::Declared(DeclaredType::Point),
        Value::LocalTime(_) => TypeKind::Declared(DeclaredType::LocalTime),
        Value::Time { .. } => TypeKind::Declared(DeclaredType::ZonedTime),
        Value::ZonedDateTime { .. } => TypeKind::Declared(DeclaredType::ZonedDateTime),
        Value::Map(_) => TypeKind::Map,
        Value::Node(_) | Value::NodeRef(_) => TypeKind::Node,
        Value::Relationship(_) => TypeKind::Relationship,
        Value::Path(_) => TypeKind::Path,
        Value::Null => TypeKind::Null,
        Value::List(items) => TypeKind::List(Box::new(element_type(items))),
    }
}

/// The element type of a list literal's value: the union of its elements'
/// types, nullable when any element is null, `NOTHING` for an empty list.
fn element_type(items: &[Value]) -> TypeExpr {
    let mut has_null = false;
    let mut kinds: Vec<TypeTerm> = Vec::new();
    for item in items {
        let term = match item {
            Value::Null => {
                has_null = true;
                continue;
            }
            Value::List(inner) => TypeTerm {
                kind: TypeKind::List(Box::new(element_type(inner))),
                not_null: true,
            },
            other => TypeTerm {
                kind: scalar_kind(other),
                not_null: true,
            },
        };
        if !kinds.contains(&term) {
            kinds.push(term);
        }
    }
    if kinds.is_empty() {
        let kind = if has_null {
            TypeKind::Null
        } else {
            TypeKind::Nothing
        };
        return TypeExpr(vec![TypeTerm {
            kind,
            not_null: false,
        }]);
    }
    if has_null {
        for term in &mut kinds {
            term.not_null = false;
        }
    }
    kinds.sort_by_key(|term| term.to_string());
    TypeExpr(kinds)
}

impl TypeTerm {
    fn matches(&self, value: &Value) -> bool {
        if matches!(value, Value::Null) {
            return !self.not_null;
        }
        match &self.kind {
            TypeKind::Any => true,
            TypeKind::Nothing | TypeKind::Null => false,
            TypeKind::PropertyValue => is_property_value(value),
            TypeKind::Declared(declared) => declared.accepts(value),
            TypeKind::Map => matches!(value, Value::Map(_)),
            TypeKind::Node => matches!(value, Value::Node(_) | Value::NodeRef(_)),
            TypeKind::Relationship => matches!(value, Value::Relationship(_)),
            TypeKind::Path => matches!(value, Value::Path(_)),
            TypeKind::List(inner) => match value {
                Value::List(items) => items.iter().all(|item| inner.matches(item)),
                _ => false,
            },
        }
    }
}

/// A value a node or relationship property may hold: a scalar, or a list of
/// them. Maps and graph entities are not property values.
fn is_property_value(value: &Value) -> bool {
    match value {
        Value::Map(_) | Value::Node(_) | Value::NodeRef(_) | Value::Relationship(_) => false,
        Value::Path(_) => false,
        Value::List(items) => items.iter().all(is_property_value),
        _ => true,
    }
}

impl fmt::Display for TypeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeKind::Any => f.write_str("ANY"),
            TypeKind::PropertyValue => f.write_str("PROPERTY VALUE"),
            TypeKind::Nothing => f.write_str("NOTHING"),
            TypeKind::Null => f.write_str("NULL"),
            TypeKind::Declared(declared) => f.write_str(declared.name()),
            TypeKind::Map => f.write_str("MAP"),
            TypeKind::Node => f.write_str("NODE"),
            TypeKind::Relationship => f.write_str("RELATIONSHIP"),
            TypeKind::Path => f.write_str("PATH"),
            TypeKind::List(inner) => write!(f, "LIST<{inner}>"),
        }
    }
}

impl fmt::Display for TypeTerm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        if self.not_null {
            f.write_str(" NOT NULL")?;
        }
        Ok(())
    }
}

impl fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (at, term) in self.0.iter().enumerate() {
            if at > 0 {
                f.write_str(" | ")?;
            }
            write!(f, "{term}")?;
        }
        Ok(())
    }
}

/// Upper-cased words and single-character punctuation tokens.
fn lex(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
            continue;
        }
        if !word.is_empty() {
            tokens.push(std::mem::take(&mut word).to_uppercase());
        }
        if !c.is_whitespace() {
            tokens.push(c.to_string());
        }
    }
    if !word.is_empty() {
        tokens.push(word.to_uppercase());
    }
    tokens
}

/// A nesting budget: `LIST<LIST<…>>` recursion is bounded so a hostile
/// expression cannot exhaust the stack.
const MAX_TYPE_DEPTH: usize = 32;

fn is_word(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

fn parse_union(tokens: &[String], at: &mut usize, depth: usize) -> Result<TypeExpr, String> {
    if depth > MAX_TYPE_DEPTH {
        return Err("type expression is nested too deeply".to_string());
    }
    let mut terms = vec![parse_term(tokens, at, depth)?];
    while tokens.get(*at).map(String::as_str) == Some("|") {
        *at += 1;
        terms.push(parse_term(tokens, at, depth)?);
    }
    let mut seen = BTreeSet::new();
    terms.retain(|term| seen.insert(term.to_string()));
    Ok(TypeExpr(terms))
}

fn parse_term(tokens: &[String], at: &mut usize, depth: usize) -> Result<TypeTerm, String> {
    let kind = parse_kind(tokens, at, depth)?;
    let mut not_null = false;
    if tokens.get(*at).map(String::as_str) == Some("NOT")
        && tokens.get(*at + 1).map(String::as_str) == Some("NULL")
    {
        *at += 2;
        not_null = true;
    }
    Ok(TypeTerm { kind, not_null })
}

fn parse_kind(tokens: &[String], at: &mut usize, depth: usize) -> Result<TypeKind, String> {
    let words: Vec<&str> = tokens[*at..]
        .iter()
        .take(MAX_PHRASE_WORDS)
        .take_while(|t| is_word(t))
        .map(String::as_str)
        .collect();
    if words.is_empty() {
        return Err(format!(
            "expected a type name, found {}; the types are {TYPE_NAMES}",
            tokens
                .get(*at)
                .map_or("end of input".to_string(), |t| format!("'{t}'"))
        ));
    }
    if matches!(words[0], "LIST" | "ARRAY") {
        *at += 1;
        if tokens.get(*at).map(String::as_str) != Some("<") {
            return Ok(TypeKind::List(Box::new(TypeExpr(vec![TypeTerm {
                kind: TypeKind::Any,
                not_null: false,
            }]))));
        }
        *at += 1;
        let inner = parse_union(tokens, at, depth + 1)?;
        if tokens.get(*at).map(String::as_str) != Some(">") {
            return Err("expected '>' to close LIST<...>".to_string());
        }
        *at += 1;
        return Ok(TypeKind::List(Box::new(inner)));
    }
    for n in (1..=words.len()).rev() {
        if let Some(kind) = base_kind(&words[..n].join(" ")) {
            *at += n;
            return Ok(kind);
        }
    }
    Err(format!(
        "unknown type '{}'; the types are {TYPE_NAMES}",
        words[0]
    ))
}

/// `valueType(v)`: Neo4j's name for the most specific type of `value`.
pub fn value_type_string(value: &Value) -> String {
    TypeExpr::of_value(value).to_string()
}
