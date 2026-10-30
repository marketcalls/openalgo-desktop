//! How a node reaches OpenAlgo: the web's `FlowOpenAlgoClient`
//! (`services/flow_openalgo_client.py`), method for method.
//!
//! Every method answers the web's `_handle_response` dict: on success
//! `{"status": "success", ...every field of the reply but status}`, on
//! failure `{"status": "error", "error": <the reply's message>, "code":
//! <HTTP status>}`. Nodes read those dicts exactly as the web's do.
//!
//! The trait has three required methods: [`FlowClient::api`], one
//! `/api/v1` call, and the two alert senders. Every named method is a
//! provided method that builds the payload the web's client builds and goes
//! through `api`, so the in-app client ([`app::AppFlowClient`]) and the
//! test fake ([`fake::FakeFlowClient`]) agree on the payloads by
//! construction.
//!
//! One addition to the web's dicts: an order call whose answer was lost, or
//! whose placement the broker did not confirm, carries
//! `"error_type": "unknown_outcome"` (the desktop's three outcomes,
//! `brokers/common/outcome`): it may have been placed, so a node must say so
//! and never place it again.

pub mod app;
#[cfg(any(test, feature = "test-support"))]
pub mod fake;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

/// `error_type` of an answer whose effect at the broker is unknown.
pub use crate::mcp::dispatch::UNKNOWN_OUTCOME;

/// One `/api/v1` call: the web client's method name (for logs and the
/// fake's record), the endpoint and the JSON body without the API key.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiCall {
    pub method: &'static str,
    pub endpoint: &'static str,
    pub payload: Map<String, Value>,
}

impl ApiCall {
    pub fn new(method: &'static str, endpoint: &'static str, payload: Value) -> Self {
        Self {
            method,
            endpoint,
            payload: match payload {
                Value::Object(m) => m,
                _ => Map::new(),
            },
        }
    }
}

/// A regular order (`place_order`), with the web client's defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaceOrder {
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub price_type: String,
    pub product_type: String,
    pub price: f64,
    pub trigger_price: f64,
    pub disclosed_quantity: i64,
    pub strategy: String,
}

impl PlaceOrder {
    /// The web's defaults: MARKET, MIS, no prices, `flow_workflow`.
    pub fn new(symbol: &str, exchange: &str, action: &str, quantity: i64) -> Self {
        Self {
            symbol: symbol.into(),
            exchange: exchange.into(),
            action: action.into(),
            quantity,
            price_type: "MARKET".into(),
            product_type: "MIS".into(),
            price: 0.0,
            trigger_price: 0.0,
            disclosed_quantity: 0,
            strategy: DEFAULT_STRATEGY.into(),
        }
    }
}

/// A smart order (`place_smart_order`): a regular order plus the target
/// position size.
#[derive(Debug, Clone, PartialEq)]
pub struct SmartOrder {
    pub order: PlaceOrder,
    pub position_size: i64,
}

/// A modification (`modify_order`): the order's new fields and its id.
#[derive(Debug, Clone, PartialEq)]
pub struct ModifyOrder {
    pub order_id: String,
    pub order: PlaceOrder,
}

/// A split order (`split_order`): a regular order cut into `split_size`
/// pieces.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitOrder {
    pub order: PlaceOrder,
    pub split_size: i64,
}

/// A history request (`get_history`); `source` is `api` or `db`.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    pub symbol: String,
    pub exchange: String,
    pub interval: String,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub source: String,
}

/// An options order (`options_order`), web defaults NRML and MARKET.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionsOrder {
    pub underlying: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub expiry_date: String,
    pub offset: String,
    pub option_type: String,
    pub price_type: String,
    pub product: String,
    pub price: f64,
    pub splitsize: i64,
    pub strategy: String,
    pub trigger_price: f64,
}

/// A multi-leg options order (`options_multi_order`).
#[derive(Debug, Clone, PartialEq)]
pub struct OptionsMultiOrder {
    pub underlying: String,
    pub exchange: String,
    pub expiry_date: String,
    pub legs: Vec<Value>,
    pub strategy: String,
    pub strike_int: Option<i64>,
}

