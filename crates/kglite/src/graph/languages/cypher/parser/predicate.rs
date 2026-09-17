//! Cypher parser: WHERE predicate tree (OR / XOR / AND / NOT / comparisons).

use super::super::ast::*;
use super::super::tokenizer::{keyword_name_token, reserved_literal_name_token, CypherToken};
use super::CypherParser;

impl CypherParser {
    pub(super) fn parse_where_clause(&mut self) -> Result<Clause, String> {
        self.expect(&CypherToken::Where)?;
        let predicate = self.parse_predicate()?;
        Ok(Clause::Where(WhereClause { predicate }))
    }

    /// Parse a predicate by delegating to the boolean *expression* tower and
    /// converting the result. There is exactly one boolean parser — the
    /// expression tower in [`super::expression`] — so predicate position
    /// (WHERE, CASE WHEN, comprehension filters, quantifiers, HAVING) and
    /// expression position (RETURN, WITH, function args) share one
    /// capability set that cannot drift. EXISTS/pattern/label-check parsing
    /// lives in the tower (`parse_primary_expression` /
    /// `parse_comparison_expression`); a non-boolean expression falls back
    /// to the historical truthy form `expr <> false`.
    pub(super) fn parse_predicate(&mut self) -> Result<Predicate, String> {
        let expr = self.parse_expression_with_predicates()?;
        Ok(Self::expression_as_predicate(expr))
    }

    /// Parse the body of an EXISTS predicate; the EXISTS keyword itself has
    /// already been consumed. Handles `EXISTS { pattern(s) [WHERE pred] }`,
    /// `EXISTS((pattern))`, and rejects the Neo4j-legacy `exists(n.prop)`
    /// with a targeted hint. Shared by predicate and expression positions.
    pub(super) fn parse_exists_predicate(&mut self) -> Result<Predicate, String> {
        if self.check(&CypherToken::LBrace) {
            self.advance(); // consume {
            let (patterns, pattern_groups) = self.parse_exists_patterns()?;
            // Check for optional WHERE clause inside EXISTS { MATCH ... WHERE ... }
            let where_clause = if self.check(&CypherToken::Where) {
                self.advance(); // consume WHERE
                Some(Box::new(self.parse_predicate()?))
            } else {
                None
            };
            self.expect(&CypherToken::RBrace)?;
            Ok(Predicate::Exists {
                patterns,
                pattern_groups,
                where_clause,
            })
        } else if self.check(&CypherToken::LParen) {
            self.advance(); // consume outer (
                            // Support EXISTS((...)) — inner parens are the pattern
            if self.check(&CypherToken::LParen) {
                let pattern_str = self.extract_pattern_string()?;
                let pattern = crate::graph::core::pattern_matching::parse_pattern(&pattern_str)?;
                self.expect(&CypherToken::RParen)?; // consume outer )
                Ok(Predicate::Exists {
                    patterns: vec![pattern],
                    pattern_groups: vec![0],
                    where_clause: None,
                })
            } else if self.looks_like_property_access() {
                // Neo4j-style `exists(n.prop)` for property-existence
                // checks. KGLite doesn't support this form — point the
                // user at the modern equivalent rather than the pattern
                // error, which sends them down the wrong rabbit hole.
                Err(
                    "exists(n.prop) is Neo4j legacy syntax for property-existence. \
                     Use `WHERE n.prop IS NOT NULL` instead. \
                     (For pattern-existence, EXISTS takes a pattern: \
                     `EXISTS { (n)-[:REL]->() }` or `EXISTS((n)-[:REL]->())`.)"
                        .to_string(),
                )
            } else {
                Err("EXISTS(...) requires a pattern in parentheses, e.g. \
                     EXISTS((n)-[:REL]->()). For property-existence checks, \
                     use `WHERE n.prop IS NOT NULL` (Neo4j-style \
                     `exists(n.prop)` is not supported)."
                    .to_string())
            }
        } else {
            Err("Expected '{' or '(' after EXISTS".to_string())
        }
    }

    /// Parse a [value, value, ...] list for IN clause
    pub(super) fn parse_list_expression(&mut self) -> Result<Vec<Expression>, String> {
        self.expect(&CypherToken::LBracket)?;
        let mut items = Vec::new();

        if !self.check(&CypherToken::RBracket) {
            items.push(self.parse_expression()?);
            while self.check(&CypherToken::Comma) {
                self.advance();
                items.push(self.parse_expression()?);
            }
        }

        self.expect(&CypherToken::RBracket)?;
        Ok(items)
    }

