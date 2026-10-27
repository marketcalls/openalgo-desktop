//! A JSON value that keeps its keys in the order they were written.
//!
//! The web builds every indicator result as a Python dict and dumps it, so
//! the model reads `indicator`, `category`, `description`, `params_used`, ...
//! in that order. `serde_json::Value` sorts object keys (the crate is built
//! without `preserve_order`, which the `/api/v1` contract relies on), so the
//! results are built as this type and serialised from it directly.

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    /// A float; NaN and the infinities serialise as `null`, as the web's
    /// `json_safe` renders them.
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn str(text: impl Into<String>) -> Self {
        Json::Str(text.into())
    }

    /// An object from `(key, value)` pairs, in the order given.
    pub fn obj<K: Into<String>>(pairs: impl IntoIterator<Item = (K, Json)>) -> Self {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// Set `key`, in place when it is already present (a Python dict
    /// assignment), at the end otherwise.
    pub fn set(&mut self, key: &str, value: Json) {
        if let Json::Obj(pairs) = self {
            match pairs.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => pairs.push((key.to_string(), value)),
            }
        }
    }

    /// The value under `key` in an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The compact text the web's `json.dumps(..., separators=(",", ":"))`
    /// gives, keys in order.
    pub fn to_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "null".into())
    }
}

impl Serialize for Json {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Json::Null => serializer.serialize_unit(),
            Json::Bool(b) => serializer.serialize_bool(*b),
            Json::Int(v) => serializer.serialize_i64(*v),
            Json::Num(v) if v.is_finite() => serializer.serialize_f64(*v),
            Json::Num(_) => serializer.serialize_unit(),
            Json::Str(s) => serializer.serialize_str(s),
            Json::Arr(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Json::Obj(pairs) => {
                let mut map = serializer.serialize_map(Some(pairs.len()))?;
                for (k, v) in pairs {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_keep_their_order_and_set_replaces_in_place() {
        let mut j = Json::obj([("b", Json::Int(1)), ("a", Json::Num(f64::NAN))]);
        j.set("b", Json::Bool(true));
        j.set("c", Json::str("x"));
        assert_eq!(j.to_text(), r#"{"b":true,"a":null,"c":"x"}"#);
        assert_eq!(j.get("c"), Some(&Json::str("x")));
    }
}
