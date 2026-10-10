//! Scriptable broker for service, HTTP and integration tests.
//!
//! Every trait method returns a scripted value (or a scripted error) and
//! records the call, so a test can assert what a service sent. Available to
//! other test crates through the `test-support` feature.

use super::common::mapping::{Exchange, Product};
use super::common::streaming::{
    BrokerFeed, FeedEvent, FeedSubscription, Message, OrderFeed, OrderUpdate, WsRequest,
};
use super::common::symbols::SymbolResolver;
use super::types::*;
use super::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};

/// A scripted outcome: a value, or a broker error carrying this message.
pub type Scripted<T> = std::result::Result<T, String>;

fn out<T: Clone>(slot: &Mutex<Option<Scripted<T>>>, default: impl FnOnce() -> T) -> Result<T> {
    match slot.lock().clone() {
        Some(Ok(v)) => Ok(v),
        Some(Err(m)) => Err(AppError::Broker(m)),
        None => Ok(default()),
    }
}

/// A placement that reaches the broker and then loses its answer (LOG-08).
/// The broker keeps the order: it is added to the mock's order book with
/// this status and fill, then the HTTP exchange really fails after sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterSend {
    /// The connection drops after the request was sent.
    Dropped,
    /// No answer within the request timeout.
    TimedOut,
}

/// A real transport error that happens after the request was sent: a local
/// server reads the whole request, then drops the connection or stays
/// silent past a short timeout.
pub async fn after_send_error(kind: AfterSend) -> AppError {
    use tokio::io::AsyncReadExt;
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(e) => return AppError::Io(e),
    };
    let addr = match listener.local_addr() {
        Ok(a) => a,
        Err(e) => return AppError::Io(e),
    };
    let server = tokio::spawn(async move {
        if let Ok((mut s, _)) = listener.accept().await {
            let mut buf = vec![0u8; 8192];
            let _ = s.read(&mut buf).await;
            if kind == AfterSend::TimedOut {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            drop(s);
        }
    });
    let client = match reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_millis(300))
        .build()
    {
        Ok(c) => c,
        Err(e) => return e.into(),
    };
    let r = client
        .post(format!("http://{}/orders", addr))
        .body("order")
        .send()
        .await;
    server.abort();
    match r {
        Err(e) => e.into(),
        Ok(_) => AppError::Internal("the after-send fault answered".into()),
    }
}

/// Every call the mock received, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum MockCall {
    Authenticate,
    PlaceOrder(ResolvedOrder),
    ModifyOrder(ResolvedModify),
    CancelOrder(String),
    CancelAll,
    CloseAll,
    OpenPosition(String, Exchange, Product),
    OrderBook,
    TradeBook,
    Positions,
    Holdings,
    Funds,
    Margin(usize),
    Quote(QuoteKey),
    MultiQuotes(Vec<QuoteKey>),
    Depth(QuoteKey),
    History(HistoryRequest),
    PlaceGtt,
    ModifyGtt(String),
    CancelGtt(String),
    GttBook,
    MasterContract,
}

