//! Request validation with the web's Marshmallow semantics.
//!
//! Every `/api/v1` body is checked against a [`Schema`] declared like the
//! web's `restx_api/schemas.py`, `data_schemas.py` and `account_schema.py`:
//! the same field kinds, defaults, validators and messages, errors collected
//! in declaration order (unknown fields last), and `post_load` hooks that
//! only run on a clean load. The result is either the loaded object (types
//! coerced, defaults filled) or a [`FieldErrors`] tree that renders three
//! ways, matching the three error shapes the web returns:
//!
//! * [`FieldErrors::to_json`]: an object of field errors (read endpoints);
//! * [`FieldErrors::to_python`]: `str(err.messages)`, a stringified Python
//!   dict (order endpoints);
//! * the optionsmultiorder `{"message": "Validation error", "errors": {...}}`
//!   envelope uses the JSON form.

use chrono::NaiveDate;
use serde_json::{Map, Number, Value};

/// A key in an error tree: a field name, or a list index.
#[derive(Debug, Clone, PartialEq)]
pub enum ErrKey {
    Field(String),
    Index(usize),
}

/// One node of an error tree.
#[derive(Debug, Clone, PartialEq)]
pub enum ErrNode {
    Messages(Vec<String>),
    Nested(FieldErrors),
}

/// Marshmallow `err.messages`, in insertion order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FieldErrors(pub Vec<(ErrKey, ErrNode)>);

impl FieldErrors {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Add messages for a field (merged with any already there).
    pub fn add(&mut self, field: &str, msg: impl Into<String>) {
        let msg = msg.into();
        for (k, v) in self.0.iter_mut() {
            if *k == ErrKey::Field(field.to_string()) {
                if let ErrNode::Messages(m) = v {
                    m.push(msg);
                    return;
                }
            }
        }
        self.0.push((
            ErrKey::Field(field.to_string()),
            ErrNode::Messages(vec![msg]),
        ));
    }

    fn nested(&mut self, key: ErrKey, errs: FieldErrors) {
        self.0.push((key, ErrNode::Nested(errs)));
    }

    /// One field, one message.
    pub fn single(field: &str, msg: impl Into<String>) -> Self {
        let mut f = FieldErrors::default();
        f.add(field, msg);
        f
    }

    /// The messages recorded for a top-level field (tests).
    pub fn messages(&self, field: &str) -> Vec<String> {
        self.0
            .iter()
            .find(|(k, _)| *k == ErrKey::Field(field.to_string()))
            .and_then(|(_, v)| match v {
                ErrNode::Messages(m) => Some(m.clone()),
                ErrNode::Nested(_) => None,
            })
            .unwrap_or_default()
    }

    /// JSON object form (Flask `jsonify(err.messages)`: index keys become
    /// strings).
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        for (k, v) in &self.0 {
            let key = match k {
                ErrKey::Field(s) => s.clone(),
                ErrKey::Index(i) => i.to_string(),
            };
            let val = match v {
                ErrNode::Messages(ms) => {
                    Value::Array(ms.iter().map(|s| Value::String(s.clone())).collect())
                }
                ErrNode::Nested(n) => n.to_json(),
            };
            m.insert(key, val);
        }
        Value::Object(m)
    }

    /// Python `str(err.messages)`.
    pub fn to_python(&self) -> String {
        let mut out = String::from("{");
        for (i, (k, v)) in self.0.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            match k {
                ErrKey::Field(s) => out.push_str(&py_repr_str(s)),
                ErrKey::Index(n) => out.push_str(&n.to_string()),
            }
            out.push_str(": ");
            match v {
                ErrNode::Messages(ms) => {
                    out.push('[');
                    out.push_str(
                        &ms.iter()
                            .map(|s| py_repr_str(s))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                    out.push(']');
                }
                ErrNode::Nested(n) => out.push_str(&n.to_python()),
            }
        }
        out.push('}');
        out
    }
}

/// Python `repr()` of a `str`.
pub fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `repr()` of a number in a validator message (`1`, `0`, `100`,
/// `1.5`).
fn py_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{}", v)
    }
}

/// A field validator (marshmallow `validate.*`).
#[derive(Clone)]
pub enum Validator {
    /// `Length(min, max, error)`.
    Length {
        min: Option<usize>,
        max: Option<usize>,
        error: Option<&'static str>,
    },
    /// `OneOf(choices, error)`.
    OneOf {
        choices: &'static [&'static str],
        error: Option<&'static str>,
    },
    /// `Range(min, max, min_inclusive, max_inclusive, error)`.
    Range {
        min: Option<f64>,
        max: Option<f64>,
        min_inclusive: bool,
        max_inclusive: bool,
        error: Option<&'static str>,
    },
    /// A custom validator returning the message on failure.
    Custom(fn(&Value) -> Result<(), String>),
}

