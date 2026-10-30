//! Math Expression (the web's `execute_math_expression` and
//! `_safe_eval_math`, `flow_executor_service.py` 2748-2857): the
//! interpolated text is parsed as a Python expression and walked node by
//! node, allowing only numbers, `+ - * / % **`, unary `+`/`-`, parentheses
//! and `floor(x)`. Nothing is ever executed; every other construct is
//! refused with the web's sentence, in the order the web's walk meets it.
//!
//! Python's arithmetic is reproduced, not approximated: integers stay exact
//! until the final `float()` (`(2**60 + 1) - 2**60` is `1.0`), `/` is true
//! division, `%` takes the divisor's sign, `**` binds tighter than unary
//! minus and to the right, and a negative base to a fractional power is a
//! complex number the final `float()` refuses. Exponents above 100 in
//! magnitude are refused, as on the web.

pub mod bigint;
mod lexer;
mod num;
mod parser;

use self::num::Num;
use self::parser::{BinOp, Expr, ParseError, UnOp};
use super::pyjson::float_value;
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome};
use crate::flow::contract::pyfmt;
use async_trait::async_trait;
use serde_json::{json, Map, Value};

/// The largest exponent `**` accepts, either sign.
const MAX_EXPONENT: u64 = 100;

/// The stack an evaluation runs on. The parser recurses once per nesting
/// level, as Python's does, up to the web's limits (200 brackets, about
/// 900 levels); in a debug build that needs several megabytes, more than a
/// tokio worker's 2 MiB, so it never runs on one.
pub const EVAL_STACK_BYTES: usize = 32 * 1024 * 1024;

/// What the node says if the evaluation thread cannot be started.
const NO_THREAD: &str = "the expression could not be evaluated right now; try again";

/// Evaluate `expression` as the web's `_safe_eval_math` does, on a thread
/// of its own with [`EVAL_STACK_BYTES`] of stack: the float result, or the
/// text of what the web raises.
pub fn safe_eval(expression: &str) -> Result<f64, String> {
    let text = expression.to_string();
    std::thread::Builder::new()
        .name("flow-math".into())
        .stack_size(EVAL_STACK_BYTES)
        .spawn(move || evaluate(&text))
        .map_err(|_| NO_THREAD.to_string())?
        .join()
        .map_err(|_| NO_THREAD.to_string())?
}

/// [`safe_eval`] from a run, without holding up the async worker.
async fn safe_eval_detached(expression: String) -> Result<f64, String> {
    tokio::task::spawn_blocking(move || safe_eval(&expression))
        .await
        .map_err(|_| NO_THREAD.to_string())?
}

/// `_safe_eval_math` itself, on the calling thread's stack.
fn evaluate(expression: &str) -> Result<f64, String> {
    // str.strip(): Unicode whitespace, which in Python includes the
    // information separators 0x1c-0x1f.
    let cleaned =
        expression.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c));
    if cleaned.is_empty() {
        return Err("Empty expression".into());
    }
    let syntax = |e: lexer::SyntaxError| format!("Invalid expression syntax: {}", e.text());
    if cleaned.contains('\0') {
        return Err(syntax(lexer::SyntaxError::new(
            "source code string cannot contain null bytes",
            1,
        )));
    }
    let tokens = lexer::tokenize(cleaned).map_err(syntax)?;
    let tree = parser::parse(tokens).map_err(|e| match e {
        ParseError::Syntax(s) => syntax(s),
        ParseError::TooDeep => "maximum recursion depth exceeded".to_string(),
    })?;
    walk(&tree)?.to_float()
}