/// A margin request (`margin`): one position, or `positions` for a basket.
#[derive(Debug, Clone, PartialEq)]
pub struct Margin {
    pub symbol: String,
    pub exchange: String,
    pub quantity: i64,
    /// The price as the node read it, sent as Python's `str()` of it: a
    /// whole `0` when the node has none (`"0"`), the number when set
    /// (`"812.5"`, `"100.0"`), as the web's `str(price)` writes it.
    pub price: Value,
    pub product_type: String,
    pub action: String,
    pub price_type: String,
    pub positions: Option<Vec<Value>>,
}

/// The strategy tag the web's client defaults to.
pub const DEFAULT_STRATEGY: &str = "flow_workflow";

fn order_body(o: &PlaceOrder) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("strategy".into(), json!(o.strategy));
    m.insert("symbol".into(), json!(o.symbol));
    m.insert("exchange".into(), json!(o.exchange));
    m.insert("action".into(), json!(o.action.to_uppercase()));
    m.insert("quantity".into(), json!(o.quantity));
    m.insert("pricetype".into(), json!(o.price_type));
    m.insert("product".into(), json!(o.product_type));
    m.insert("price".into(), json!(o.price));
    m.insert("trigger_price".into(), json!(o.trigger_price));
    m
}

/// The web's `FlowOpenAlgoClient`, over whatever answers [`FlowClient::api`].
#[async_trait]
pub trait FlowClient: Send + Sync {
    /// One `/api/v1` call, answered as the web's `_handle_response` dict.
    async fn api(&self, call: ApiCall) -> Value;

    /// A Telegram alert to the trader's linked account (web `telegram`).
    async fn telegram(&self, message: &str) -> Value;

    /// A WhatsApp text through the paired device; `to` empty or `None`
    /// sends to the paired number itself (web `whatsapp`).
    async fn whatsapp(&self, message: &str, to: Option<&str>) -> Value;

    // --- Orders -----------------------------------------------------------

    async fn place_order(&self, o: &PlaceOrder) -> Value {
        let mut body = order_body(o);
        body.insert("disclosed_quantity".into(), json!(o.disclosed_quantity));
        self.api(ApiCall::new(
            "place_order",
            "placeorder",
            Value::Object(body),
        ))
        .await
    }

    async fn place_smart_order(&self, o: &SmartOrder) -> Value {
        let mut body = order_body(&o.order);
        body.insert("position_size".into(), json!(o.position_size));
        self.api(ApiCall::new(
            "place_smart_order",
            "placesmartorder",
            Value::Object(body),
        ))
        .await
    }

    async fn modify_order(&self, o: &ModifyOrder) -> Value {
        let mut body = order_body(&o.order);
        body.insert("orderid".into(), json!(o.order_id));
        body.insert(
            "disclosed_quantity".into(),
            json!(o.order.disclosed_quantity),
        );
        self.api(ApiCall::new(
            "modify_order",
            "modifyorder",
            Value::Object(body),
        ))
        .await
    }

    /// The web passes no strategy here (its service ignores one); the
    /// desktop's endpoint requires one, so it is the client's default
    /// (parity CL-07).
    async fn cancel_order(&self, order_id: &str) -> Value {
        self.api(ApiCall::new(
            "cancel_order",
            "cancelorder",
            json!({"orderid": order_id, "strategy": DEFAULT_STRATEGY}),
        ))
        .await
    }

    async fn cancel_all_orders(&self) -> Value {
        self.api(ApiCall::new(
            "cancel_all_orders",
            "cancelallorder",
            json!({"strategy": DEFAULT_STRATEGY}),
        ))
        .await
    }

    /// Square off every open position.
    async fn close_all_positions(&self) -> Value {
        self.api(ApiCall::new(
            "close_all_positions",
            "closeposition",
            json!({"strategy": DEFAULT_STRATEGY}),
        ))
        .await
    }

    /// The web's `close_position(symbol, exchange, product)`. Note: the
    /// `/api/v1/closeposition` it reaches squares off every position in
    /// both modes (the web's live path does too); a node that must close
    /// one symbol only cannot rely on the filter fields.
    async fn close_position(
        &self,
        symbol: &str,
        exchange: &str,
        product_type: &str,
        strategy: &str,
    ) -> Value {
        self.api(ApiCall::new(
            "close_position",
            "closeposition",
            json!({"strategy": strategy, "symbol": symbol, "exchange": exchange, "product": product_type}),
        ))
        .await
    }

    async fn basket_order(&self, orders: &[Value], strategy: &str) -> Value {
        self.api(ApiCall::new(
            "basket_order",
            "basketorder",
            json!({"strategy": strategy, "orders": orders}),
        ))
        .await
    }