    /// True when the next three tokens are `<ident> . <ident>` — i.e. the
    /// shape of a property access (`n.prop`). Used by `parse_exists_predicate`
    /// to recognise the Neo4j-legacy `exists(n.prop)` form and steer the
    /// user to `IS NOT NULL` instead of the generic pattern-required error.
    pub(super) fn looks_like_property_access(&self) -> bool {
        matches!(self.peek(), Some(CypherToken::Identifier(_)))
            && matches!(self.peek_at(1), Some(CypherToken::Dot))
            && matches!(self.peek_at(2), Some(CypherToken::Identifier(_)))
    }

    /// Quick lookahead to check if ( starts a pattern (node pattern) vs a parenthesized predicate
    pub(super) fn looks_like_pattern_start(&self) -> bool {
        // Pattern: (var:Type), (:Type), (), (var)-[...]->()
        // Predicate: (expr op expr), (NOT ...)
        match self.peek_at(1) {
            Some(CypherToken::RParen) => {
                // () closed immediately — pattern if an edge continuation follows
                self.is_edge_continuation_at(2)
            }
            Some(CypherToken::Colon) => true, // (:Type)
            Some(CypherToken::Identifier(_)) => {
                match self.peek_at(2) {
                    // (var:Type — only a *node pattern* if the label list is
                    // followed by a property map or the closing paren.
                    // `(n:A OR n:B)` is a parenthesised boolean expression:
                    // committing to the pattern parser here made it a syntax
                    // error while the unparenthesised `n:A OR n:B` parsed.
                    Some(CypherToken::Colon) => self.node_pattern_after_labels(2),
                    // (var {props} — a property map cannot open an expression
                    // after a variable, so this is a node pattern.
                    Some(CypherToken::LBrace) => true,
                    Some(CypherToken::RParen) => {
                        // (var) — pattern only if a real edge continuation
                        // follows, e.g. (p)-[:REL]->() / (p)-->() / (p)<-[...].
                        // A lone `-` is NOT enough: `(a) - (b)` is subtraction.
                        self.is_edge_continuation_at(3)
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Walk the label list that starts at `offset` (which points at the first
    /// `:` after a variable) and report whether what follows it can only be a
    /// node pattern.
    ///
    /// A node pattern continues with `{` (property map) or `)` — the latter
    /// either closing an existential node pattern or preceding a relationship.
    /// Anything else (`OR`, `AND`, `XOR`, a comparison operator, …) belongs to
    /// the boolean tower, which parses `n:A` as a label check itself. Both
    /// `:A:B` chains and `:A|B` alternations are walked, because the pattern
    /// parser accepts both spellings.
    fn node_pattern_after_labels(&self, offset: usize) -> bool {
        let mut idx = offset;
        while matches!(
            self.peek_at(idx),
            Some(CypherToken::Colon) | Some(CypherToken::Pipe)
        ) {
            if !self.is_label_name_at(idx + 1) {
                return false;
            }
            idx += 2;
        }
        matches!(
            self.peek_at(idx),
            Some(CypherToken::LBrace) | Some(CypherToken::RParen)
        )
    }

    /// True when the token at `offset` can stand in a label position — the
    /// same set [`CypherParser::expect_label_name`] accepts: an identifier, a
    /// `$param` reference, or a keyword soft enough to be used as a name.
    fn is_label_name_at(&self, offset: usize) -> bool {
        match self.peek_at(offset) {
            Some(CypherToken::Identifier(_)) | Some(CypherToken::Parameter(_)) => true,
            Some(token) => {
                keyword_name_token(token).is_some() || reserved_literal_name_token(token).is_some()
            }
            None => false,
        }
    }

    /// True when the tokens at `offset` (relative to the current position)
    /// begin a relationship continuation: `-[`, `--`, `->`, or `<-`.
    fn is_edge_continuation_at(&self, offset: usize) -> bool {
        match self.peek_at(offset) {
            Some(CypherToken::Dash) => matches!(
                self.peek_at(offset + 1),
                Some(CypherToken::LBracket)
                    | Some(CypherToken::Dash)
                    | Some(CypherToken::GreaterThan)
            ),
            Some(CypherToken::LessThan) => self.peek_at(offset + 1) == Some(&CypherToken::Dash),
            _ => false,
        }
    }
}