impl Validator {
    pub const fn one_of(choices: &'static [&'static str]) -> Self {
        Validator::OneOf {
            choices,
            error: None,
        }
    }

    pub const fn length(min: usize, max: usize) -> Self {
        Validator::Length {
            min: Some(min),
            max: Some(max),
            error: None,
        }
    }

    pub const fn min_len(min: usize, error: Option<&'static str>) -> Self {
        Validator::Length {
            min: Some(min),
            max: None,
            error,
        }
    }

    /// `Range(min=..)` (inclusive) with an optional message.
    pub const fn min(min: f64, error: Option<&'static str>) -> Self {
        Validator::Range {
            min: Some(min),
            max: None,
            min_inclusive: true,
            max_inclusive: true,
            error,
        }
    }

    /// `Range(min=.., min_inclusive=False)`.
    pub const fn gt(min: f64, error: Option<&'static str>) -> Self {
        Validator::Range {
            min: Some(min),
            max: None,
            min_inclusive: false,
            max_inclusive: true,
            error,
        }
    }

    pub const fn between(min: f64, max: f64) -> Self {
        Validator::Range {
            min: Some(min),
            max: Some(max),
            min_inclusive: true,
            max_inclusive: true,
            error: None,
        }
    }

    fn check(&self, v: &Value) -> Result<(), String> {
        match self {
            Validator::Length { min, max, error } => {
                let n = match v {
                    Value::String(s) => s.chars().count(),
                    Value::Array(a) => a.len(),
                    _ => return Ok(()),
                };
                let fail =
                    min.map(|m| n < m).unwrap_or(false) || max.map(|m| n > m).unwrap_or(false);
                if !fail {
                    return Ok(());
                }
                let msg = match (min, max) {
                    (Some(a), Some(b)) => format!("Length must be between {} and {}.", a, b),
                    (Some(a), None) => format!("Shorter than minimum length {}.", a),
                    (None, Some(b)) => format!("Longer than maximum length {}.", b),
                    (None, None) => String::new(),
                };
                Err(error.map(|e| e.to_string()).unwrap_or(msg))
            }
            Validator::OneOf { choices, error } => {
                let s = match v {
                    Value::String(s) => s.as_str(),
                    _ => "",
                };
                if choices.contains(&s) && matches!(v, Value::String(_)) {
                    Ok(())
                } else {
                    Err(error
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| format!("Must be one of: {}.", choices.join(", "))))
                }
            }
            Validator::Range {
                min,
                max,
                min_inclusive,
                max_inclusive,
                error,
            } => {
                let Some(x) = v.as_f64() else {
                    return Ok(());
                };
                let lo_fail = min
                    .map(|m| if *min_inclusive { x < m } else { x <= m })
                    .unwrap_or(false);
                let hi_fail = max
                    .map(|m| if *max_inclusive { x > m } else { x >= m })
                    .unwrap_or(false);
                if !lo_fail && !hi_fail {
                    return Ok(());
                }
                if let Some(e) = error {
                    return Err(e.to_string());
                }
                let ge = if *min_inclusive {
                    "greater than or equal to"
                } else {
                    "greater than"
                };
                let le = if *max_inclusive {
                    "less than or equal to"
                } else {
                    "less than"
                };
                Err(match (min, max) {
                    (Some(a), Some(b)) => {
                        format!("Must be {} {} and {} {}.", ge, py_num(*a), le, py_num(*b))
                    }
                    (Some(a), None) => format!("Must be {} {}.", ge, py_num(*a)),
                    (None, Some(b)) => format!("Must be {} {}.", le, py_num(*b)),
                    (None, None) => String::new(),
                })
            }
            Validator::Custom(f) => f(v),
        }
    }
}

/// Field kinds (marshmallow `fields.*`).
#[derive(Clone)]
pub enum Kind {
    Str,
    Float,
    Int,
    Bool,
    /// `fields.Date(format="%Y-%m-%d")`; loaded as the `YYYY-MM-DD` text.
    Date,
    /// `fields.List(fields.Nested(schema))`.
    NestedList(&'static Schema),
}

/// One schema field.
#[derive(Clone)]
pub struct Field {
    pub name: &'static str,
    pub kind: Kind,
    pub required: bool,
    /// `load_default` / `missing`.
    pub default: Option<fn() -> Value>,
    pub allow_none: bool,
    pub validators: Vec<Validator>,
}

impl Field {
    pub fn new(name: &'static str, kind: Kind) -> Self {
        Self {
            name,
            kind,
            required: false,
            default: None,
            allow_none: false,
            validators: Vec::new(),
        }
    }