    async fn split_order(&self, o: &SplitOrder) -> Value {
        let mut body = order_body(&o.order);
        body.insert("splitsize".into(), json!(o.split_size));
        self.api(ApiCall::new(
            "split_order",
            "splitorder",
            Value::Object(body),
        ))
        .await
    }

    async fn options_order(&self, o: &OptionsOrder) -> Value {
        self.api(ApiCall::new(
            "options_order",
            "optionsorder",
            json!({
                "strategy": o.strategy, "underlying": o.underlying, "exchange": o.exchange,
                "action": o.action.to_uppercase(), "quantity": o.quantity,
                "expiry_date": o.expiry_date, "offset": o.offset,
                "option_type": o.option_type.to_uppercase(), "pricetype": o.price_type,
                "product": o.product, "price": o.price, "trigger_price": o.trigger_price,
                "splitsize": o.splitsize,
            }),
        ))
        .await
    }

    async fn options_multi_order(&self, o: &OptionsMultiOrder) -> Value {
        self.api(ApiCall::new(
            "options_multi_order",
            "optionsmultiorder",
            json!({
                "strategy": o.strategy, "underlying": o.underlying, "exchange": o.exchange,
                "expiry_date": o.expiry_date, "strike_int": o.strike_int, "legs": o.legs,
            }),
        ))
        .await
    }

    // --- Market data ------------------------------------------------------

    async fn get_quotes(&self, symbol: &str, exchange: &str) -> Value {
        self.api(ApiCall::new(
            "get_quotes",
            "quotes",
            json!({"symbol": symbol, "exchange": exchange}),
        ))
        .await
    }

    async fn get_depth(&self, symbol: &str, exchange: &str) -> Value {
        self.api(ApiCall::new(
            "get_depth",
            "depth",
            json!({"symbol": symbol, "exchange": exchange}),
        ))
        .await
    }

    async fn get_multi_quotes(&self, symbols: &[Value]) -> Value {
        self.api(ApiCall::new(
            "get_multi_quotes",
            "multiquotes",
            json!({"symbols": symbols}),
        ))
        .await
    }

    /// History from the broker (`source` `api`) or the Historify store
    /// (`db`, which needs both dates, refused here as the web does).
    async fn get_history(&self, h: &History) -> Value {
        if h.source == "db" && (h.start_date.is_none() || h.end_date.is_none()) {
            return json!({
                "status": "error",
                "code": 400,
                "error": "source='db' requires both start_date and end_date (YYYY-MM-DD)",
            });
        }
        let mut body = json!({
            "symbol": h.symbol, "exchange": h.exchange, "interval": h.interval, "source": h.source,
        });
        if let Some(obj) = body.as_object_mut() {
            if let Some(s) = &h.start_date {
                obj.insert("start_date".into(), json!(s));
            }
            if let Some(e) = &h.end_date {
                obj.insert("end_date".into(), json!(e));
            }
        }
        self.api(ApiCall::new("get_history", "history", body)).await
    }

    async fn get_intervals(&self) -> Value {
        self.api(ApiCall::new("get_intervals", "intervals", json!({})))
            .await
    }

    async fn symbol(&self, symbol: &str, exchange: &str) -> Value {
        self.api(ApiCall::new(
            "symbol",
            "symbol",
            json!({"symbol": symbol, "exchange": exchange}),
        ))
        .await
    }

    async fn get_expiry(&self, symbol: &str, exchange: &str, instrumenttype: &str) -> Value {
        self.api(ApiCall::new(
            "get_expiry",
            "expiry",
            json!({"symbol": symbol, "exchange": exchange, "instrumenttype": instrumenttype}),
        ))
        .await
    }

    // --- Account ----------------------------------------------------------

    async fn orderbook(&self) -> Value {
        self.api(ApiCall::new("orderbook", "orderbook", json!({})))
            .await
    }

    async fn tradebook(&self) -> Value {
        self.api(ApiCall::new("tradebook", "tradebook", json!({})))
            .await
    }

    async fn positionbook(&self) -> Value {
        self.api(ApiCall::new("positionbook", "positionbook", json!({})))
            .await
    }

    async fn holdings(&self) -> Value {
        self.api(ApiCall::new("holdings", "holdings", json!({})))
            .await
    }

    async fn funds(&self) -> Value {
        self.api(ApiCall::new("funds", "funds", json!({}))).await
    }