pub struct MockBroker {
    pub id: &'static str,
    pub symbols: SymbolResolver,
    /// Back-compat switch: when false, `get_funds` fails like an expired token.
    pub funds_ok: Mutex<bool>,
    /// Credentials passed to the last `authenticate` call.
    pub last_auth: Mutex<Option<BrokerCredentials>>,
    pub funds_calls: Mutex<u32>,
    pub calls: Mutex<Vec<MockCall>>,
    /// Order ids handed out by `place_order`, front first (then `MOCK-<n>`).
    pub order_ids: Mutex<VecDeque<Scripted<String>>>,
    /// Placements that lose their answer after reaching the broker, front
    /// first, with the status the kept order then has (`complete` fills
    /// it in full at `after_send_price`).
    pub after_send: Mutex<VecDeque<(AfterSend, &'static str)>>,
    pub after_send_price: Mutex<f64>,
    /// `place_order` records the order, then fails by panicking: a failure
    /// after the order reached the broker (MCP-01).
    pub panic_after_place: Mutex<bool>,
    pub modify: Mutex<Option<Scripted<OrderResponse>>>,
    pub cancel: Mutex<Option<Scripted<OrderResponse>>>,
    pub order_book: Mutex<Option<Scripted<Vec<Order>>>>,
    pub trade_book: Mutex<Option<Scripted<Vec<Trade>>>>,
    pub positions: Mutex<Option<Scripted<Vec<Position>>>>,
    pub holdings: Mutex<Option<Scripted<Vec<Holding>>>>,
    /// Portfolio totals the broker reports with its holdings (Angel).
    pub holdings_totals: Mutex<Option<PortfolioStats>>,
    pub funds: Mutex<Option<Scripted<Funds>>>,
    pub margin: Mutex<Option<Scripted<MarginResult>>>,
    /// Quotes keyed by `EXCHANGE:SYMBOL`.
    pub quotes: Mutex<HashMap<String, Quote>>,
    pub depth: Mutex<Option<Scripted<MarketDepth>>>,
    pub history: Mutex<Option<Scripted<Vec<Candle>>>>,
    pub gtt: Mutex<Option<Scripted<GttResponse>>>,
    pub gtt_book: Mutex<Option<Scripted<Vec<GttOrder>>>>,
    pub master: Mutex<Option<Scripted<Vec<SymbolData>>>>,
    /// Address of a fake feed server; when set, `create_feed` returns a
    /// `MockFeed` pointed at it.
    pub feed_url: Mutex<Option<String>>,
    /// Address of a fake order-update server; when set,
    /// `create_order_feed` returns a `MockFeed` socket pointed at it.
    pub order_feed_url: Mutex<Option<String>>,
    /// Depth socket address and the levels it serves.
    pub depth_feed: Mutex<Option<(String, u8)>>,
    /// `on_logout` calls.
    pub logouts: Mutex<u32>,
    /// Contract multipliers by token sent with the master.
    pub contract_values: Mutex<HashMap<String, f64>>,
    /// What `carries_stored` answers.
    pub carry: Mutex<Option<&'static str>>,
    /// The stored rows the last `download_master_carrying` was given.
    pub carried: Mutex<Option<Vec<SymbolData>>>,
    /// The account id a sign-in returns.
    pub auth_user_id: Mutex<String>,
    /// What `begin_login` returns (a consent address).
    pub login_url: Mutex<Option<String>>,
    /// Credentials passed to the last `restore_session`.
    pub restored: Mutex<Option<BrokerCredentials>>,
    /// Sign in from the saved API key and secret alone (the XTS direct
    /// logins, Delta Exchange); otherwise a code, password or TOTP is needed.
    pub saved_keys_sign_in: Mutex<bool>,
    next_id: Mutex<u64>,
}

impl MockBroker {
    pub fn new(id: &'static str) -> Self {
        Self::with_symbols(id, SymbolResolver::new())
    }

    pub fn with_symbols(id: &'static str, symbols: SymbolResolver) -> Self {
        Self {
            id,
            symbols,
            funds_ok: Mutex::new(true),
            last_auth: Mutex::new(None),
            funds_calls: Mutex::new(0),
            calls: Mutex::new(Vec::new()),
            order_ids: Mutex::new(VecDeque::new()),
            after_send: Mutex::new(VecDeque::new()),
            after_send_price: Mutex::new(100.0),
            panic_after_place: Mutex::new(false),
            modify: Mutex::new(None),
            cancel: Mutex::new(None),
            order_book: Mutex::new(None),
            trade_book: Mutex::new(None),
            positions: Mutex::new(None),
            holdings: Mutex::new(None),
            holdings_totals: Mutex::new(None),
            funds: Mutex::new(None),
            margin: Mutex::new(None),
            quotes: Mutex::new(HashMap::new()),
            depth: Mutex::new(None),
            history: Mutex::new(None),
            gtt: Mutex::new(None),
            gtt_book: Mutex::new(None),
            master: Mutex::new(None),
            feed_url: Mutex::new(None),
            order_feed_url: Mutex::new(None),
            depth_feed: Mutex::new(None),
            logouts: Mutex::new(0),
            contract_values: Mutex::new(HashMap::new()),
            carry: Mutex::new(None),
            carried: Mutex::new(None),
            auth_user_id: Mutex::new("AB1234".into()),
            login_url: Mutex::new(None),
            restored: Mutex::new(None),
            saved_keys_sign_in: Mutex::new(false),
            next_id: Mutex::new(0),
        }
    }

