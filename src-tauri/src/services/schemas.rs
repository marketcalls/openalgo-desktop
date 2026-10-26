//! The web's request schemas (`restx_api/schemas.py`, `data_schemas.py`,
//! `account_schema.py`), field for field, in declaration order.

use super::schema::{Field, FieldErrors, Schema, Validator};
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

pub const VALID_EXCHANGES: &[&str] = &[
    "NSE",
    "NFO",
    "CDS",
    "BSE",
    "BFO",
    "BCD",
    "MCX",
    "NCDEX",
    "NCO",
    "NSE_INDEX",
    "BSE_INDEX",
    "MCX_INDEX",
    "GLOBAL_INDEX",
    "CRYPTO",
];

/// `data_schemas.F_AND_O_EXCHANGES`.
pub const FNO_EXCHANGES: &[&str] = &["NFO", "BFO", "MCX", "CDS", "NCO", "BCD", "NCDEX", "CRYPTO"];

pub const CRYPTO_EXCHANGES: &[&str] = &["CRYPTO"];

const ACTIONS: &[&str] = &["BUY", "SELL", "buy", "sell"];
const PRICETYPES: &[&str] = &["MARKET", "LIMIT", "SL", "SL-M"];
const PRODUCTS: &[&str] = &["MIS", "NRML", "CNC"];
const OPTION_TYPES: &[&str] = &["CE", "PE", "ce", "pe"];

/// `utils.constants.SUPPORTED_INTERVALS`.
pub const SUPPORTED_INTERVALS: &[&str] = &[
    "1s", "5s", "10s", "15s", "30s", "45s", "1m", "2m", "3m", "5m", "10m", "15m", "20m", "30m",
    "1h", "2h", "3h", "4h", "D", "W", "M", "Q", "Y",
];

macro_rules! schema {
    ($name:ident, $body:expr) => {
        pub fn $name() -> &'static Schema {
            static S: OnceLock<Schema> = OnceLock::new();
            S.get_or_init(|| $body)
        }
    };
}

fn apikey() -> Field {
    Field::str("apikey")
        .required()
        .validate(Validator::length(1, 256))
}

fn exchange() -> Field {
    Field::str("exchange")
        .required()
        .validate(Validator::one_of(VALID_EXCHANGES))
}

fn action() -> Field {
    Field::str("action")
        .required()
        .validate(Validator::one_of(ACTIONS))
}

fn pricetype() -> Field {
    Field::str("pricetype")
        .default(|| json!("MARKET"))
        .validate(Validator::one_of(PRICETYPES))
}

fn product() -> Field {
    Field::str("product")
        .default(|| json!("MIS"))
        .validate(Validator::one_of(PRODUCTS))
}

fn price() -> Field {
    Field::float("price")
        .default(|| json!(0.0))
        .validate(Validator::min(
            0.0,
            Some("Price must be a non-negative number."),
        ))
}

fn trigger_price() -> Field {
    Field::float("trigger_price")
        .default(|| json!(0.0))
        .validate(Validator::min(
            0.0,
            Some("Trigger price must be a non-negative number."),
        ))
}

fn disclosed_quantity() -> Field {
    Field::int("disclosed_quantity")
        .default(|| json!(0))
        .validate(Validator::min(
            0.0,
            Some("Disclosed quantity must be a non-negative integer."),
        ))
}

fn positive_quantity(msg: &'static str) -> Field {
    Field::float("quantity")
        .required()
        .validate(Validator::gt(0.0, Some(msg)))
}

/// The largest whole-number quantity a broker takes (a 32-bit integer).
pub(crate) const MAX_BROKER_QUANTITY: i64 = i32::MAX as i64;

/// The refusal for a quantity above [`MAX_BROKER_QUANTITY`].
pub(crate) const QUANTITY_TOO_LARGE: &str = "Quantity is larger than any exchange accepts.";

/// The refusal for a GTT quantity below 1, by the schema and by the GTT
/// page's modify, which reaches the service without one.
pub(crate) const GTT_QUANTITY_NOT_POSITIVE: &str = "Quantity must be a positive number.";