    async fn get_order_status(&self, order_id: &str) -> Value {
        self.api(ApiCall::new(
            "get_order_status",
            "orderstatus",
            json!({"orderid": order_id, "strategy": DEFAULT_STRATEGY}),
        ))
        .await
    }

    /// The quantity and P&L of one position, read from the position book
    /// (web `get_open_position`: it does not call `openposition`).
    async fn get_open_position(
        &self,
        symbol: &str,
        exchange: &str,
        product_type: Option<&str>,
    ) -> Value {
        let book = self.positionbook().await;
        if book.get("status").and_then(Value::as_str) != Some("success") {
            return book;
        }
        let flat = json!({"status": "success", "quantity": 0, "pnl": 0});
        let Some(positions) = book.get("data").and_then(Value::as_array) else {
            return flat;
        };
        for pos in positions {
            let same = pos.get("symbol").and_then(Value::as_str) == Some(symbol)
                && pos.get("exchange").and_then(Value::as_str) == Some(exchange);
            if !same {
                continue;
            }
            let pos_product = pos
                .get("product")
                .filter(|v| super::pyfmt::truthy(v))
                .or_else(|| pos.get("product_type"))
                .and_then(Value::as_str);
            if let Some(want) = product_type.filter(|p| !p.is_empty()) {
                if pos_product != Some(want) {
                    continue;
                }
            }
            return json!({
                "status": "success",
                "quantity": pos.get("quantity").cloned().unwrap_or(json!(0)),
                "pnl": pos.get("pnl").cloned().unwrap_or(json!(0)),
            });
        }
        flat
    }

    async fn margin(&self, m: &Margin) -> Value {
        let positions = match &m.positions {
            Some(p) if !p.is_empty() => p.clone(),
            _ => vec![json!({
                "exchange": m.exchange, "symbol": m.symbol, "action": m.action.to_uppercase(),
                "quantity": m.quantity.to_string(), "product": m.product_type,
                "pricetype": m.price_type, "price": super::pyfmt::str(&m.price),
            })],
        };
        self.api(ApiCall::new(
            "margin",
            "margin",
            json!({"positions": positions}),
        ))
        .await
    }

    // --- Options ----------------------------------------------------------

    async fn optionchain(
        &self,
        underlying: &str,
        exchange: &str,
        expiry_date: &str,
        strike_count: i64,
    ) -> Value {
        self.api(ApiCall::new(
            "optionchain",
            "optionchain",
            json!({"underlying": underlying, "exchange": exchange, "expiry_date": expiry_date, "strike_count": strike_count}),
        ))
        .await
    }

    async fn optionsymbol(
        &self,
        underlying: &str,
        exchange: &str,
        expiry_date: &str,
        offset: &str,
        option_type: &str,
    ) -> Value {
        self.api(ApiCall::new(
            "optionsymbol",
            "optionsymbol",
            json!({"underlying": underlying, "exchange": exchange, "expiry_date": expiry_date, "offset": offset, "option_type": option_type}),
        ))
        .await
    }

    async fn syntheticfuture(&self, underlying: &str, exchange: &str, expiry_date: &str) -> Value {
        self.api(ApiCall::new(
            "syntheticfuture",
            "syntheticfuture",
            json!({"underlying": underlying, "exchange": exchange, "expiry_date": expiry_date}),
        ))
        .await
    }

    // --- Market calendar ----------------------------------------------------

    async fn holidays(&self, year: Option<i32>) -> Value {
        let body = match year {
            Some(y) => json!({"year": y}),
            None => json!({}),
        };
        self.api(ApiCall::new("holidays", "market/holidays", body))
            .await
    }

    async fn timings(&self, date: &str) -> Value {
        self.api(ApiCall::new(
            "timings",
            "market/timings",
            json!({"date": date}),
        ))
        .await
    }
}

