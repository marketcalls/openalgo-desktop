//! Reading an order's fields without turning bad data into defaults (the
//! web's `RuntimeOrderResolver`, `flow_executor_service.py` 573-686).
//!
//! The general accessors on `NodeCtx` (`get_str`, `get_int`, `get_float`)
//! forgive: a value that does not parse becomes the default, which suits
//! display and data nodes. At an order boundary that is unsafe: a webhook
//! that omitted `qty` would place quantity 1 at MARKET. This resolver uses a
//! default only when no key is present at all; a supplied value must resolve
//! and validate on its own, or the node fails with the sentence the web
//! writes (`quantity must be at least 1, got 0.0`).

use super::pyfmt;
use super::vars::{has_unresolved, VarStore};
use serde_json::{Map, Value};

/// One order payload being resolved: the node's data and its label.
pub struct OrderFields<'a> {
    vars: &'a VarStore,
    data: &'a Map<String, Value>,
}

impl<'a> OrderFields<'a> {
    pub fn new(vars: &'a VarStore, data: &'a Map<String, Value>) -> Self {
        Self { vars, data }
    }

    /// The first present key among `key` and `aliases`, resolved; its
    /// default when none is present; an error when there is no default.
    pub fn value(
        &self,
        key: &str,
        default: Option<Value>,
        aliases: &[&str],
    ) -> Result<Value, String> {
        for candidate in std::iter::once(key).chain(aliases.iter().copied()) {
            let Some(raw) = self.data.get(candidate) else {
                continue;
            };
            let resolved = self.vars.resolve_raw(raw);
            if let Value::String(s) = &resolved {
                if has_unresolved(s) {
                    return Err(format!(
                        "{} has unresolved value {}",
                        candidate,
                        pyfmt::quote(s)
                    ));
                }
            }
            return Ok(resolved);
        }
        default.ok_or_else(|| format!("{} is required", key))
    }

    /// Non-blank text, trimmed.
    pub fn text(
        &self,
        key: &str,
        default: Option<&str>,
        aliases: &[&str],
    ) -> Result<String, String> {
        let value = self.value(key, default.map(Value::from), aliases)?;
        let Value::String(s) = value else {
            return Err(format!(
                "{} must resolve to text, got {}",
                key,
                pyfmt::repr(&value)
            ));
        };
        let t = s.trim();
        if t.is_empty() {
            return Err(format!("{} must not be blank", key));
        }
        Ok(t.to_string())
    }

    /// Text upper-cased and checked against `allowed`.
    pub fn choice(
        &self,
        key: &str,
        allowed: &[&str],
        default: Option<&str>,
        aliases: &[&str],
    ) -> Result<String, String> {
        let canonical = self.text(key, default, aliases)?.to_uppercase();
        if !allowed.contains(&canonical.as_str()) {
            let mut sorted: Vec<&str> = allowed.to_vec();
            sorted.sort_unstable();
            return Err(format!(
                "{} must be one of {}, got {}",
                key,
                sorted.join(", "),
                pyfmt::quote(&canonical)
            ));
        }
        Ok(canonical)
    }

    /// A finite number (a boolean, blank text or non-number is refused).
    pub fn number(&self, key: &str, default: Option<f64>, aliases: &[&str]) -> Result<f64, String> {
        let value = self.value(key, default.map(Value::from), aliases)?;
        if let Value::Bool(_) = value {
            return Err(format!(
                "{} must be numeric, got boolean {}",
                key,
                pyfmt::repr(&value)
            ));
        }
        if let Value::String(s) = &value {
            if s.trim().is_empty() {
                return Err(format!("{} must not be blank", key));
            }
        }
        let n = pyfmt::to_float(&value)
            .ok_or_else(|| format!("{} must be numeric, got {}", key, pyfmt::repr(&value)))?;
        if !n.is_finite() {
            return Err(format!(
                "{} must be finite, got {}",
                key,
                pyfmt::repr(&value)
            ));
        }
        Ok(n)
    }

    /// A whole number of at least `minimum` (truncated toward zero, as
    /// Python's `int(float)`).
    pub fn integer(
        &self,
        key: &str,
        default: Option<i64>,
        aliases: &[&str],
        minimum: Option<i64>,
    ) -> Result<i64, String> {
        let n = self.number(key, default.map(|d| d as f64), aliases)?;
        if let Some(min) = minimum {
            if n < min as f64 {
                let qualifier = if min == 0 {
                    "non-negative".to_string()
                } else {
                    format!("at least {}", min)
                };
                return Err(format!(
                    "{} must be {}, got {}",
                    key,
                    qualifier,
                    pyfmt::float(n)
                ));
            }
        }
        Ok(n.trunc() as i64)
    }

    /// An optional per-leg text: absent or `null` is empty, otherwise text
    /// (the web's `_optional_leg_text`).
    pub fn optional_text(&self, key: &str) -> Result<String, String> {
        match self.value(key, Some(Value::from("")), &[])? {
            Value::Null => Ok(String::new()),
            Value::String(s) => Ok(s.trim().to_string()),
            other => Err(format!(
                "{} must resolve to text, got {}",
                key,
                pyfmt::repr(&other)
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn data(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn a_default_is_used_only_when_no_key_is_present() {
        let vars = VarStore::default();
        let d = data(json!({}));
        let f = OrderFields::new(&vars, &d);
        assert_eq!(f.integer("quantity", Some(1), &[], Some(1)), Ok(1));
        let d = data(json!({"quantity": "{{webhook.qty}}"}));
        let f = OrderFields::new(&vars, &d);
        assert_eq!(
            f.integer("quantity", Some(1), &[], Some(1)),
            Err("quantity has unresolved value '{{webhook.qty}}'".into())
        );
    }

    #[test]
    fn supplied_values_must_stand_on_their_own() {
        let vars = VarStore::default();
        vars.set("q", json!(0));
        let d = data(json!({"quantity": "{{q}}", "action": "hold", "price": true, "x": " "}));
        let f = OrderFields::new(&vars, &d);
        assert_eq!(
            f.integer("quantity", Some(1), &[], Some(1)),
            Err("quantity must be at least 1, got 0.0".into())
        );
        assert_eq!(
            f.choice("action", &["SELL", "BUY"], Some("BUY"), &[]),
            Err("action must be one of BUY, SELL, got 'HOLD'".into())
        );
        assert_eq!(
            f.number("price", Some(0.0), &[]),
            Err("price must be numeric, got boolean True".into())
        );
        assert_eq!(f.text("x", None, &[]), Err("x must not be blank".into()));
        assert_eq!(
            f.text("symbol", None, &[]),
            Err("symbol is required".into())
        );
    }

    #[test]
    fn aliases_and_whole_references_resolve() {
        let vars = VarStore::default();
        vars.set("px", json!(101.5));
        let d = data(json!({"pricetype": "limit", "price": "{{px}}"}));
        let f = OrderFields::new(&vars, &d);
        assert_eq!(
            f.choice(
                "priceType",
                &["MARKET", "LIMIT"],
                Some("MARKET"),
                &["pricetype"]
            ),
            Ok("LIMIT".into())
        );
        assert_eq!(f.number("price", Some(0.0), &[]), Ok(101.5));
        assert_eq!(f.optional_text("expiryDate"), Ok(String::new()));
    }
}