/// `_coerce_quantity_to_int`: fractional quantities only on crypto.
///
/// Desktop addition (API-02): a quantity or disclosed quantity above
/// [`MAX_BROKER_QUANTITY`] is refused here, before anything is dispatched,
/// simulated or logged. Brokers take a 32-bit quantity, and the desktop
/// used to clamp a larger one silently at the broker boundary, so the
/// broker, the sandbox and the order log saw three different quantities.
pub fn coerce_quantity(m: &mut Map<String, Value>) -> Result<(), FieldErrors> {
    let exchange = m.get("exchange").and_then(Value::as_str).unwrap_or("");
    if CRYPTO_EXCHANGES.contains(&exchange) {
        return Ok(());
    }
    let quantity = m.get("quantity").and_then(Value::as_f64);
    if let Some(q) = quantity.filter(|q| q.fract() != 0.0) {
        return Err(FieldErrors::single(
            "quantity",
            format!(
                "Fractional quantity ({}) is not allowed for non-crypto exchanges.",
                py_float_repr(q)
            ),
        ));
    }
    cap_quantities(m)?;
    if let Some(q) = quantity {
        m.insert("quantity".into(), json!(q as i64));
    }
    Ok(())
}

/// The cap of [`coerce_quantity`] (API-02): a quantity or disclosed
/// quantity above [`MAX_BROKER_QUANTITY`] is refused. The options schemas
/// apply it on every exchange: their quantities are whole numbers, which
/// the order path would refuse above the cap anyway, after resolving the
/// contract.
fn cap_quantities(m: &mut Map<String, Value>) -> Result<(), FieldErrors> {
    let over = |k: &str| {
        m.get(k)
            .and_then(Value::as_f64)
            .is_some_and(|q| q > MAX_BROKER_QUANTITY as f64)
    };
    if over("quantity") {
        return Err(FieldErrors::single("quantity", QUANTITY_TOO_LARGE));
    }
    if over("disclosed_quantity") {
        return Err(FieldErrors::single(
            "disclosed_quantity",
            "Disclosed quantity is larger than any exchange accepts.",
        ));
    }
    Ok(())
}

/// The refusal for a `position_size` above [`MAX_BROKER_QUANTITY`].
pub(crate) const POSITION_TOO_LARGE: &str = "Position size is larger than any exchange accepts.";

/// `coerce_quantity`, and the target position held to what an exchange
/// takes. Desktop addition: outside CRYPTO a fractional `position_size`, or
/// one above [`MAX_BROKER_QUANTITY`], is refused like a fractional
/// quantity. The web keeps it a float and its brokers and sandbox truncate
/// it (`int(position_size)`), so 0.5 became 0 and closed the position.
fn coerce_smart_order(m: &mut Map<String, Value>) -> Result<(), FieldErrors> {
    coerce_quantity(m)?;
    let exchange = m.get("exchange").and_then(Value::as_str).unwrap_or("");
    if CRYPTO_EXCHANGES.contains(&exchange) {
        return Ok(());
    }
    let Some(p) = m.get("position_size").and_then(Value::as_f64) else {
        return Ok(());
    };
    if p.fract() != 0.0 {
        return Err(FieldErrors::single(
            "position_size",
            format!(
                "Fractional position size ({}) is not allowed for non-crypto exchanges.",
                py_float_repr(p)
            ),
        ));
    }
    if p.abs() > MAX_BROKER_QUANTITY as f64 {
        return Err(FieldErrors::single("position_size", POSITION_TOO_LARGE));
    }
    Ok(())
}

/// Python `repr(float)` for the values a client sends (`1.5`, `2.25`).
pub fn py_float_repr(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e16 {
        format!("{:.1}", x)
    } else {
        format!("{}", x)
    }
}

schema!(
    order,
    Schema::new(vec![
        apikey(),
        Field::str("strategy").required(),
        exchange(),
        Field::str("symbol").required(),
        action(),
        positive_quantity("Quantity must be a positive number."),
        pricetype(),
        product(),
        price(),
        trigger_price(),
        disclosed_quantity(),
        Field::float("underlying_ltp").default(|| Value::Null),
    ])
    .post_load(coerce_quantity)
);