/// The web's `_eval`: a pure walk over the parsed tree (no code runs).
fn walk(e: &Expr) -> Result<Num, String> {
    match e {
        Expr::Num(n) => Ok(n.clone()),
        Expr::Const(text) => Err(format!("Unsupported constant: {}", text)),
        Expr::Call {
            func,
            args,
            keywords,
        } => {
            let floor_call = matches!(func.as_ref(), Expr::Name(n) if n == "floor");
            match args.as_slice() {
                [arg] if floor_call && *keywords == 0 => walk(arg)?.floor(),
                _ => Err("Only floor(expression) is supported".into()),
            }
        }
        Expr::Bin(left, op, right) => {
            let l = walk(left)?;
            let r = walk(right)?;
            match op {
                BinOp::Add => l.add(&r),
                BinOp::Sub => l.sub(&r),
                BinOp::Mult => l.mul(&r),
                BinOp::Div => l.div(&r),
                BinOp::Mod => l.rem(&r),
                BinOp::Pow if r.abs_over(MAX_EXPONENT)? => Err(format!(
                    "Exponent too large: {} (max {})",
                    r.text(),
                    MAX_EXPONENT
                )),
                BinOp::Pow => l.pow(&r),
                other => Err(format!("Unsupported operator: {}", other.name())),
            }
        }
        Expr::Unary(op, operand) => {
            let v = walk(operand)?;
            match op {
                UnOp::USub => Ok(v.neg()),
                UnOp::UAdd => Ok(v),
                UnOp::Not => Err("Unsupported unary operator: Not".into()),
                UnOp::Invert => Err("Unsupported unary operator: Invert".into()),
            }
        }
        Expr::Name(_) => Err("Unsupported expression type: Name".into()),
        Expr::Other(kind) => Err(format!("Unsupported expression type: {}", kind)),
    }
}

/// Every `{{...}}` still in interpolated text, in order.
fn unresolved_refs(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else { break };
        if after[end..].starts_with("}}") {
            out.push(&rest[start..start + 2 + end + 2]);
            rest = &after[end + 2..];
        } else {
            rest = &after[end..];
        }
    }
    out
}

/// What a missing variable reads (parity N-mathExpression-11: the web
/// parses `{{x}}` as a set and says `Unsupported expression type: Set`).
fn unresolved_message(refs: &[&str]) -> String {
    match refs {
        [one] => format!("unresolved variable {}", one),
        many => format!("unresolved variables {}", many.join(", ")),
    }
}

pub struct MathExpression;

#[async_trait]
impl FlowNode for MathExpression {
    fn node_type(&self) -> &'static str {
        "mathExpression"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let expression = data.get("expression").cloned().unwrap_or(json!(""));
        let output = data
            .get("outputVariable")
            .cloned()
            .unwrap_or(json!("result"));
        if !pyfmt::truthy(&expression) {
            ctx.error("No expression provided");
            return NodeOutcome::error("No expression provided");
        }
        ctx.info(format!("Evaluating: {}", pyfmt::str(&expression)));
        let interpolated = ctx.vars.interpolate_value(&expression);
        ctx.info(format!("Interpolated: {}", pyfmt::str(&interpolated)));
        let evaluated = match &interpolated {
            Value::String(text) => {
                let refs = unresolved_refs(text);
                if refs.is_empty() {
                    safe_eval_detached(text.clone()).await
                } else {
                    Err(unresolved_message(&refs))
                }
            }
            // `.strip()` on a number or object from imported JSON.
            other => Err(format!(
                "'{}' object has no attribute 'strip'",
                pyfmt::type_name(other)
            )),
        };
        match evaluated {
            Ok(result) => {
                let name = pyfmt::str(&output);
                let value = float_value(result);
                ctx.vars.set(&name, value.clone());
                ctx.info(format!("Result: {} = {}", name, pyfmt::float(result)));
                NodeOutcome::result(json!({
                    "status": "success",
                    "expression": expression,
                    "interpolated": interpolated,
                    "result": value,
                    "outputVariable": output,
                }))
            }
            Err(message) => {
                ctx.error(format!("Math expression failed: {}", message));
                NodeOutcome::error(message)
            }
        }
    }
}

#[cfg(test)]
mod tests;