    pub fn str(name: &'static str) -> Self {
        Self::new(name, Kind::Str)
    }

    pub fn float(name: &'static str) -> Self {
        Self::new(name, Kind::Float)
    }

    pub fn int(name: &'static str) -> Self {
        Self::new(name, Kind::Int)
    }

    pub fn boolean(name: &'static str) -> Self {
        Self::new(name, Kind::Bool)
    }

    pub fn date(name: &'static str) -> Self {
        Self::new(name, Kind::Date)
    }

    pub fn list(name: &'static str, item: &'static Schema) -> Self {
        Self::new(name, Kind::NestedList(item))
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// `missing=`/`load_default=`. A `null` default implies `allow_none`,
    /// as in marshmallow.
    pub fn default(mut self, f: fn() -> Value) -> Self {
        if f().is_null() {
            self.allow_none = true;
        }
        self.default = Some(f);
        self
    }

    pub fn allow_none(mut self) -> Self {
        self.allow_none = true;
        self
    }

    pub fn validate(mut self, v: Validator) -> Self {
        self.validators.push(v);
        self
    }
}

/// What to do with keys the schema does not declare.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Unknown {
    Raise,
    Exclude,
}

/// A `post_load` hook: may rewrite the loaded object or raise field errors.
pub type PostLoad = fn(&mut Map<String, Value>) -> Result<(), FieldErrors>;

/// A marshmallow schema.
pub struct Schema {
    pub fields: Vec<Field>,
    pub unknown: Unknown,
    pub post_load: Option<PostLoad>,
}

impl Schema {
    pub fn new(fields: Vec<Field>) -> Self {
        Self {
            fields,
            unknown: Unknown::Raise,
            post_load: None,
        }
    }

    pub fn exclude_unknown(mut self) -> Self {
        self.unknown = Unknown::Exclude;
        self
    }

    pub fn post_load(mut self, f: PostLoad) -> Self {
        self.post_load = Some(f);
        self
    }

    /// `schema.load(data)`.
    pub fn load(&self, data: &Map<String, Value>) -> Result<Map<String, Value>, FieldErrors> {
        let mut errs = FieldErrors::default();
        let mut out = Map::new();
        for f in &self.fields {
            let key = f.name;
            match data.get(key) {
                None => {
                    if f.required {
                        errs.add(key, "Missing data for required field.");
                    } else if let Some(d) = f.default {
                        out.insert(f.name.to_string(), d());
                    }
                }
                Some(Value::Null) => {
                    if f.allow_none {
                        out.insert(f.name.to_string(), Value::Null);
                    } else {
                        errs.add(key, "Field may not be null.");
                    }
                }
                Some(v) => match deserialize(&f.kind, v) {
                    Ok(loaded) => {
                        let mut msgs = Vec::new();
                        for val in &f.validators {
                            if let Err(m) = val.check(&loaded) {
                                msgs.push(m);
                            }
                        }
                        if msgs.is_empty() {
                            out.insert(f.name.to_string(), loaded);
                        } else {
                            for m in msgs {
                                errs.add(key, m);
                            }
                        }
                    }
                    Err(Fail::Messages(m)) => {
                        for x in m {
                            errs.add(key, x);
                        }
                    }
                    Err(Fail::Nested(n)) => errs.nested(ErrKey::Field(key.to_string()), n),
                },
            }
        }
        if self.unknown == Unknown::Raise {
            let known: Vec<&str> = self.fields.iter().map(|f| f.name).collect();
            for k in data.keys() {
                if !known.contains(&k.as_str()) {
                    errs.add(k, "Unknown field.");
                }
            }
        }
        if !errs.is_empty() {
            return Err(errs);
        }
        if let Some(hook) = self.post_load {
            hook(&mut out)?;
        }
        Ok(out)
    }
}

enum Fail {
    Messages(Vec<String>),
    Nested(FieldErrors),
}

fn fail(m: &str) -> Fail {
    Fail::Messages(vec![m.to_string()])
}

fn float_value(x: f64) -> Value {
    Number::from_f64(x)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// Python `float(value)` for a JSON value.
fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = s.trim().replace('_', "");
            let lower = t.to_ascii_lowercase();
            match lower.as_str() {
                "nan" | "+nan" | "-nan" => Some(f64::NAN),
                "inf" | "+inf" | "infinity" | "+infinity" => Some(f64::INFINITY),
                "-inf" | "-infinity" => Some(f64::NEG_INFINITY),
                _ => t.parse::<f64>().ok(),
            }
        }
        _ => None,
    }
}

