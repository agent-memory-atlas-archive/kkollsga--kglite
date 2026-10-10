//! Cypher scalar functions — `toBoolean` and `tail`.
//!
//! Routed from `super::evaluate_scalar_function` after the larger category
//! modules; returns `Ok(None)` when `name` is not one of these so the
//! dispatcher moves on.
use super::super::helpers::*;
use super::super::*;
use crate::datatypes::values::Value;

impl<'a> CypherExecutor<'a> {
    pub(super) fn eval_conversion_fn(
        &self,
        name: &str,
        args: &[Expression],
        row: &ResultRow,
    ) -> Result<Option<Value>, String> {
        let result = match name {
            "toboolean" => {
                let val = self.evaluate_expression(super::first_arg(name, args)?, row)?;
                match &val {
                    Value::Null | Value::Boolean(_) => Ok(val),
                    Value::String(s) if s.eq_ignore_ascii_case("true") => Ok(Value::Boolean(true)),
                    Value::String(s) if s.eq_ignore_ascii_case("false") => {
                        Ok(Value::Boolean(false))
                    }
                    Value::String(_) => Ok(Value::Null),
                    other => Err(format!(
                        "toBoolean() expects a boolean or a string, got {}",
                        other.type_name()
                    )),
                }
            }
            "tail" => {
                if args.len() != 1 {
                    return Err("tail() requires 1 argument".into());
                }
                let val = self.evaluate_expression(super::first_arg(name, args)?, row)?;
                Ok(match list_operand(&val, "tail()")? {
                    None => Value::Null,
                    Some(items) => Value::List(items.iter().skip(1).cloned().collect()),
                })
            }
            _ => return Ok(None),
        };
        result.map(Some)
    }
}