schema!(
    smart_order,
    Schema::new(vec![
        apikey(),
        Field::str("strategy").required(),
        exchange(),
        Field::str("symbol").required(),
        action(),
        Field::float("quantity").required().validate(Validator::min(
            0.0,
            Some("Quantity must be a non-negative number.")
        )),
        Field::float("position_size").required(),
        pricetype(),
        product(),
        price(),
        trigger_price(),
        disclosed_quantity(),
    ])
    .post_load(coerce_smart_order)
);

schema!(
    modify_order,
    Schema::new(vec![
        apikey(),
        Field::str("strategy").required(),
        exchange(),
        Field::str("symbol").required(),
        Field::str("orderid").required(),
        action(),
        Field::str("product")
            .required()
            .validate(Validator::one_of(PRODUCTS)),
        Field::str("pricetype")
            .required()
            .validate(Validator::one_of(PRICETYPES)),
        Field::float("price").required().validate(Validator::min(
            0.0,
            Some("Price must be a non-negative number.")
        )),
        positive_quantity("Quantity must be a positive number."),
        Field::int("disclosed_quantity")
            .required()
            .validate(Validator::min(
                0.0,
                Some("Disclosed quantity must be a non-negative integer.")
            )),
        Field::float("trigger_price")
            .required()
            .validate(Validator::min(
                0.0,
                Some("Trigger price must be a non-negative number.")
            )),
    ])
    .post_load(coerce_quantity)
);

schema!(
    cancel_order,
    Schema::new(vec![
        apikey(),
        Field::str("strategy").required(),
        Field::str("orderid").required(),
    ])
);

schema!(
    strategy_only,
    Schema::new(vec![apikey(), Field::str("strategy").required()])
);

schema!(
    basket_item,
    Schema::new(vec![
        exchange(),
        Field::str("symbol").required(),
        action(),
        positive_quantity("Quantity must be a positive number."),
        pricetype(),
        product(),
        price(),
        trigger_price(),
        disclosed_quantity(),
    ])
    .post_load(coerce_quantity)
);

schema!(
    basket_order,
    Schema::new(vec![
        apikey(),
        Field::str("strategy").required(),
        Field::list("orders", basket_item())
            .required()
            .validate(Validator::min_len(
                1,
                Some("Orders must contain at least 1 item.")
            )),
    ])
);

schema!(
    split_order,
    Schema::new(vec![
        apikey(),
        Field::str("strategy").required(),
        exchange(),
        Field::str("symbol").required(),
        action(),
        positive_quantity("Total quantity must be a positive number."),
        Field::int("splitsize").required().validate(Validator::min(
            1.0,
            Some("Split size must be a positive integer.")
        )),
        pricetype(),
        product(),
        price(),
        trigger_price(),
        disclosed_quantity(),
    ])
    .post_load(coerce_quantity)
);

fn option_product() -> Field {
    Field::str("product")
        .default(|| json!("MIS"))
        .validate(Validator::one_of(&["MIS", "NRML"]))
}

fn splitsize() -> Field {
    Field::int("splitsize")
        .default(|| json!(0))
        .allow_none()
        .validate(Validator::min(
            0.0,
            Some("Split size must be a non-negative integer."),
        ))
}

fn strike_int() -> Field {
    Field::int("strike_int")
        .allow_none()
        .validate(Validator::min(1.0, None))
}

schema!(
    options_order,
    Schema::new(vec![
        apikey(),
        Field::str("strategy").required(),
        Field::str("underlying").required(),
        exchange(),
        Field::str("expiry_date"),
        strike_int(),
        Field::str("offset").required(),
        Field::str("option_type")
            .required()
            .validate(Validator::one_of(OPTION_TYPES)),
        action(),
        Field::int("quantity").required().validate(Validator::min(
            1.0,
            Some("Quantity must be a positive integer.")
        )),
        splitsize(),
        pricetype(),
        option_product(),
        price(),
        trigger_price(),
        disclosed_quantity(),
    ])
    .post_load(cap_quantities)
);

schema!(
    options_leg,
    Schema::new(vec![
        Field::str("offset").required(),
        Field::str("option_type")
            .required()
            .validate(Validator::one_of(OPTION_TYPES)),
        action(),
        Field::int("quantity").required().validate(Validator::min(
            1.0,
            Some("Quantity must be a positive integer.")
        )),
        splitsize(),
        Field::str("expiry_date"),
        pricetype(),
        option_product(),
        price(),
        trigger_price(),
        disclosed_quantity(),
    ])
    .post_load(cap_quantities)
);

