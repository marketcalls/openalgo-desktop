//! The Broker trait from outside the crate: a minimal adapter gets the
//! default optional capabilities (typed Unsupported) and default helpers
//! (multiquotes loop, cancel-all, close-all, open position); the scriptable
//! `MockBroker` is reachable through the `test-support` feature.

use async_trait::async_trait;
use openalgo_desktop_lib::brokers::common::mapping::{Exchange, Product};
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::mock::{MockBroker, MockCall};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{AuthResponse, Broker, BrokerCredentials, BrokerRegistry};
use openalgo_desktop_lib::error::{AppError, Result};
use parking_lot::Mutex;
use std::sync::Arc;

fn row(symbol: &str, exchange: &str, token: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: format!("{}-EQ", symbol),
        name: symbol.into(),
        exchange: exchange.into(),
        brexchange: exchange.into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

fn order(id: &str, status: &str) -> Order {
    Order {
        order_tag: None,
        order_id: id.into(),
        exchange_order_id: None,
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        side: "BUY".into(),
        quantity: 1,
        filled_quantity: 0,
        pending_quantity: 1,
        price: 0.0,
        trigger_price: 0.0,
        average_price: 0.0,
        order_type: "LIMIT".into(),
        product: "MIS".into(),
        status: status.into(),
        validity: "DAY".into(),
        order_timestamp: String::new(),
        exchange_timestamp: None,
        rejection_reason: None,
    }
}

fn position(symbol: &str, qty: i32) -> Position {
    Position {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        product: "MIS".into(),
        quantity: qty,
        overnight_quantity: 0,
        average_price: 100.0,
        ltp: 101.0,
        pnl: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        buy_quantity: 0,
        buy_value: 0.0,
        sell_quantity: 0,
        sell_value: 0.0,
    }
}

/// Implements only the required methods.
struct Minimal {
    symbols: SymbolResolver,
    placed: Mutex<Vec<ResolvedOrder>>,
    cancelled: Mutex<Vec<String>>,
}

#[async_trait]
impl Broker for Minimal {
    fn id(&self) -> &'static str {
        "minimal"
    }
    fn name(&self) -> &'static str {
        "Minimal"
    }
    fn logo(&self) -> &'static str {
        ""
    }
    fn login_kind(&self) -> LoginKind {
        LoginKind::DirectTotp {
            fields: &["client_id", "password", "totp"],
        }
    }
    fn supported_exchanges(&self) -> &'static [Exchange] {
        &[Exchange::Nse]
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }
    fn symbols(&self) -> Option<&SymbolResolver> {
        Some(&self.symbols)
    }
    async fn authenticate(&self, _: BrokerCredentials) -> Result<AuthResponse> {
        Err(AppError::Auth("no".into()))
    }
    async fn place_order(&self, _: &AuthToken, o: &ResolvedOrder) -> Result<OrderResponse> {
        if o.symbol == "TCS" {
            return Err(AppError::Broker("RMS: margin exceeds".into()));
        }
        self.placed.lock().push(o.clone());
        Ok(OrderResponse {
            order_id: format!("X{}", self.placed.lock().len()),
            message: None,
        })
    }
    async fn modify_order(&self, _: &AuthToken, o: &ResolvedModify) -> Result<OrderResponse> {
        Ok(OrderResponse {
            order_id: o.order_id.clone(),
            message: None,
        })
    }
    async fn cancel_order(&self, _: &AuthToken, id: &str) -> Result<OrderResponse> {
        if id == "bad" {
            return Err(AppError::Broker("cannot".into()));
        }
        self.cancelled.lock().push(id.into());
        Ok(OrderResponse {
            order_id: id.into(),
            message: None,
        })
    }
    async fn get_order_book(&self, _: &AuthToken) -> Result<Vec<Order>> {
        Ok(vec![
            order("1", "open"),
            order("2", "complete"),
            order("3", "trigger pending"),
            order("bad", "open"),
            order("4", "rejected"),
        ])
    }
    async fn get_trade_book(&self, _: &AuthToken) -> Result<Vec<Trade>> {
        Ok(vec![])
    }
    async fn get_positions(&self, _: &AuthToken) -> Result<Vec<Position>> {
        Ok(vec![
            position("SBIN", 5),
            position("INFY", 0),
            position("TCS", -2),
            position("GONE", 3),
        ])
    }
    async fn get_holdings(&self, _: &AuthToken) -> Result<Vec<Holding>> {
        Ok(vec![])
    }
    async fn get_funds(&self, _: &AuthToken) -> Result<Funds> {
        Ok(Funds::default())
    }
    async fn get_quote(&self, _: &AuthToken, k: &QuoteKey) -> Result<Quote> {
        if k.symbol == "NOPE" {
            return Err(AppError::Broker("No quote".into()));
        }
        Ok(Quote {
            symbol: k.symbol.clone(),
            exchange: k.exchange.clone(),
            ltp: 1.0,
            ..Default::default()
        })
    }
    async fn get_market_depth(&self, _: &AuthToken, _: &QuoteKey) -> Result<MarketDepth> {
        Err(AppError::Unsupported("depth"))
    }
    async fn get_history(&self, _: &AuthToken, _: &HistoryRequest) -> Result<Vec<Candle>> {
        Err(AppError::Unsupported("history"))
    }
    async fn download_master_contract(&self, _: &AuthToken) -> Result<Vec<SymbolData>> {
        Ok(vec![])
    }
}