    fn record(&self, c: MockCall) {
        self.calls.lock().push(c);
    }

    /// Calls received so far.
    pub fn calls(&self) -> Vec<MockCall> {
        self.calls.lock().clone()
    }

    pub fn set_quote(&self, q: Quote) {
        self.quotes
            .lock()
            .insert(format!("{}:{}", q.exchange, q.symbol), q);
    }
}

#[async_trait]
impl Broker for MockBroker {
    fn id(&self) -> &'static str {
        self.id
    }
    fn name(&self) -> &'static str {
        "Mock"
    }
    fn logo(&self) -> &'static str {
        ""
    }
    fn login_kind(&self) -> LoginKind {
        LoginKind::Redirect {
            param: "request_token",
        }
    }
    fn supported_exchanges(&self) -> &'static [Exchange] {
        Exchange::ALL
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            history: true,
            multiquotes_batch: false,
            margin: true,
            gtt: true,
            streaming: true,
            order_feed: true,
            depth_levels: &[5],
        }
    }
    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        &[("1m", "1m"), ("5m", "5m"), ("D", "D")]
    }
    fn feed_depth_levels(&self, _exchange: &str) -> Vec<u8> {
        match *self.depth_feed.lock() {
            Some((_, levels)) => vec![5, levels],
            None => vec![5],
        }
    }
    fn restore_session(&self, credentials: &BrokerCredentials) {
        *self.restored.lock() = Some(credentials.clone());
    }
    async fn on_logout(&self) {
        *self.logouts.lock() += 1;
    }
    async fn begin_login(&self, _credentials: &BrokerCredentials) -> Result<Option<String>> {
        Ok(self.login_url.lock().clone())
    }
    fn symbols(&self) -> Option<&SymbolResolver> {
        Some(&self.symbols)
    }

    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse> {
        self.record(MockCall::Authenticate);
        let ok = credentials.request_token.is_some()
            || credentials.totp.is_some()
            || credentials.password.is_some()
            || (*self.saved_keys_sign_in.lock() && credentials.api_secret.is_some());
        *self.last_auth.lock() = Some(credentials);
        if !ok {
            return Err(AppError::Auth("Mock rejected the sign-in".into()));
        }
        Ok(AuthResponse {
            auth_token: "mock-access-token".into(),
            feed_token: Some("mock-feed-token".into()),
            user_id: self.auth_user_id.lock().clone(),
            user_name: Some("Mock Trader".into()),
        })
    }

    async fn place_order(&self, _: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse> {
        self.record(MockCall::PlaceOrder(order.clone()));
        if *self.panic_after_place.lock() {
            panic!("mock broker: failure after the order reached the broker");
        }
        let fault = self.after_send.lock().pop_front();
        if let Some((kind, status)) = fault {
            // The broker took the order; its answer is lost on the way back.
            let id = {
                let mut n = self.next_id.lock();
                *n += 1;
                format!("MOCK-{}", *n)
            };
            let filled = if status == "complete" {
                order.quantity as i32
            } else {
                0
            };
            let row = Order {
                order_id: id,
                exchange_order_id: None,
                symbol: order.symbol.clone(),
                exchange: order.exchange.as_str().to_string(),
                side: order.action.as_str().to_string(),
                quantity: order.quantity as i32,
                filled_quantity: filled,
                pending_quantity: order.quantity as i32 - filled,
                price: 0.0,
                trigger_price: 0.0,
                average_price: if filled > 0 {
                    *self.after_send_price.lock()
                } else {
                    0.0
                },
                order_type: order.pricetype.as_str().to_string(),
                product: order.product.as_str().to_string(),
                status: status.to_string(),
                validity: "DAY".into(),
                order_timestamp: String::new(),
                exchange_timestamp: None,
                rejection_reason: None,
                order_tag: None,
            };
            {
                let mut book = self.order_book.lock();
                let mut rows = match book.take() {
                    Some(Ok(rows)) => rows,
                    _ => Vec::new(),
                };
                rows.push(row);
                *book = Some(Ok(rows));
            }
            return Err(after_send_error(kind).await);
        }
        let scripted = self.order_ids.lock().pop_front();
        match scripted {
            Some(Ok(id)) => Ok(OrderResponse {
                order_id: id,
                message: None,
            }),
            Some(Err(m)) => Err(AppError::Broker(m)),
            None => {
                let mut n = self.next_id.lock();
                *n += 1;
                Ok(OrderResponse {
                    order_id: format!("MOCK-{}", *n),
                    message: None,
                })
            }
        }
    }

    async fn modify_order(&self, _: &AuthToken, order: &ResolvedModify) -> Result<OrderResponse> {
        self.record(MockCall::ModifyOrder(order.clone()));
        out(&self.modify, || OrderResponse {
            order_id: order.order_id.clone(),
            message: None,
        })
    }

    async fn cancel_order(&self, _: &AuthToken, order_id: &str) -> Result<OrderResponse> {
        self.record(MockCall::CancelOrder(order_id.to_string()));
        out(&self.cancel, || OrderResponse {
            order_id: order_id.to_string(),
            message: None,
        })
    }

    async fn cancel_all_orders(&self, auth: &AuthToken) -> Result<CancelAllResult> {
        self.record(MockCall::CancelAll);
        // Exercise the trait's default path through the scripted book.
        let book = self.get_order_book(auth).await?;
        let mut r = CancelAllResult::default();
        for o in book {
            if o.status == "open" || o.status == "trigger pending" {
                match self.cancel_order(auth, &o.order_id).await {
                    Ok(_) => r.cancelled.push(o.order_id),
                    Err(_) => r.failed.push(o.order_id),
                }
            }
        }
        Ok(r)
    }

    async fn close_all_positions(&self, auth: &AuthToken) -> Result<CloseAllResult> {
        self.record(MockCall::CloseAll);
        let positions = self.get_positions(auth).await?;
        let mut r = CloseAllResult::default();
        for p in positions.into_iter().filter(|p| p.quantity != 0) {
            let req = OrderRequest {
                symbol: p.symbol.clone(),
                exchange: p.exchange.clone(),
                side: if p.quantity > 0 { "SELL" } else { "BUY" }.into(),
                quantity: p.quantity.abs(),
                price: 0.0,
                order_type: "MARKET".into(),
                product: p.product.clone(),
                validity: "DAY".into(),
                trigger_price: None,
                disclosed_quantity: None,
                amo: false,
            };
            let placed = match ResolvedOrder::resolve(&req, &self.symbols) {
                Ok(o) => self.place_order(auth, &o).await,
                Err(e) => Err(e),
            };
            match placed {
                Ok(o) => r.placed.push(o.order_id),
                Err(e) => r.failed.push(format!(
                    "{} ({}): {}",
                    p.symbol,
                    p.exchange,
                    e.client_message()
                )),
            }
        }
        Ok(r)
    }

    async fn get_open_position(
        &self,
        auth: &AuthToken,
        symbol: &str,
        exchange: Exchange,
        product: Product,
    ) -> Result<i64> {
        self.record(MockCall::OpenPosition(
            symbol.to_string(),
            exchange,
            product,
        ));
        let positions = self.get_positions(auth).await?;
        Ok(positions
            .iter()
            .find(|p| {
                p.symbol == symbol
                    && p.exchange == exchange.as_str()
                    && p.product == product.as_str()
            })
            .map(|p| i64::from(p.quantity))
            .unwrap_or(0))
    }

    async fn get_order_book(&self, _: &AuthToken) -> Result<Vec<Order>> {
        self.record(MockCall::OrderBook);
        out(&self.order_book, Vec::new)
    }
    async fn get_trade_book(&self, _: &AuthToken) -> Result<Vec<Trade>> {
        self.record(MockCall::TradeBook);
        out(&self.trade_book, Vec::new)
    }
    async fn get_positions(&self, _: &AuthToken) -> Result<Vec<Position>> {
        self.record(MockCall::Positions);
        out(&self.positions, Vec::new)
    }
    async fn get_holdings(&self, _: &AuthToken) -> Result<Vec<Holding>> {
        self.record(MockCall::Holdings);
        out(&self.holdings, Vec::new)
    }

    async fn get_holdings_with_totals(&self, auth: &AuthToken) -> Result<HoldingsBook> {
        Ok(HoldingsBook {
            holdings: self.get_holdings(auth).await?,
            totals: *self.holdings_totals.lock(),
        })
    }

    async fn get_funds(&self, auth: &AuthToken) -> Result<Funds> {
        self.record(MockCall::Funds);
        *self.funds_calls.lock() += 1;
        if !*self.funds_ok.lock() || auth.raw() != "mock-access-token" {
            return Err(AppError::Broker(
                "Incorrect `api_key` or `access_token`.".into(),
            ));
        }
        out(&self.funds, || Funds {
            available_cash: 125000.5,
            used_margin: 2500.25,
            total_margin: 127500.75,
            opening_balance: 127500.75,
            collateral: 1000.0,
            utilised_debits: 2500.25,
            ..Default::default()
        })
    }

    async fn calculate_margin(&self, _: &AuthToken, legs: &[MarginLeg]) -> Result<MarginResult> {
        self.record(MockCall::Margin(legs.len()));
        out(&self.margin, MarginResult::default)
    }

    async fn get_quote(&self, _: &AuthToken, key: &QuoteKey) -> Result<Quote> {
        self.record(MockCall::Quote(key.clone()));
        self.quotes
            .lock()
            .get(&format!("{}:{}", key.exchange, key.symbol))
            .cloned()
            .ok_or_else(|| {
                AppError::Broker(format!("No quote for {} {}", key.exchange, key.symbol))
            })
    }

    async fn get_multiquotes(&self, _: &AuthToken, keys: &[QuoteKey]) -> Result<Vec<QuoteResult>> {
        self.record(MockCall::MultiQuotes(keys.to_vec()));
        let q = self.quotes.lock();
        Ok(keys
            .iter()
            .map(|k| {
                let hit = q.get(&format!("{}:{}", k.exchange, k.symbol)).cloned();
                QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    error: hit.is_none().then(|| "No quote data available".to_string()),
                    data: hit,
                }
            })
            .collect())
    }

    async fn get_market_depth(&self, _: &AuthToken, key: &QuoteKey) -> Result<MarketDepth> {
        self.record(MockCall::Depth(key.clone()));
        out(&self.depth, || MarketDepth {
            symbol: key.symbol.clone(),
            exchange: key.exchange.clone(),
            bids: vec![DepthLevel::default(); 5],
            asks: vec![DepthLevel::default(); 5],
            ..Default::default()
        })
    }

    async fn get_history(&self, _: &AuthToken, req: &HistoryRequest) -> Result<Vec<Candle>> {
        self.record(MockCall::History(req.clone()));
        out(&self.history, Vec::new)
    }

    async fn place_gtt(&self, _: &AuthToken, _: &GttRequest) -> Result<GttResponse> {
        self.record(MockCall::PlaceGtt);
        out(&self.gtt, || GttResponse {
            trigger_id: "GTT-1".into(),
        })
    }

    async fn modify_gtt(&self, _: &AuthToken, id: &str, _: &GttRequest) -> Result<GttResponse> {
        self.record(MockCall::ModifyGtt(id.to_string()));
        out(&self.gtt, || GttResponse {
            trigger_id: id.to_string(),
        })
    }

    async fn cancel_gtt(&self, _: &AuthToken, id: &str) -> Result<GttResponse> {
        self.record(MockCall::CancelGtt(id.to_string()));
        out(&self.gtt, || GttResponse {
            trigger_id: id.to_string(),
        })
    }

    async fn get_gtt_book(&self, _: &AuthToken, _: bool) -> Result<Vec<GttOrder>> {
        self.record(MockCall::GttBook);
        out(&self.gtt_book, Vec::new)
    }

    async fn download_master_contract(&self, _: &AuthToken) -> Result<Vec<SymbolData>> {
        self.record(MockCall::MasterContract);
        out(&self.master, Vec::new)
    }

    async fn download_master(&self, auth: &AuthToken) -> Result<MasterContract> {
        let mut m = MasterContract::new(self.download_master_contract(auth).await?);
        m.contract_values = self.contract_values.lock().clone();
        Ok(m)
    }

    fn carries_stored(&self) -> Option<&'static str> {
        *self.carry.lock()
    }

    async fn download_master_carrying(
        &self,
        auth: &AuthToken,
        stored: Vec<SymbolData>,
    ) -> Result<MasterContract> {
        *self.carried.lock() = Some(stored);
        self.download_master(auth).await
    }

    fn create_feed(&self, _: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        match self.feed_url.lock().clone() {
            Some(url) => Ok(Box::new(MockFeed::new(url))),
            None => Err(AppError::Unsupported("streaming")),
        }
    }

    fn create_order_feed(&self, _: &AuthToken) -> Result<OrderFeed> {
        match self.order_feed_url.lock().clone() {
            Some(url) => Ok(OrderFeed::Socket(Box::new(MockFeed::new(url)))),
            None => Err(AppError::Unsupported("order_feed")),
        }
    }

    fn create_depth_feed(&self, _: &AuthToken, levels: u8) -> Result<Box<dyn BrokerFeed>> {
        match self.depth_feed.lock().clone() {
            Some((url, l)) if l == levels => Ok(Box::new(MockFeed::new(url))),
            _ => Err(AppError::Unsupported("depth_feed")),
        }
    }
}