/// The web's `_handle_response` over what `/api/v1` answered: success
/// passes every field through under `"status": "success"`; failure keeps
/// the reply's message under `error` with the HTTP status as `code`.
///
/// One desktop adjustment: the web's Flow calls the service directly, which
/// reports a field problem as its message (a dict of field errors); the
/// desktop's `/api/v1` wraps the same errors as `{"message": "Validation
/// error", "errors": {...}}` (optionchain, optionsymbol, optionsmultiorder).
/// The field errors are what the web's node shows, so they become the
/// `error`, not the bare "Validation error".
pub fn handle_response(status: u16, body: &Value) -> Value {
    let failed = !(200..300).contains(&status)
        || body.get("status").and_then(Value::as_str) == Some("error");
    if failed {
        let field_errors = body
            .get("errors")
            .filter(|e| e.is_object() && super::pyfmt::truthy(e));
        let message = field_errors
            .or_else(|| body.get("message"))
            .cloned()
            .unwrap_or_else(|| json!("Unknown error"));
        return json!({"status": "error", "error": message, "code": status});
    }
    let mut out = Map::new();
    out.insert("status".into(), json!("success"));
    if let Value::Object(m) = body {
        for (k, v) in m {
            if k != "status" {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::fake::FakeFlowClient;
    use super::*;

    /// Every method posts to the web's service with the web client's payload
    /// (parity CL-04..CL-23); the apikey is added by the dispatch.
    #[tokio::test]
    async fn every_method_posts_the_web_payload() {
        let c = FakeFlowClient::new();
        let mut o = PlaceOrder::new("SBIN", "NSE", "sell", 10);
        o.price_type = "LIMIT".into();
        o.price = 812.5;
        o.strategy = "Gap fade".into();
        c.place_smart_order(&SmartOrder {
            order: o.clone(),
            position_size: 0,
        })
        .await;
        c.modify_order(&ModifyOrder {
            order_id: "42".into(),
            order: o.clone(),
        })
        .await;
        c.split_order(&SplitOrder {
            order: o.clone(),
            split_size: 5,
        })
        .await;
        c.cancel_order("42").await;
        c.cancel_all_orders().await;
        c.close_all_positions().await;
        c.close_position("SBIN", "NSE", "MIS", "Gap fade").await;
        c.basket_order(&[json!({"symbol": "SBIN"})], "Gap fade")
            .await;
        c.get_depth("SBIN", "NSE").await;
        c.get_multi_quotes(&[json!({"symbol": "SBIN", "exchange": "NSE"})])
            .await;
        c.get_intervals().await;
        c.symbol("SBIN", "NSE").await;
        c.get_expiry("NIFTY", "NFO", "options").await;
        c.orderbook().await;
        c.tradebook().await;
        c.holdings().await;
        c.funds().await;
        c.get_order_status("42").await;
        c.optionchain("NIFTY", "NSE_INDEX", "28OCT25", 10).await;
        c.optionsymbol("NIFTY", "NSE_INDEX", "28OCT25", "ATM", "CE")
            .await;
        c.syntheticfuture("NIFTY", "NSE_INDEX", "28OCT25").await;
        c.holidays(Some(2026)).await;
        c.timings("2026-10-30").await;
        let calls = c.calls();
        let by = |m: &str| calls.iter().find(|x| x.method == m).cloned().unwrap();
        let smart = by("place_smart_order");
        assert_eq!(smart.endpoint, "placesmartorder");
        assert_eq!(
            Value::Object(smart.payload),
            json!({"strategy": "Gap fade", "symbol": "SBIN", "exchange": "NSE", "action": "SELL",
                   "quantity": 10, "position_size": 0, "pricetype": "LIMIT", "product": "MIS",
                   "price": 812.5, "trigger_price": 0.0})
        );
        assert_eq!(by("modify_order").payload["orderid"], "42");
        assert_eq!(by("modify_order").payload["disclosed_quantity"], 0);
        assert_eq!(by("split_order").payload["splitsize"], 5);
        assert_eq!(
            Value::Object(by("cancel_order").payload),
            json!({"orderid": "42", "strategy": "flow_workflow"})
        );
        assert_eq!(by("cancel_all_orders").endpoint, "cancelallorder");
        assert_eq!(by("close_all_positions").endpoint, "closeposition");
        assert_eq!(by("close_position").payload["product"], "MIS");
        assert_eq!(by("basket_order").payload["orders"][0]["symbol"], "SBIN");
        assert_eq!(by("get_order_status").payload["strategy"], "flow_workflow");
        assert_eq!(by("optionchain").payload["strike_count"], 10);
        assert_eq!(by("optionsymbol").payload["offset"], "ATM");
        assert_eq!(
            (
                by("holidays").endpoint,
                by("holidays").payload["year"].clone()
            ),
            ("market/holidays", json!(2026))
        );
        assert_eq!(by("timings").payload["date"], "2026-10-30");
        for (m, e) in [
            ("get_depth", "depth"),
            ("get_multi_quotes", "multiquotes"),
            ("get_intervals", "intervals"),
            ("symbol", "symbol"),
            ("get_expiry", "expiry"),
            ("orderbook", "orderbook"),
            ("tradebook", "tradebook"),
            ("holdings", "holdings"),
            ("funds", "funds"),
            ("syntheticfuture", "syntheticfuture"),
        ] {
            assert_eq!(by(m).endpoint, e, "{}", m);
        }
    }

    #[tokio::test]
    async fn options_margin_and_history_payloads_follow_the_web() {
        let c = FakeFlowClient::new();
        c.options_order(&OptionsOrder {
            underlying: "NIFTY".into(),
            exchange: "NSE_INDEX".into(),
            action: "buy".into(),
            quantity: 75,
            expiry_date: "28OCT25".into(),
            offset: "OTM2".into(),
            option_type: "ce".into(),
            price_type: "MARKET".into(),
            product: "NRML".into(),
            price: 0.0,
            splitsize: 0,
            strategy: "s".into(),
            trigger_price: 0.0,
        })
        .await;
        c.options_multi_order(&OptionsMultiOrder {
            underlying: "NIFTY".into(),
            exchange: "NSE_INDEX".into(),
            expiry_date: "28OCT25".into(),
            legs: vec![json!({"offset": "ATM"})],
            strategy: "s".into(),
            strike_int: None,
        })
        .await;
        c.margin(&Margin {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            quantity: 10,
            price: json!(0),
            product_type: "MIS".into(),
            action: "buy".into(),
            price_type: "MARKET".into(),
            positions: None,
        })
        .await;
        let refused = c
            .get_history(&History {
                symbol: "SBIN".into(),
                exchange: "NSE".into(),
                interval: "5m".into(),
                start_date: None,
                end_date: Some("2026-10-30".into()),
                source: "db".into(),
            })
            .await;
        assert_eq!(
            refused,
            json!({"status": "error", "code": 400,
            "error": "source='db' requires both start_date and end_date (YYYY-MM-DD)"})
        );
        let calls = c.calls();
        assert_eq!(calls.len(), 3, "the refused history call is never sent");
        assert_eq!(calls[0].payload["action"], "BUY");
        assert_eq!(calls[0].payload["option_type"], "CE");
        assert_eq!(calls[1].payload["strike_int"], Value::Null);
        assert_eq!(
            calls[2].payload["positions"][0],
            json!({"exchange": "NSE", "symbol": "SBIN", "action": "BUY", "quantity": "10",
                   "product": "MIS", "pricetype": "MARKET", "price": "0"})
        );
    }

    /// An option call refused for its fields reports the field errors, not
    /// the bare "Validation error" of the API's envelope.
    #[test]
    fn a_validation_envelope_reports_its_field_errors() {
        let refused = json!({"status": "error", "message": "Validation error",
                             "errors": {"exchange": ["Must be one of: NSE_INDEX, NSE, NFO, BSE_INDEX, BSE, BFO, MCX, CDS."]}});
        assert_eq!(
            handle_response(400, &refused),
            json!({"status": "error", "error": {"exchange": ["Must be one of: NSE_INDEX, NSE, NFO, BSE_INDEX, BSE, BFO, MCX, CDS."]}, "code": 400})
        );
        let plain = json!({"status": "error", "message": "Validation error", "errors": {}});
        assert_eq!(handle_response(400, &plain)["error"], "Validation error");
    }

    #[test]
    fn replies_are_shaped_as_the_web_client_shapes_them() {
        assert_eq!(
            handle_response(200, &json!({"status": "success", "data": {"ltp": 1.5}})),
            json!({"status": "success", "data": {"ltp": 1.5}})
        );
        assert_eq!(
            handle_response(
                403,
                &json!({"status": "error", "message": "Invalid openalgo apikey"})
            ),
            json!({"status": "error", "error": "Invalid openalgo apikey", "code": 403})
        );
        assert_eq!(
            handle_response(
                400,
                &json!({"status": "error", "message": {"symbol": ["Missing data for required field."]}})
            ),
            json!({"status": "error", "error": {"symbol": ["Missing data for required field."]}, "code": 400})
        );
        assert_eq!(
            handle_response(500, &json!("not json")),
            json!({"status": "error", "error": "Unknown error", "code": 500})
        );
    }
}