fn minimal() -> Minimal {
    let symbols = SymbolResolver::new();
    symbols.load(vec![
        row("SBIN", "NSE", "1"),
        row("TCS", "NSE", "2"),
        row("INFY", "NSE", "3"),
    ]);
    Minimal {
        symbols,
        placed: Mutex::new(vec![]),
        cancelled: Mutex::new(vec![]),
    }
}

fn auth() -> AuthToken {
    AuthToken::new("key:token")
}

#[tokio::test]
async fn optional_capabilities_default_to_typed_unsupported() {
    let b: Arc<dyn Broker> = Arc::new(minimal());
    let req = GttRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        trigger_type: GttTriggerType::Single,
        action: openalgo_desktop_lib::brokers::common::mapping::Action::Buy,
        product: Product::Cnc,
        quantity: 1,
        pricetype: openalgo_desktop_lib::brokers::common::mapping::PriceType::Limit,
        price: 1.0,
        trigger_price: 1.0,
        triggerprice_sl: 0.0,
        stoploss: 0.0,
        triggerprice_tg: 0.0,
        target: 0.0,
        last_price: None,
    };
    for e in [
        b.place_gtt(&auth(), &req).await.unwrap_err(),
        b.modify_gtt(&auth(), "1", &req).await.unwrap_err(),
        b.cancel_gtt(&auth(), "1").await.unwrap_err(),
        b.get_gtt_book(&auth(), false).await.unwrap_err(),
    ] {
        assert!(matches!(e, AppError::Unsupported("gtt")));
        assert_eq!(e.code(), "UNSUPPORTED");
    }
    let e = b.calculate_margin(&auth(), &[]).await.unwrap_err();
    assert!(matches!(e, AppError::Unsupported("margin")));
    assert!(b.create_feed(&auth()).is_err());
    assert!(b.requires_totp());
}

#[tokio::test]
async fn default_multiquotes_loops_get_quote_in_order() {
    let b = minimal();
    let keys = [
        QuoteKey::new("NSE", "SBIN"),
        QuoteKey::new("NSE", "NOPE"),
        QuoteKey::new("NSE", "TCS"),
    ];
    let r = b.get_multiquotes(&auth(), &keys).await.unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].data.as_ref().unwrap().symbol, "SBIN");
    assert!(r[1].data.is_none());
    assert_eq!(r[1].error.as_deref(), Some("No quote"));
    assert_eq!(r[2].symbol, "TCS");
}

#[tokio::test]
async fn default_cancel_all_uses_lowercase_statuses() {
    let b = minimal();
    let r = b.cancel_all_orders(&auth()).await.unwrap();
    assert_eq!(r.cancelled, ["1", "3"]);
    assert_eq!(r.failed, ["bad"]);
}

#[tokio::test]
async fn default_close_all_squares_off_and_reports_failures() {
    let b = minimal();
    let r = b.close_all_positions(&auth()).await.unwrap();
    assert_eq!(r.placed, ["X1"]);
    assert_eq!(r.failed.len(), 2);
    assert!(r.failed[0].starts_with("TCS (NSE): RMS: margin exceeds"));
    assert!(r.failed[1].starts_with("GONE (NSE): Symbol GONE was not found"));
    let placed = b.placed.lock().clone();
    assert_eq!(placed[0].action.as_str(), "SELL");
    assert_eq!(placed[0].quantity, 5);
    assert_eq!(placed[0].instrument.brsymbol, "SBIN-EQ");
    assert!(r
        .message()
        .starts_with("2 position(s) could not be squared off"));
}

#[tokio::test]
async fn default_open_position_matches_symbol_exchange_product() {
    let b = minimal();
    assert_eq!(
        b.get_open_position(&auth(), "SBIN", Exchange::Nse, Product::Mis)
            .await
            .unwrap(),
        5
    );
    assert_eq!(
        b.get_open_position(&auth(), "SBIN", Exchange::Nse, Product::Cnc)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        b.get_open_position(&auth(), "TCS", Exchange::Nse, Product::Mis)
            .await
            .unwrap(),
        -2
    );
}

#[tokio::test]
async fn mock_broker_is_scriptable_and_records_calls() {
    let symbols = SymbolResolver::new();
    symbols.load(vec![row("SBIN", "NSE", "1")]);
    let m = Arc::new(MockBroker::with_symbols("mock", symbols.clone()));
    m.order_ids.lock().push_back(Ok("OID-1".into()));
    m.order_ids
        .lock()
        .push_back(Err("Insufficient funds".into()));
    *m.positions.lock() = Some(Ok(vec![position("SBIN", 2)]));
    let reg = BrokerRegistry::with_symbols(symbols.clone(), vec![m.clone() as Arc<dyn Broker>]);
    let b = reg.get("mock").unwrap();
    let req = OrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        side: "BUY".into(),
        quantity: 1,
        price: 0.0,
        order_type: "MARKET".into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };
    let o = ResolvedOrder::resolve(&req, &reg.symbols()).unwrap();
    assert_eq!(b.place_order(&auth(), &o).await.unwrap().order_id, "OID-1");
    let e = b.place_order(&auth(), &o).await.unwrap_err();
    assert_eq!(e.client_message(), "Insufficient funds");
    assert_eq!(b.place_order(&auth(), &o).await.unwrap().order_id, "MOCK-1");
    assert_eq!(
        b.get_open_position(&auth(), "SBIN", Exchange::Nse, Product::Mis)
            .await
            .unwrap(),
        2
    );
    let calls = m.calls();
    assert!(matches!(calls[0], MockCall::PlaceOrder(_)));
    assert!(calls.contains(&MockCall::Positions));
}