schema!(
    options_multi_order,
    Schema::new(vec![
        apikey(),
        Field::str("strategy").required(),
        Field::str("underlying").required(),
        exchange(),
        Field::str("expiry_date"),
        strike_int(),
        Field::list("legs", options_leg())
            .required()
            .validate(Validator::Length {
                min: Some(1),
                max: Some(20),
                error: Some("Legs must contain 1 to 20 items."),
            }),
    ])
);

schema!(
    synthetic_future,
    Schema::new(vec![
        apikey(),
        Field::str("underlying").required(),
        exchange(),
        Field::str("expiry_date").required(),
    ])
);

schema!(
    margin_position,
    Schema::new(vec![
        Field::str("symbol").required().validate(Validator::Length {
            min: Some(1),
            max: Some(50),
            error: Some("Symbol must be between 1 and 50 characters."),
        }),
        exchange(),
        action(),
        Field::str("quantity").required(),
        Field::str("product")
            .required()
            .validate(Validator::one_of(PRODUCTS)),
        Field::str("pricetype")
            .required()
            .validate(Validator::one_of(PRICETYPES)),
        Field::str("price").default(|| json!("0")),
        Field::str("trigger_price").default(|| json!("0")),
    ])
);

schema!(
    margin,
    Schema::new(vec![
        Field::str("apikey").required().validate(Validator::Length {
            min: Some(1),
            max: Some(256),
            error: Some("API key must be between 1 and 256 characters."),
        }),
        Field::list("positions", margin_position())
            .required()
            .validate(Validator::Length {
                min: Some(1),
                max: Some(50),
                error: Some("Positions must contain 1 to 50 items."),
            }),
    ])
);

// ------------------------------------------------------------------ GTT

fn is_zero_or_none(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(x) => x.as_f64() == Some(0.0),
    }
}