fn deserialize(kind: &Kind, v: &Value) -> Result<Value, Fail> {
    match kind {
        Kind::Str => match v {
            Value::String(_) => Ok(v.clone()),
            _ => Err(fail("Not a valid string.")),
        },
        Kind::Float => {
            if v.is_boolean() {
                return Err(fail("Not a valid number."));
            }
            let x = py_float(v).ok_or_else(|| fail("Not a valid number."))?;
            if !x.is_finite() {
                return Err(fail(
                    "Special numeric values (nan or infinity) are not permitted.",
                ));
            }
            Ok(float_value(x))
        }
        Kind::Int => match v {
            Value::Bool(_) => Err(fail("Not a valid integer.")),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(Value::from(i))
                } else if let Some(x) = n.as_f64() {
                    // int(5.7) == 5
                    if x.is_finite() && x.abs() < 9.0e18 {
                        Ok(Value::from(x.trunc() as i64))
                    } else {
                        Err(fail("Not a valid integer."))
                    }
                } else {
                    Err(fail("Not a valid integer."))
                }
            }
            Value::String(s) => s
                .trim()
                .replace('_', "")
                .parse::<i64>()
                .map(Value::from)
                .map_err(|_| fail("Not a valid integer.")),
            _ => Err(fail("Not a valid integer.")),
        },
        Kind::Bool => {
            const TRUTHY: &[&str] = &[
                "t", "T", "true", "True", "TRUE", "on", "On", "ON", "y", "Y", "yes", "Yes", "YES",
                "1",
            ];
            const FALSY: &[&str] = &[
                "f", "F", "false", "False", "FALSE", "off", "Off", "OFF", "n", "N", "no", "No",
                "NO", "0",
            ];
            match v {
                Value::Bool(b) => Ok(Value::Bool(*b)),
                Value::String(s) if TRUTHY.contains(&s.as_str()) => Ok(Value::Bool(true)),
                Value::String(s) if FALSY.contains(&s.as_str()) => Ok(Value::Bool(false)),
                Value::Number(n) if n.as_f64() == Some(1.0) => Ok(Value::Bool(true)),
                Value::Number(n) if n.as_f64() == Some(0.0) => Ok(Value::Bool(false)),
                _ => Err(fail("Not a valid boolean.")),
            }
        }
        Kind::Date => match v {
            Value::String(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(|d| Value::String(d.format("%Y-%m-%d").to_string()))
                .map_err(|_| fail("Not a valid date.")),
            _ => Err(fail("Not a valid date.")),
        },
        Kind::NestedList(item) => {
            let Value::Array(items) = v else {
                return Err(fail("Not a valid list."));
            };
            let mut errs = FieldErrors::default();
            let mut out = Vec::with_capacity(items.len());
            for (i, it) in items.iter().enumerate() {
                match it {
                    Value::Object(m) => match item.load(m) {
                        Ok(x) => out.push(Value::Object(x)),
                        Err(e) => errs.nested(ErrKey::Index(i), e),
                    },
                    _ => errs.nested(
                        ErrKey::Index(i),
                        FieldErrors::single("_schema", "Invalid input type."),
                    ),
                }
            }
            if errs.is_empty() {
                Ok(Value::Array(out))
            } else {
                Err(Fail::Nested(errs))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    fn item() -> &'static Schema {
        static S: std::sync::OnceLock<Schema> = std::sync::OnceLock::new();
        S.get_or_init(|| {
            Schema::new(vec![
                Field::str("symbol").required(),
                Field::str("product")
                    .default(|| json!("MIS"))
                    .validate(Validator::one_of(&["MIS", "NRML", "CNC"])),
            ])
        })
    }

    #[test]
    fn python_repr_matches_marshmallow_messages() {
        let s = Schema::new(vec![
            Field::str("apikey").required(),
            Field::float("quantity").required().validate(Validator::gt(
                0.0,
                Some("Quantity must be a positive number."),
            )),
            Field::list("orders", item()).required(),
        ]);
        let e = s
            .load(&obj(json!({"apikey": "k", "quantity": 0, "orders": [{"symbol": "A", "product": "BAD"}]})))
            .unwrap_err();
        assert_eq!(
            e.to_python(),
            "{'quantity': ['Quantity must be a positive number.'], 'orders': {0: {'product': ['Must be one of: MIS, NRML, CNC.']}}}"
        );
        assert_eq!(
            e.to_json(),
            json!({"quantity": ["Quantity must be a positive number."], "orders": {"0": {"product": ["Must be one of: MIS, NRML, CNC."]}}})
        );
    }

    #[test]
    fn repr_switches_quotes_like_python() {
        assert_eq!(
            py_repr_str("Must be 'SINGLE' or 'OCO'."),
            "\"Must be 'SINGLE' or 'OCO'.\""
        );
        assert_eq!(py_repr_str("plain"), "'plain'");
        assert_eq!(py_repr_str("a'b\"c"), "'a\\'b\"c'");
    }

    #[test]
    fn field_kinds_coerce_like_marshmallow() {
        let s = Schema::new(vec![
            Field::float("f"),
            Field::int("i"),
            Field::boolean("b"),
            Field::date("d"),
        ]);
        let ok = s
            .load(&obj(
                json!({"f": "1.5", "i": 5.7, "b": "yes", "d": "2026-10-03"}),
            ))
            .unwrap();
        assert_eq!(ok["f"], json!(1.5));
        assert_eq!(ok["i"], json!(5));
        assert_eq!(ok["b"], json!(true));
        let e = s
            .load(&obj(
                json!({"f": true, "i": "5.5", "b": "maybe", "d": "03-10-2026", "x": 1}),
            ))
            .unwrap_err();
        assert_eq!(e.messages("f"), ["Not a valid number."]);
        assert_eq!(e.messages("i"), ["Not a valid integer."]);
        assert_eq!(e.messages("b"), ["Not a valid boolean."]);
        assert_eq!(e.messages("d"), ["Not a valid date."]);
        assert_eq!(e.messages("x"), ["Unknown field."]);
        let e = s.load(&obj(json!({"f": "nan"}))).unwrap_err();
        assert_eq!(
            e.messages("f"),
            ["Special numeric values (nan or infinity) are not permitted."]
        );
    }

    #[test]
    fn defaults_null_and_required() {
        let s = Schema::new(vec![
            Field::str("a").required(),
            Field::float("p").default(|| json!(0.0)),
            Field::float("u").default(|| Value::Null),
            Field::str("n"),
        ]);
        let ok = s.load(&obj(json!({"a": "x", "u": null}))).unwrap();
        assert_eq!(ok["p"], json!(0.0));
        assert!(ok["u"].is_null());
        assert!(!ok.contains_key("n"));
        let e = s.load(&obj(json!({"n": null}))).unwrap_err();
        assert_eq!(e.messages("a"), ["Missing data for required field."]);
        assert_eq!(e.messages("n"), ["Field may not be null."]);
    }

    #[test]
    fn validator_messages() {
        let s = Schema::new(vec![
            Field::str("k").validate(Validator::length(1, 256)),
            Field::int("y").validate(Validator::between(2020.0, 2050.0)),
            Field::int("m").validate(Validator::min(1.0, None)),
            Field::list("l", item()).validate(Validator::min_len(1, None)),
        ]);
        let e = s
            .load(&obj(json!({"k": "", "y": 1999, "m": 0, "l": []})))
            .unwrap_err();
        assert_eq!(e.messages("k"), ["Length must be between 1 and 256."]);
        assert_eq!(
            e.messages("y"),
            ["Must be greater than or equal to 2020 and less than or equal to 2050."]
        );
        assert_eq!(e.messages("m"), ["Must be greater than or equal to 1."]);
        assert_eq!(e.messages("l"), ["Shorter than minimum length 1."]);
    }

    #[test]
    fn post_load_runs_only_on_a_clean_load() {
        fn hook(m: &mut Map<String, Value>) -> Result<(), FieldErrors> {
            if m.get("q").and_then(Value::as_f64) == Some(1.5) {
                return Err(FieldErrors::single("q", "fractional"));
            }
            m.insert("seen".into(), json!(true));
            Ok(())
        }
        let s = Schema::new(vec![Field::float("q")]).post_load(hook);
        assert_eq!(
            s.load(&obj(json!({"q": 1.5}))).unwrap_err().messages("q"),
            ["fractional"]
        );
        assert_eq!(s.load(&obj(json!({"q": 2}))).unwrap()["seen"], json!(true));
        let e = s.load(&obj(json!({"q": "x"}))).unwrap_err();
        assert_eq!(e.messages("q"), ["Not a valid number."]);
    }
}
