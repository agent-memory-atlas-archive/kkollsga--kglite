//! The statement prefix `FOR <axis> AS OF <instant>` or `FOR <axis> ALL`, and the refusals for
//! prefixes written where only a statement may carry them.
//!
//! `FOR`, `OF` and the axis names are not reserved words: they arrive as
//! identifiers and are matched case-insensitively. The parser accepts any
//! axis; lowering refuses every axis but `VALID_TIME`, so a client can probe
//! for support and a later axis adds semantics, not syntax.

use super::super::ast::{ContextInstant, ContextOrigin, Expression, StatementContext};
use super::super::tokenizer::{describe_token, describe_token_opt, CypherToken};
use super::CypherParser;
use crate::datatypes::values::Value;

pub(super) const DOUBLED_CONTEXT: &str =
    "A statement takes one FOR <axis> AS OF context; this one has two";

const INSTANT_FORMS: &str = "a quoted ISO date or datetime, $param, date('…'), date($param), \
                             datetime('…'), datetime($param), or date() for today (UTC)";

impl CypherParser {
    /// Parse `FOR <axis> AS OF <instant>` or `FOR <axis> ALL`; the caller has
    /// seen `FOR`.
    pub(super) fn parse_statement_context(&mut self) -> Result<StatementContext, String> {
        self.advance();
        let axis = match self.peek() {
            Some(CypherToken::Identifier(axis)) => axis.clone(),
            other => {
                return Err(format!(
                    "Expected a time axis after FOR (FOR VALID_TIME AS OF <instant>), got {}",
                    describe_token_opt(other)
                ))
            }
        };
        self.advance();
        if self.check(&CypherToken::All) {
            self.advance();
            return Ok(StatementContext {
                axis,
                instant: ContextInstant::All,
                origin: ContextOrigin::Explicit,
                refusal: None,
                merged: None,
                body_start: 0,
            });
        }
        if !self.check(&CypherToken::As) {
            return Err(format!(
                "Expected AS OF or ALL after FOR {axis}, got {}",
                describe_token_opt(self.peek())
            ));
        }
        self.advance();
        self.expect_soft_word("OF", "FOR <axis> AS OF <instant>")?;
        let start = self.pos;
        let instant = self.parse_expression()?;
        if !is_context_instant(&instant) {
            // Point the caret at the instant, not past it.
            self.pos = start;
            return Err(format!(
                "FOR {axis} AS OF takes a constant instant: {INSTANT_FORMS}"
            ));
        }
        Ok(StatementContext {
            axis,
            instant: ContextInstant::AsOf(instant),
            origin: ContextOrigin::Explicit,
            refusal: None,
            merged: None,
            body_start: 0,
        })
    }

    /// The error for a token that cannot start a clause. `EXPLAIN`,
    /// `PROFILE` and a context prefix are statement prefixes, so reaching one
    /// here means it was written inside a UNION arm, a `CALL { }` body or
    /// after a clause.
    pub(super) fn unexpected_clause_start(&self, token: &CypherToken) -> String {
        match token {
            CypherToken::Explain | CypherToken::Profile => format!(
                "{} must lead the statement; a UNION arm or a CALL {{ }} body cannot carry it",
                describe_token(token)
            ),
            CypherToken::Identifier(word) if word.eq_ignore_ascii_case("FOR") => {
                "FOR <axis> AS OF is a statement prefix; a UNION arm or a CALL { } body cannot \
                 set its own context (one context per statement)"
                    .to_string()
            }
            _ => format!(
                "Unexpected token at start of clause: {}",
                describe_token(token)
            ),
        }
    }
}

/// A literal string, a parameter, or `date` / `datetime` of one — and
/// `date()` with no argument, today in UTC when the statement executes.
/// `datetime()` with no argument is not accepted: the statement resolves its
/// instant more than once (the timeless-plan check, the execution filter, the
/// diagnostics echo) and a clock reading to the microsecond differs between
/// them, while today's date does not.
fn is_context_instant(expression: &Expression) -> bool {
    let operand = |e: &Expression| {
        matches!(
            e,
            Expression::Literal(Value::String(_)) | Expression::Parameter(_)
        )
    };
    match expression {
        Expression::FunctionCall {
            name,
            args,
            distinct: false,
        } => {
            let date = name.eq_ignore_ascii_case("date");
            match args.as_slice() {
                [] => date,
                [arg] => (date || name.eq_ignore_ascii_case("datetime")) && operand(arg),
                _ => false,
            }
        }
        other => operand(other),
    }
}

#[cfg(test)]
#[path = "statement_context_tests.rs"]
mod tests;