/// `_validate_gtt_place_request`.
pub fn gtt_post_process(m: &mut Map<String, Value>) -> Result<(), FieldErrors> {
    let trigger_type = m
        .get("trigger_type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_uppercase();
    if trigger_type != "SINGLE" && trigger_type != "OCO" {
        return Err(FieldErrors::single(
            "trigger_type",
            "Must be 'SINGLE' or 'OCO'.",
        ));
    }
    m.insert("trigger_type".into(), json!(trigger_type));
    let sl = m.get("triggerprice_sl").cloned();
    let tg = m.get("triggerprice_tg").cloned();
    if trigger_type == "OCO" {
        if is_zero_or_none(sl.as_ref()) {
            return Err(FieldErrors::single(
                "triggerprice_sl",
                "Required for OCO (stoploss trigger).",
            ));
        }
        if is_zero_or_none(m.get("stoploss")) {
            return Err(FieldErrors::single(
                "stoploss",
                "Required for OCO (stoploss leg limit).",
            ));
        }
        if is_zero_or_none(tg.as_ref()) {
            return Err(FieldErrors::single(
                "triggerprice_tg",
                "Required for OCO (target trigger).",
            ));
        }
        if is_zero_or_none(m.get("target")) {
            return Err(FieldErrors::single(
                "target",
                "Required for OCO (target leg limit).",
            ));
        }
        let (s, t) = (
            sl.as_ref().and_then(Value::as_f64).unwrap_or(0.0),
            tg.as_ref().and_then(Value::as_f64).unwrap_or(0.0),
        );
        if s >= t {
            return Err(FieldErrors::single(
                "triggerprice_sl",
                "Stoploss trigger must be less than target trigger (triggerprice_tg).",
            ));
        }
        m.insert("trigger_price".into(), json!(t));
    } else {
        let s = sl.as_ref().and_then(Value::as_f64).unwrap_or(0.0);
        let t = tg.as_ref().and_then(Value::as_f64).unwrap_or(0.0);
        if s <= 0.0 && t <= 0.0 {
            return Err(FieldErrors::single(
                "triggerprice_sl",
                "SINGLE GTT requires a positive triggerprice_sl or triggerprice_tg.",
            ));
        }
        let resolved = if s > 0.0 { s } else { t };
        m.insert(
            "triggerprice_sl".into(),
            if s > 0.0 { json!(s) } else { Value::Null },
        );
        m.insert(
            "triggerprice_tg".into(),
            if s <= 0.0 { json!(t) } else { Value::Null },
        );
        m.insert("stoploss".into(), Value::Null);
        m.insert("target".into(), Value::Null);
        m.insert("trigger_price".into(), json!(resolved));
    }
    // placeorder's rule for the quantity: whole outside CRYPTO. The cap
    // holds on CRYPTO too (PROD-TR-05): no GTT takes a fraction, so a GTT is
    // whole units on every exchange.
    coerce_quantity(m)?;
    cap_quantities(m)?;
    if let Some(a) = m.get("action").and_then(Value::as_str) {
        let up = a.to_ascii_uppercase();
        m.insert("action".into(), json!(up));
    }
    Ok(())
}

/// `pre_load coerce_empty_to_none`.
pub fn gtt_pre_load(m: &mut Map<String, Value>) {
    for k in ["stoploss", "target", "triggerprice_sl", "triggerprice_tg"] {
        if m.get(k) == Some(&json!("")) {
            let v = if k == "stoploss" || k == "target" {
                Value::Null
            } else {
                json!(0.0)
            };
            m.insert(k.into(), v);
        }
    }
}

fn gtt_fields(with_trigger_id: bool, with_expiry: bool) -> Vec<Field> {
    let mut v = vec![apikey(), Field::str("strategy").required()];
    if with_trigger_id {
        v.push(
            Field::str("trigger_id")
                .required()
                .validate(Validator::Length {
                    min: Some(1),
                    max: None,
                    error: None,
                }),
        );
    }
    v.extend([
        Field::str("trigger_type").required(),
        exchange(),
        Field::str("symbol").required(),
        action(),
        Field::str("product").required().validate(Validator::OneOf {
            choices: &["NRML", "CNC"],
            error: Some(
                "GTT supports only CNC (delivery) or NRML (overnight F&O); MIS is intraday-only.",
            ),
        }),
        positive_quantity(GTT_QUANTITY_NOT_POSITIVE),
        Field::str("pricetype")
            .default(|| json!("LIMIT"))
            .validate(Validator::one_of(&["LIMIT", "MARKET"])),
        Field::float("price").required().validate(Validator::min(
            0.0,
            Some("Price must be a non-negative number."),
        )),
        Field::float("triggerprice_sl")
            .default(|| json!(0.0))
            .validate(Validator::min(
                0.0,
                Some("triggerprice_sl must be non-negative."),
            )),
        Field::float("triggerprice_tg")
            .default(|| json!(0.0))
            .validate(Validator::min(
                0.0,
                Some("triggerprice_tg must be non-negative."),
            )),
        Field::float("stoploss").default(|| Value::Null),
        Field::float("target").default(|| Value::Null),
    ]);
    if with_expiry {
        v.push(Field::str("expires_at").default(|| Value::Null));
    }
    v
}

schema!(
    place_gtt,
    Schema::new(gtt_fields(false, true))
        .exclude_unknown()
        .post_load(gtt_post_process)
);

schema!(
    modify_gtt,
    Schema::new(gtt_fields(true, false))
        .exclude_unknown()
        .post_load(gtt_post_process)
);

schema!(
    cancel_gtt,
    Schema::new(vec![
        apikey(),
        Field::str("strategy").required(),
        Field::str("trigger_id")
            .required()
            .validate(Validator::Length {
                min: Some(1),
                max: None,
                error: None,
            }),
    ])
);

schema!(
    gtt_orderbook,
    Schema::new(vec![
        apikey(),
        Field::str("status")
            .default(|| json!("active"))
            .validate(Validator::one_of(&["active", "all"])),
    ])
);

// ------------------------------------------------------------------ account

schema!(apikey_only, Schema::new(vec![apikey()]));

schema!(
    order_status,
    Schema::new(vec![
        apikey(),
        Field::str("strategy").required(),
        Field::str("orderid").required(),
    ])
);

schema!(
    open_position,
    Schema::new(vec![
        apikey(),
        Field::str("strategy").required(),
        Field::str("symbol").required(),
        Field::str("exchange").required(),
        Field::str("product")
            .required()
            .validate(Validator::one_of(PRODUCTS)),
    ])
);

schema!(
    analyzer_toggle,
    Schema::new(vec![apikey(), Field::boolean("mode").required()])
);

// ------------------------------------------------------------------ data

schema!(
    quotes,
    Schema::new(vec![apikey(), Field::str("symbol").required(), exchange()])
);

schema!(
    symbol_exchange_pair,
    Schema::new(vec![Field::str("symbol").required(), exchange()])
);

schema!(
    multiquotes,
    Schema::new(vec![
        apikey(),
        Field::list("symbols", symbol_exchange_pair())
            .required()
            .validate(Validator::min_len(1, None)),
    ])
);

schema!(
    history,
    Schema::new(vec![
        apikey(),
        Field::str("symbol").required(),
        exchange(),
        Field::str("interval")
            .required()
            .validate(Validator::one_of(SUPPORTED_INTERVALS)),
        Field::date("start_date").required(),
        Field::date("end_date").required(),
        Field::str("source")
            .default(|| json!("api"))
            .validate(Validator::one_of(&["api", "db"])),
    ])
);

schema!(
    search,
    Schema::new(vec![
        apikey(),
        Field::str("query").required(),
        Field::str("exchange").validate(Validator::one_of(VALID_EXCHANGES)),
    ])
);

schema!(
    expiry,
    Schema::new(vec![
        apikey(),
        Field::str("symbol").required(),
        Field::str("exchange")
            .required()
            .validate(Validator::one_of(FNO_EXCHANGES)),
        Field::str("instrumenttype")
            .required()
            .validate(Validator::one_of(&["futures", "options"])),
    ])
);

/// `validate_option_offset`.
pub fn option_offset_valid(s: &str) -> bool {
    let up = s.to_ascii_uppercase();
    if up == "ATM" {
        return true;
    }
    let rest = up.strip_prefix("ITM").or_else(|| up.strip_prefix("OTM"));
    match rest {
        Some(n) => {
            !n.is_empty()
                && !n.starts_with('0')
                && n.chars().all(|c| c.is_ascii_digit())
                && n.parse::<u32>()
                    .map(|x| (1..=50).contains(&x))
                    .unwrap_or(false)
        }
        None => false,
    }
}

fn offset_validator(v: &Value) -> Result<(), String> {
    match v.as_str() {
        Some(s) if option_offset_valid(s) => Ok(()),
        _ => Err("Offset must be ATM, ITM1-ITM50, or OTM1-OTM50".to_string()),
    }
}

schema!(
    option_symbol,
    Schema::new(vec![
        apikey(),
        Field::str("strategy").allow_none(),
        Field::str("underlying").required(),
        exchange(),
        Field::str("expiry_date"),
        strike_int(),
        Field::str("offset")
            .required()
            .validate(Validator::Custom(offset_validator)),
        Field::str("option_type")
            .required()
            .validate(Validator::one_of(OPTION_TYPES)),
    ])
);

schema!(
    option_greeks,
    Schema::new(vec![
        apikey(),
        Field::str("symbol").required(),
        Field::str("exchange")
            .required()
            .validate(Validator::one_of(FNO_EXCHANGES)),
        Field::float("interest_rate").validate(Validator::between(0.0, 100.0)),
        Field::float("forward_price").validate(Validator::min(0.0, None)),
        Field::str("underlying_symbol"),
        Field::str("underlying_exchange"),
        Field::str("expiry_time"),
    ])
);

schema!(
    instruments,
    Schema::new(vec![
        apikey(),
        Field::str("exchange").validate(Validator::one_of(VALID_EXCHANGES)),
        Field::str("format").validate(Validator::one_of(&["json", "csv"])),
    ])
);

schema!(
    option_chain,
    Schema::new(vec![
        apikey(),
        Field::str("underlying").required(),
        exchange(),
        Field::str("expiry_date").required(),
        Field::int("strike_count")
            .allow_none()
            .validate(Validator::between(1.0, 100.0)),
        Field::boolean("with_greeks").default(|| json!(false)),
        Field::float("interest_rate")
            .allow_none()
            .validate(Validator::between(0.0, 100.0)),
    ])
);

schema!(
    option_symbol_request,
    Schema::new(vec![
        Field::str("symbol").required(),
        Field::str("exchange")
            .required()
            .validate(Validator::one_of(FNO_EXCHANGES)),
        Field::str("underlying_symbol"),
        Field::str("underlying_exchange"),
    ])
);

schema!(
    multi_option_greeks,
    Schema::new(vec![
        apikey(),
        Field::list("symbols", option_symbol_request())
            .required()
            .validate(Validator::length(1, 50)),
        Field::float("interest_rate").validate(Validator::between(0.0, 100.0)),
        Field::str("expiry_time"),
    ])
);

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn order_schema_messages_match_fixtures() {
        let base = json!({"apikey": "k", "strategy": "s", "exchange": "NSE", "symbol": "SBIN",
            "action": "BUY", "quantity": 1, "pricetype": "MARKET", "product": "MIS"});
        let with = |k: &str, v: Value| {
            let mut m = obj(base.clone());
            m.insert(k.into(), v);
            m
        };
        let e = order().load(&with("quantity", json!(1.5))).unwrap_err();
        assert_eq!(
            e.to_python(),
            "{'quantity': ['Fractional quantity (1.5) is not allowed for non-crypto exchanges.']}"
        );
        let e = order().load(&with("quantity", json!(0))).unwrap_err();
        assert_eq!(
            e.to_python(),
            "{'quantity': ['Quantity must be a positive number.']}"
        );
        let e = order()
            .load(&with("exchange", json!("NASDAQ")))
            .unwrap_err();
        assert_eq!(e.to_python(), "{'exchange': ['Must be one of: NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX, CRYPTO.']}");
        let ok = order().load(&with("action", json!("buy"))).unwrap();
        assert_eq!(ok["quantity"], json!(1));
        assert_eq!(ok["price"], json!(0.0));
        assert!(ok["underlying_ltp"].is_null());
        // Crypto keeps fractions.
        let ok = order()
            .load(&obj(
                json!({"apikey": "k", "strategy": "s", "exchange": "CRYPTO", "symbol": "BTC",
                "action": "BUY", "quantity": 0.5}),
            ))
            .unwrap();
        assert_eq!(ok["quantity"], json!(0.5));
    }

    /// A target position outside CRYPTO is whole and within what an
    /// exchange takes; the web truncates 0.5 to 0 (a closed position).
    #[test]
    fn smart_order_position_size_is_whole_outside_crypto() {
        let base = json!({"apikey": "k", "strategy": "s", "exchange": "NSE", "symbol": "SBIN",
            "action": "BUY", "quantity": 1, "position_size": 5});
        let with = |k: &str, v: Value| {
            let mut m = obj(base.clone());
            m.insert(k.into(), v);
            m
        };
        let e = smart_order()
            .load(&with("position_size", json!(0.5)))
            .unwrap_err();
        assert_eq!(
            e.to_python(),
            "{'position_size': ['Fractional position size (0.5) is not allowed for non-crypto exchanges.']}"
        );
        for big in [
            json!(2_147_483_648u64),
            json!(-2_147_483_648i64),
            json!(1e30),
        ] {
            let e = smart_order().load(&with("position_size", big)).unwrap_err();
            assert_eq!(e.messages("position_size"), [POSITION_TOO_LARGE]);
        }
        let ok = smart_order()
            .load(&with("position_size", json!(-3)))
            .unwrap();
        assert_eq!(ok["position_size"], json!(-3.0));
        // CRYPTO keeps its exact target for the live broker.
        let mut crypto = with("position_size", json!(0.5));
        crypto.insert("exchange".into(), json!("CRYPTO"));
        assert_eq!(
            smart_order().load(&crypto).unwrap()["position_size"],
            json!(0.5)
        );
    }

    /// API-02 in the options and GTT schemas: placeorder's cap and error.
    #[test]
    fn options_and_gtt_quantities_are_capped() {
        let big = json!(2_147_483_648u64);
        let options = json!({"apikey": "k", "strategy": "s", "underlying": "NIFTY",
            "exchange": "NSE_INDEX", "offset": "ATM", "option_type": "CE", "action": "BUY",
            "quantity": 65});
        let with = |base: &Value, k: &str, v: &Value| {
            let mut m = obj(base.clone());
            m.insert(k.into(), v.clone());
            m
        };
        let e = options_order()
            .load(&with(&options, "quantity", &big))
            .unwrap_err();
        assert_eq!(e.to_json(), json!({"quantity": [QUANTITY_TOO_LARGE]}));
        let e = options_order()
            .load(&with(&options, "disclosed_quantity", &big))
            .unwrap_err();
        assert_eq!(
            e.messages("disclosed_quantity"),
            ["Disclosed quantity is larger than any exchange accepts."]
        );
        let leg = json!({"offset": "ATM", "option_type": "CE", "action": "BUY", "quantity": big});
        let multi = json!({"apikey": "k", "strategy": "s", "underlying": "NIFTY",
            "exchange": "NSE_INDEX", "legs": [leg]});
        assert_eq!(
            options_multi_order()
                .load(&obj(multi))
                .unwrap_err()
                .to_json(),
            json!({"legs": {"0": {"quantity": [QUANTITY_TOO_LARGE]}}})
        );
        let largest = json!(MAX_BROKER_QUANTITY);
        assert!(options_order()
            .load(&with(&options, "quantity", &largest))
            .is_ok());

        let gtt = json!({"apikey": "k", "strategy": "s", "trigger_type": "SINGLE",
            "exchange": "NSE", "symbol": "SBIN", "action": "buy", "product": "CNC",
            "quantity": 1, "price": 900, "triggerprice_sl": 905});
        // CRYPTO too (PROD-TR-05): a GTT is whole units on every exchange.
        let crypto = with(&gtt, "exchange", &json!("CRYPTO"));
        for schema in [place_gtt(), modify_gtt()] {
            for base in [&gtt, &Value::Object(crypto.clone())] {
                let mut m = with(base, "quantity", &big);
                m.insert("trigger_id".into(), json!("GTT-1"));
                assert_eq!(
                    schema.load(&m).unwrap_err().to_python(),
                    "{'quantity': ['Quantity is larger than any exchange accepts.']}"
                );
            }
        }
        let ok = place_gtt().load(&with(&gtt, "quantity", &largest)).unwrap();
        assert_eq!(ok["quantity"], largest);
    }

    #[test]
    fn gtt_rules() {
        let base = json!({"apikey": "k", "strategy": "s", "trigger_type": "SINGLE",
            "exchange": "NSE", "symbol": "SBIN", "action": "buy", "product": "CNC",
            "quantity": 1, "price": 900, "triggerprice_sl": 905});
        let ok = place_gtt().load(&obj(base.clone())).unwrap();
        assert_eq!(ok["trigger_price"], json!(905.0));
        assert_eq!(ok["action"], json!("BUY"));
        assert!(ok["triggerprice_tg"].is_null());
        let mut m = obj(base.clone());
        m.insert("trigger_type".into(), json!("BOTH"));
        assert_eq!(
            place_gtt().load(&m).unwrap_err().to_python(),
            "{'trigger_type': [\"Must be 'SINGLE' or 'OCO'.\"]}"
        );
        let mut m = obj(base.clone());
        m.insert("trigger_type".into(), json!("OCO"));
        assert_eq!(
            place_gtt().load(&m).unwrap_err().to_python(),
            "{'stoploss': ['Required for OCO (stoploss leg limit).']}"
        );
        let mut m = obj(base);
        m.insert("product".into(), json!("MIS"));
        m.insert("unknown".into(), json!(1));
        assert_eq!(
            place_gtt().load(&m).unwrap_err().to_python(),
            "{'product': ['GTT supports only CNC (delivery) or NRML (overnight F&O); MIS is intraday-only.']}"
        );
    }

    #[test]
    fn offsets() {
        for ok in ["ATM", "itm1", "OTM50", "ITM10"] {
            assert!(option_offset_valid(ok), "{}", ok);
        }
        for bad in ["ITM0", "OTM51", "ITM", "XYZ", "ITM01", "OTM1.5"] {
            assert!(!option_offset_valid(bad), "{}", bad);
        }
    }
}