/// A JSON-text feed for manager tests: subscribe frames are
/// `{"sub":[..]}`, ticks arrive as `{"t":"SYMBOL","x":"EXCH","p":123.4}`,
/// order updates as `{"order": {OrderUpdate fields}}`.
pub struct MockFeed {
    url: String,
}

impl MockFeed {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

impl BrokerFeed for MockFeed {
    fn broker(&self) -> &'static str {
        "mock"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("bad mock feed url".into()))
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let v: Vec<String> = subs
            .iter()
            .map(|s| format!("{}:{}:{}", s.exchange, s.symbol, s.mode.code()))
            .collect();
        vec![Message::Text(serde_json::json!({ "sub": v }).to_string())]
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let v: Vec<String> = subs
            .iter()
            .map(|s| format!("{}:{}:{}", s.exchange, s.symbol, s.mode.code()))
            .collect();
        vec![Message::Text(serde_json::json!({ "unsub": v }).to_string())]
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        let Message::Text(t) = msg else {
            return Vec::new();
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(t) else {
            return Vec::new();
        };
        match (v.get("t"), v.get("x"), v.get("p")) {
            (Some(s), Some(x), Some(p)) => vec![FeedEvent::Tick(super::common::NormalizedTick {
                symbol: s.as_str().unwrap_or_default().to_string(),
                exchange: x.as_str().unwrap_or_default().to_string(),
                mode: 1,
                ltp: p.as_f64().unwrap_or(0.0),
                timestamp_ms: super::common::streaming::now_ms(),
                ..Default::default()
            })],
            _ if v.get("order").is_some_and(|o| o.is_object()) => {
                let o = &v["order"];
                let s = |k: &str| o[k].as_str().unwrap_or_default().to_string();
                let i = |k: &str| o[k].as_i64().unwrap_or(0);
                let f = |k: &str| o[k].as_f64().unwrap_or(0.0);
                vec![FeedEvent::OrderUpdate(OrderUpdate {
                    orderid: s("orderid"),
                    symbol: s("symbol"),
                    exchange: s("exchange"),
                    action: s("action"),
                    quantity: i("quantity"),
                    price: f("price"),
                    trigger_price: f("trigger_price"),
                    pricetype: s("pricetype"),
                    product: s("product"),
                    order_status: s("order_status"),
                    filled_quantity: i("filled_quantity"),
                    pending_quantity: i("pending_quantity"),
                    average_price: f("average_price"),
                    rejection_reason: s("rejection_reason"),
                })]
            }
            _ if v.get("auth") == Some(&serde_json::json!("denied")) => {
                vec![FeedEvent::AuthFailed("denied".into())]
            }
            _ => Vec::new(),
        }
    }
}
