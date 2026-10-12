//! The order reconciler: one service that settles what an order update did
//! not (Codex LOG-08, ARCH-01, EV-01, EV-03).
//!
//! Order updates are events, and events can be lost: a placement whose
//! answer never came back, a critical queue that was full, a relay that fell
//! behind the feed, an order socket that reconnected while an order filled.
//! The broker's order book is the authority. Every [`PASS_INTERVAL`], and at
//! once after a gap, the reconciler reads it **once per destination** (Live
//! through the broker session, Sandbox through the sandbox engine) and
//! hands that one read to every owner of open orders (strategies, scalping,
//! OpenScript) to fold:
//!
//! * **an order with a broker id** is folded through the owner's own fill
//!   path, which is idempotent on cumulative quantities, so a fill applied
//!   twice is booked once;
//! * **an unconfirmed placement** (no definite answer from the broker) is
//!   looked up by its client tag where the broker reports one, else by its
//!   order fields among the orders nobody owns ([`find_placed`]). Found, it
//!   is bound and folded. Absent, the owner asks [`AbsenceTracker::judge`]:
//!   * **Sandbox**: an absence confirmed by [`ABSENT_READS`] reads over at
//!     least [`ABSENT_SPAN_SECS`] seconds releases the claim (the sandbox
//!     book is complete).
//!   * **Live, never on absence alone** (P0-3): a broker's book can lag or
//!     drop an order, so a confirmed absence holds the claim and raises a
//!     critical alert once. The order is released when the trader says it
//!     was not placed ([`OrderReconciler::confirm_not_placed`], from the
//!     Health page), or automatically only with corroboration: absent for
//!     [`LIVE_RELEASE_SECS`], no matching trade in the trade book, and the
//!     net position unchanged over that window.
//!
//!   Until then nothing is placed again.
//!
//! **Gaps.** A lost order fact opens a gap for the destination: a critical
//! bus delivery dropped, a critical subscriber failed, the order relay
//! lagged, the order feed (re)connected, a broker session started. While a
//! gap is open, new entries are refused ([`OrderReconciler::entry_refusal`])
//! and risk-reducing exits still run. A pass that read the book after the
//! gap opened closes it. Health shows open gaps and the counters behind
//! them.
//!
//! Hygiene: one owned task (a `JoinHandle`), aborted on shutdown and
//! restarted on every broker session change (logout, switch), so a pass
//! never applies one session's facts after another began. Owners are held
//! weakly. Every per-order map is pruned to the orders still unconfirmed.

use crate::clock::Clock;
use crate::events::{Event, EventBus, Lane, Subscriber, Topic};
use crate::strategy::dispatch::{BookOrder, OrderGateway, RunMode};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// How often open orders are checked against the broker's order book.
pub const PASS_INTERVAL: Duration = Duration::from_secs(5);
/// Consecutive reads that must miss an unconfirmed order before its
/// absence is confirmed.
pub const ABSENT_READS: u32 = 4;
/// ... spanning at least this long (a broker's book can lag its orders).
pub const ABSENT_SPAN_SECS: i64 = 20;
/// Live: an absent unconfirmed order may be released without the trader
/// only after this long, and only with corroboration (P0-3).
pub const LIVE_RELEASE_SECS: i64 = 600;
/// A book row timestamped this long before the placement cannot be it.
/// A book row timestamped more than this before the placement cannot be it
/// (clock skew between the broker and this computer; P1-4 tightened it from
/// 120 s).
pub const TIMESTAMP_SLACK_SECS: i64 = 60;

/// What a trader reads when a new entry waits for a reconcile.
pub const ENTRIES_PAUSED: &str = "New entries are paused for a moment while OpenAlgo re-checks \
the broker's order book after a gap in order updates. Stops and exits keep running. Try again in \
a few seconds.";

/// Why a destination's order facts may be incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GapCause {
    /// A critical bus delivery was dropped or its consumer failed.
    EventLoss,
    /// The broker order relay fell behind the feed and skipped updates.
    RelayLagged,
    /// The order feed connected or reconnected: updates in between are lost.
    OrderFeedReconnected,
}

impl GapCause {
    pub fn as_str(&self) -> &'static str {
        match self {
            GapCause::EventLoss => "event_loss",
            GapCause::RelayLagged => "relay_lagged",
            GapCause::OrderFeedReconnected => "order_feed_reconnected",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Gap {
    cause: GapCause,
    since: DateTime<Utc>,
    /// The gap sequence number when it opened; a pass that started after
    /// it closes it.
    seq: u64,
}

fn slot(mode: RunMode) -> usize {
    match mode {
        RunMode::Live => 0,
        RunMode::Sandbox => 1,
    }
}

const MODES: [RunMode; 2] = [RunMode::Live, RunMode::Sandbox];

/// The trade book and positions of one destination, read at most once per
/// pass and only when a Live absence needs corroborating (P0-3).
#[derive(Debug, Clone, Default)]
pub struct Corroboration {
    /// Trade book rows (`symbol`, `exchange`, `action`, `quantity`,
    /// `timestamp`, ...).
    pub trades: Vec<Value>,
    /// Position book rows (`symbol`, `exchange`, `product`, `quantity`).
    pub positions: Vec<Value>,
}

fn envelope_rows(body: &Value, key: &str) -> Vec<Value> {
    let data = body.get("data").unwrap_or(&Value::Null);
    data.get(key)
        .or(Some(data))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn text_of(row: &Value, key: &str) -> String {
    match row.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn qty_of(row: &Value, key: &str) -> Option<i64> {
    match row.get(key)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

impl Corroboration {
    /// From the service envelopes of `tradebook` and `positionbook`.
    pub fn from_books(trades: &Value, positions: &Value) -> Self {
        Self {
            trades: envelope_rows(trades, "trades"),
            positions: envelope_rows(positions, "positions"),
        }
    }

    /// Whether the trade book shows a fill that could be this order: same
    /// symbol, exchange and side, not timestamped before the placement. A
    /// row whose time cannot be read counts (never released on a guess).
    pub fn trade_seen(&self, u: &Unconfirmed<'_>) -> bool {
        let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
        let earliest = u
            .placed_at
            .map(|t| t - chrono::Duration::seconds(TIMESTAMP_SLACK_SECS));
        self.trades.iter().any(|t| {
            same(&text_of(t, "symbol"), u.symbol)
                && same(&text_of(t, "exchange"), u.exchange)
                && same(&text_of(t, "action"), u.action)
                && match (earliest, book_time(t)) {
                    (Some(e), Some(at)) => at >= e,
                    _ => true,
                }
        })
    }

    /// The net position on this instrument (product when given), or `None`
    /// when a matching row's quantity cannot be read.
    pub fn net(&self, symbol: &str, exchange: &str, product: Option<&str>) -> Option<i64> {
        let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
        let mut net = 0i64;
        for p in &self.positions {
            if !(same(&text_of(p, "symbol"), symbol) && same(&text_of(p, "exchange"), exchange)) {
                continue;
            }
            if let Some(want) = product.filter(|w| !w.trim().is_empty()) {
                let have = text_of(p, "product");
                if !have.is_empty() && !same(&have, want) {
                    continue;
                }
            }
            net = net.checked_add(qty_of(p, "quantity")?)?;
        }
        Some(net)
    }
}

/// Raises the critical "held order" alert (the reconciler's alert sink).
pub type HeldAlert = Arc<dyn Fn(String) + Send + Sync>;

/// One order-book read, shared by every owner in a pass.
#[derive(Clone)]
pub struct BookSnapshot {
    pub rows: Vec<BookOrder>,
    pub read_at: DateTime<Utc>,
    /// Every owner, asked whether a book order already belongs to it.
    pub owners: Vec<Arc<dyn OrderOwner>>,
    /// The broker reports every order's client tag (its capability).
    pub tags_reported: bool,
    /// Where the corroborating books are read from, on demand.
    source: Option<(Arc<dyn OrderGateway>, RunMode)>,
    corroboration: Arc<tokio::sync::OnceCell<Option<Arc<Corroboration>>>>,
    held_alert: Option<HeldAlert>,
}

impl BookSnapshot {
    pub fn new(rows: Vec<BookOrder>, read_at: DateTime<Utc>) -> Self {
        Self {
            rows,
            read_at,
            owners: Vec::new(),
            tags_reported: false,
            source: None,
            corroboration: Arc::new(tokio::sync::OnceCell::new()),
            held_alert: None,
        }
    }

    /// A read from a broker that reports every order's client tag.
    pub fn with_tags(self, tags_reported: bool) -> Self {
        Self {
            tags_reported,
            ..self
        }
    }

    /// Find an unconfirmed placement in this read ([`find_placed`]).
    pub fn find<'a>(&'a self, u: &Unconfirmed<'_>, bound: &dyn Fn(&str) -> bool) -> Found<'a> {
        find_placed(&self.rows, u, bound, self.tags_reported)
    }

    /// A read whose trade book and positions are already known (tests).
    pub fn with_corroboration(self, c: Corroboration) -> Self {
        let cell = tokio::sync::OnceCell::new();
        let _ = cell.set(Some(Arc::new(c)));
        Self {
            corroboration: Arc::new(cell),
            ..self
        }
    }

    /// Read corroborating books from `gateway` when an owner asks.
    pub fn with_source(self, gateway: Arc<dyn OrderGateway>, mode: RunMode) -> Self {
        Self {
            source: Some((gateway, mode)),
            ..self
        }
    }

    /// Send held-order alerts here.
    pub fn with_held_alert(self, alert: HeldAlert) -> Self {
        Self {
            held_alert: Some(alert),
            ..self
        }
    }

    /// The trade book and positions, read once for this pass. `None` when
    /// they cannot be read: nothing is then released on them.
    pub async fn corroboration(&self) -> Option<Arc<Corroboration>> {
        self.corroboration
            .get_or_init(|| async {
                let (gw, mode) = self.source.as_ref()?;
                let trades = gw.book(*mode, crate::strategy::dispatch::Book::Trades).await;
                let positions = gw
                    .book(*mode, crate::strategy::dispatch::Book::Positions)
                    .await;
                match (trades, positions) {
                    (Ok(t), Ok(p)) => Some(Arc::new(Corroboration::from_books(&t, &p))),
                    _ => {
                        tracing::warn!(
                            "Trade book or positions could not be read to check an unconfirmed order"
                        );
                        None
                    }
                }
            })
            .await
            .clone()
    }

    /// Raise the critical alert for an order held for the trader.
    pub fn alert_held(&self, message: String) {
        tracing::error!("{}", message);
        if let Some(a) = &self.held_alert {
            a(message);
        }
    }

    pub fn by_id(&self, order_id: &str) -> Option<&BookOrder> {
        self.rows.iter().find(|r| r.order_id == order_id)
    }

    /// Whether a book order already belongs to some owner (never a match
    /// for an unconfirmed placement).
    pub fn owned(&self, order_id: &str) -> bool {
        self.owners.iter().any(|o| o.owns_order(order_id))
    }
}

/// An owner of orders: folds a book read into its own state.
#[async_trait::async_trait]
pub trait OrderOwner: Send + Sync {
    fn name(&self) -> &'static str;
    /// Whether this owner holds working or unconfirmed orders in `mode`
    /// (a pass reads a destination's book only when someone does).
    fn has_open_orders(&self, mode: RunMode) -> bool;
    /// Whether this broker order id is already one of this owner's orders.
    fn owns_order(&self, order_id: &str) -> bool;
    /// Fold one book read. Returns how many unconfirmed placements are
    /// still unresolved.
    async fn reconcile(&self, mode: RunMode, book: &BookSnapshot) -> usize;
    /// Live unconfirmed orders held for the trader's decision (P0-3).
    fn held_orders(&self) -> Vec<HeldOrder> {
        Vec::new()
    }
    /// The trader says the held order `id` was not placed: release its
    /// claim and manage the position again. `Ok(false)` when `id` is not
    /// one of this owner's held orders; `Err` is a trader-facing refusal.
    async fn confirm_not_placed(&self, _id: &str) -> Result<bool, String> {
        Ok(false)
    }
}

/// A Live unconfirmed order whose absence from the broker's book is
/// confirmed but not proven: its claim is held until the trader decides
/// or corroboration releases it (P0-3).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HeldOrder {
    /// The owner's name (filled in by the reconciler).
    pub owner: String,
    /// The owner's id for the order, passed back to `confirm_not_placed`.
    pub id: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    /// When its first read missed it.
    pub since: DateTime<Utc>,
    /// Trader-facing description.
    pub message: String,
    /// The rest of the order, for re-checking the broker's books before
    /// the trader's "not placed" is accepted (review P1-3). Not shown.
    #[serde(skip)]
    pub product: Option<String>,
    #[serde(skip)]
    pub client_tag: Option<String>,
    #[serde(skip)]
    pub placed_at: Option<DateTime<Utc>>,
}

impl HeldOrder {
    /// The order as the matcher reads it.
    pub fn unconfirmed(&self) -> Unconfirmed<'_> {
        Unconfirmed {
            symbol: &self.symbol,
            exchange: &self.exchange,
            action: &self.action,
            quantity: self.quantity,
            product: self.product.as_deref(),
            client_tag: self.client_tag.as_deref(),
            placed_at: self.placed_at,
            net_before: None,
            // Not kept: any shape matches, so the re-check is the stricter.
            pricetype: None,
            price: None,
            trigger_price: None,
        }
    }
}

/// What a confirmed absence lets the owner do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Absence {
    /// Not yet confirmed: keep waiting.
    Wait,
    /// Settle it as never placed and release the claim.
    NotPlaced,
    /// Live: keep the claim. `alert` is true the first time, when the owner
    /// raises its critical alert.
    Hold { alert: bool },
}

/// The trader-facing text of a held order.
pub fn held_message(what: &str, u: &Unconfirmed<'_>) -> String {
    format!(
        "{}: the broker did not confirm the {} {} {} on {}, and it is not in the order book. It may still be at the broker, so it is not sent again and the position stays managed as it is. Check the broker's order book and trade book. If the order is not there, mark it as not placed on the Health page to resume.",
        what, u.action, u.quantity, u.symbol, u.exchange
    )
}

/// Who may open new exposure right now.
pub trait EntryGate: Send + Sync {
    /// A trader-facing refusal while new entries must wait.
    fn entry_refusal(&self, mode: RunMode) -> Option<String>;
}

// ------------------------------------------------------------ matching

/// An unconfirmed placement, as its owner recorded it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unconfirmed<'a> {
    pub symbol: &'a str,
    pub exchange: &'a str,
    /// `BUY` / `SELL`.
    pub action: &'a str,
    pub quantity: i64,
    /// Compared only when both sides name one.
    pub product: Option<&'a str>,
    pub client_tag: Option<&'a str>,
    pub placed_at: Option<DateTime<Utc>>,
    /// The owner's net position on this instrument just before the order
    /// was sent (signed). A Live absence is released without the trader
    /// only while the broker still shows exactly this (review P1-2);
    /// `None` never releases.
    pub net_before: Option<i64>,
    /// `MARKET`, `LIMIT`, `SL`, `SL-M` as sent (P1-4). A MARKET order may
    /// show as LIMIT and an SL-M as SL (market price protection).
    pub pricetype: Option<&'a str>,
    /// The limit price, compared for LIMIT and SL orders when both sides
    /// carry one.
    pub price: Option<f64>,
    /// The trigger price, compared for SL and SL-M orders.
    pub trigger_price: Option<f64>,
}

/// What the book says about an unconfirmed placement.
#[derive(Debug, Clone, PartialEq)]
pub enum Found<'a> {
    Order(&'a BookOrder),
    /// Not in this read.
    Absent,
    /// Several orders fit and nothing tells them apart: never guessed.
    Ambiguous(usize),
}

/// A book row's own timestamp, read as IST (the brokers' local time).
pub fn book_time(row: &Value) -> Option<DateTime<Utc>> {
    let text = row.get("timestamp").and_then(Value::as_str)?.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(text) {
        return Some(t.with_timezone(&Utc));
    }
    for f in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%d-%m-%Y %H:%M:%S",
        "%d-%b-%Y %H:%M:%S",
        "%d/%m/%Y %H:%M:%S",
        "%H:%M:%S %d-%m-%Y",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(n) = NaiveDateTime::parse_from_str(text, f) {
            return chrono_tz::Asia::Kolkata
                .from_local_datetime(&n)
                .single()
                .map(|t| t.with_timezone(&Utc));
        }
    }
    None
}

/// `SL-M`, `SL_M`, `slm` -> `SLM`.
fn price_kind(t: &str) -> String {
    t.trim()
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Whether a book row has the order's shape: the price type it was sent
/// with (a MARKET order may show as LIMIT, an SL-M as SL, after market
/// price protection), and its limit and trigger prices where they apply.
/// A side that does not say is not compared.
fn same_shape(u: &Unconfirmed<'_>, r: &BookOrder) -> bool {
    let Some(sent) = u.pricetype.map(price_kind).filter(|k| !k.is_empty()) else {
        return true;
    };
    let shown = price_kind(&r.pricetype);
    if shown.is_empty() {
        return true;
    }
    let compatible =
        sent == shown || (sent == "MARKET" && shown == "LIMIT") || (sent == "SLM" && shown == "SL");
    if !compatible {
        return false;
    }
    let close = |a: f64, b: f64| (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0);
    if sent == shown && (sent == "LIMIT" || sent == "SL") {
        if let Some(p) = u.price.filter(|p| *p > 0.0) {
            if r.price > 0.0 && !close(r.price, p) {
                return false;
            }
        }
    }
    if sent == "SL" || sent == "SLM" {
        if let Some(t) = u.trigger_price.filter(|t| *t > 0.0) {
            if r.trigger_price > 0.0 && !close(r.trigger_price, t) {
                return false;
            }
        }
    }
    true
}

/// Find an unconfirmed placement in one book read (P1-4).
///
/// * When the broker reports the client tag of every order
///   (`tags_reported`, its capability, never inferred from the rows) and
///   the placement carried one, only that tag matches.
/// * Otherwise the order's symbol, exchange, side, quantity, product and
///   shape (price type, limit and trigger prices) must match; the row must
///   not already belong to anyone (`bound`) nor carry another order's tag;
///   and its own timestamp must be readable and no more than
///   [`TIMESTAMP_SLACK_SECS`] before the placement. A row whose time
///   cannot be read is never taken: it could be any earlier order.
pub fn find_placed<'a>(
    book: &'a [BookOrder],
    u: &Unconfirmed<'_>,
    bound: &dyn Fn(&str) -> bool,
    tags_reported: bool,
) -> Found<'a> {
    let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    let fields = |r: &&BookOrder| {
        same(&r.symbol, u.symbol)
            && same(&r.exchange, u.exchange)
            && same(&r.action, u.action)
            && r.quantity == u.quantity
            && match (u.product, r.product.trim()) {
                (Some(p), rp) if !p.trim().is_empty() && !rp.is_empty() => same(p, rp),
                _ => true,
            }
            && same_shape(u, r)
    };
    let tag = u.client_tag.map(str::trim).filter(|t| !t.is_empty());
    let count = |hits: Vec<&'a BookOrder>| match hits.len() {
        0 => Found::Absent,
        1 => Found::Order(hits[0]),
        n => Found::Ambiguous(n),
    };
    if let (Some(tag), true) = (tag, tags_reported) {
        return count(
            book.iter()
                .filter(|r| r.client_tag.as_deref().map(str::trim) == Some(tag))
                .filter(fields)
                .collect(),
        );
    }
    let earliest = u
        .placed_at
        .map(|t| t - chrono::Duration::seconds(TIMESTAMP_SLACK_SECS));
    count(
        book.iter()
            .filter(fields)
            .filter(|r| !bound(&r.order_id))
            // Another order's tag is never this one's.
            .filter(|r| {
                match r
                    .client_tag
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                {
                    Some(t) => Some(t) == tag,
                    None => true,
                }
            })
            .filter(|r| match book_time(&r.row) {
                Some(t) => earliest.is_none_or(|e| t >= e),
                None => false,
            })
            .collect(),
    )
}

/// One unconfirmed placement the book has not shown.
struct Miss {
    since: DateTime<Utc>,
    reads: u32,
    /// Live: held for the trader, with its description.
    held: Option<HeldOrder>,
}

/// Counts reads that missed an unconfirmed placement and holds Live ones
/// for the trader. Bounded by the placements still unconfirmed: owners
/// prune it every pass.
#[derive(Default)]
pub struct AbsenceTracker {
    misses: Mutex<HashMap<String, Miss>>,
}

impl AbsenceTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a read without the order; true once its absence is confirmed
    /// ([`ABSENT_READS`] reads over at least [`ABSENT_SPAN_SECS`]). This is
    /// not a release decision: use [`AbsenceTracker::judge`].
    pub fn absent(&self, key: &str, now: DateTime<Utc>) -> bool {
        let mut m = self.misses.lock();
        let e = m.entry(key.to_string()).or_insert(Miss {
            since: now,
            reads: 0,
            held: None,
        });
        e.reads += 1;
        e.reads >= ABSENT_READS && (now - e.since).num_seconds() >= ABSENT_SPAN_SECS
    }

    /// Record a read that missed the unconfirmed placement `u` (owner key
    /// `key`, trader-facing id `id`) and say what its owner may do:
    ///
    /// * Sandbox: [`Absence::NotPlaced`] once the absence is confirmed.
    /// * Live: never on absence alone. [`Absence::Hold`] (alert once), and
    ///   [`Absence::NotPlaced`] only after [`LIVE_RELEASE_SECS`], with no
    ///   working look-alike in the order book, no matching trade in the
    ///   trade book, and the net position still exactly what the owner had
    ///   before sending (`u.net_before`). The trade book and positions are
    ///   read only then, not on every pass.
    ///
    /// The owner forgets the key once it settles the order.
    pub async fn judge(
        &self,
        key: &str,
        id: &str,
        mode: RunMode,
        book: &BookSnapshot,
        u: &Unconfirmed<'_>,
    ) -> Absence {
        let now = book.read_at;
        if !self.absent(key, now) {
            return Absence::Wait;
        }
        if mode == RunMode::Sandbox {
            return Absence::NotPlaced;
        }
        let Some(since) = self.misses.lock().get(key).map(|e| e.since) else {
            return Absence::Wait;
        };
        let long = (now - since).num_seconds() >= LIVE_RELEASE_SECS;
        // A working order of the same instrument and side that was not
        // matched (an unreadable time, a changed price) could be this one.
        // A row another order already owns is accounted for.
        let lookalike = book.rows.iter().any(|r| {
            r.symbol.trim().eq_ignore_ascii_case(u.symbol.trim())
                && r.exchange.trim().eq_ignore_ascii_case(u.exchange.trim())
                && r.action.trim().eq_ignore_ascii_case(u.action.trim())
                && !matches!(
                    r.status.trim(),
                    "complete"
                        | "completed"
                        | "filled"
                        | "rejected"
                        | "cancelled"
                        | "canceled"
                        | "expired"
                )
                && !book.owned(&r.order_id)
        });
        if long && !lookalike && u.net_before.is_some() {
            // Read without holding the lock (a network read), and only now.
            let corr = book.corroboration().await;
            let net_now = corr
                .as_ref()
                .and_then(|c| c.net(u.symbol, u.exchange, u.product));
            let traded = corr.as_ref().map(|c| c.trade_seen(u));
            if traded == Some(false) && net_now.is_some() && net_now == u.net_before {
                tracing::warn!(
                    "Unconfirmed {} {} {} on {}: absent for {} s, no matching trade, position as before sending; released as not placed",
                    u.action,
                    u.quantity,
                    u.symbol,
                    u.exchange,
                    (now - since).num_seconds()
                );
                return Absence::NotPlaced;
            }
        }
        let mut m = self.misses.lock();
        let Some(e) = m.get_mut(key) else {
            return Absence::Wait;
        };
        let alert = e.held.is_none();
        if alert {
            e.held = Some(HeldOrder {
                owner: String::new(),
                id: id.to_string(),
                symbol: u.symbol.to_string(),
                exchange: u.exchange.to_string(),
                action: u.action.to_string(),
                quantity: u.quantity,
                since: e.since,
                message: String::new(),
                product: u.product.map(str::to_string),
                client_tag: u.client_tag.map(str::to_string),
                placed_at: u.placed_at,
            });
        }
        Absence::Hold { alert }
    }

    /// Describe a held order (after the owner raised its alert).
    pub fn describe(&self, key: &str, message: &str) {
        if let Some(h) = self
            .misses
            .lock()
            .get_mut(key)
            .and_then(|e| e.held.as_mut())
        {
            h.message = message.to_string();
        }
    }

    /// The orders held for the trader.
    pub fn held(&self) -> Vec<HeldOrder> {
        let mut v: Vec<HeldOrder> = self
            .misses
            .lock()
            .values()
            .filter_map(|e| e.held.clone())
            .collect();
        v.sort_by(|a, b| a.since.cmp(&b.since).then_with(|| a.id.cmp(&b.id)));
        v
    }

    /// Keep only the keys `keep` accepts (an owner pruning entries whose
    /// orders are gone).
    pub fn retain_by(&self, keep: impl Fn(&str) -> bool) {
        self.misses.lock().retain(|k, _| keep(k));
    }

    /// The owner key of the held order with this trader-facing id.
    pub fn held_key(&self, id: &str) -> Option<String> {
        self.misses
            .lock()
            .iter()
            .find(|(_, e)| e.held.as_ref().is_some_and(|h| h.id == id))
            .map(|(k, _)| k.clone())
    }

    /// The order was seen (or settled): start over.
    pub fn forget(&self, key: &str) {
        self.misses.lock().remove(key);
    }

    /// Keep only the keys still unconfirmed.
    pub fn retain(&self, live: &HashSet<String>) {
        self.misses.lock().retain(|k, _| live.contains(k));
    }

    /// As `retain`, for the keys of one scope (one destination's pass)
    /// only: keys starting with `scope` and not in `live` go.
    pub fn retain_scope(&self, scope: &str, live: &HashSet<String>) {
        self.misses
            .lock()
            .retain(|k, _| !k.starts_with(scope) || live.contains(k));
    }

    pub fn len(&self) -> usize {
        self.misses.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ------------------------------------------------------------ the service

/// What the health alert for lost order facts should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactAlert {
    /// An order fact may have been lost (a critical delivery dropped or
    /// failed, the relay fell behind).
    Raised(GapCause),
    /// Every gap is repaired.
    Resolved,
    /// A Live unconfirmed order is held for the trader (P0-3): trader text.
    Held(String),
    /// No unconfirmed order is left.
    HeldResolved,
}

/// Where fact alerts go (the health alert table in the app).
pub type AlertSink = Arc<dyn Fn(FactAlert) + Send + Sync>;

/// Counters health reads.
#[derive(Debug, Default)]
pub struct ReconcileStats {
    pub passes: AtomicU64,
    pub book_reads: AtomicU64,
    pub book_read_failures: AtomicU64,
    pub gaps_opened: AtomicU64,
    pub gaps_closed: AtomicU64,
    pub unresolved: AtomicU64,
    /// Passes asked for at once (an uncertain placement was just recorded).
    pub requested: AtomicU64,
}

pub struct OrderReconciler {
    gateway: Arc<dyn OrderGateway>,
    clock: Arc<dyn Clock>,
    bus: Option<Arc<EventBus>>,
    owners: RwLock<Vec<Arc<dyn OrderOwner>>>,
    gaps: Mutex<[Option<Gap>; 2]>,
    gap_seq: AtomicU64,
    wake: Notify,
    task: Mutex<Option<JoinHandle<()>>>,
    /// One pass at a time.
    pass_lock: tokio::sync::Mutex<()>,
    pub stats: ReconcileStats,
    /// Order-feed status streams a reconnect is detected on.
    feeds: Mutex<Vec<tokio::sync::watch::Receiver<crate::websocket::FeedStatus>>>,
    alert_sink: std::sync::OnceLock<AlertSink>,
    /// A fact-loss alert is raised and not yet resolved.
    alerting: std::sync::atomic::AtomicBool,
    /// A held-order alert is raised and not yet resolved.
    held_alerting: Arc<std::sync::atomic::AtomicBool>,
    /// The bus's fact-loss count already acted on (survives a restart).
    gaps_seen: AtomicU64,
    /// Of those, the relay's.
    relays_seen: AtomicU64,
}

impl OrderReconciler {
    pub fn new(
        gateway: Arc<dyn OrderGateway>,
        clock: Arc<dyn Clock>,
        bus: Option<Arc<EventBus>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            gateway,
            clock,
            bus,
            owners: RwLock::new(Vec::new()),
            gaps: Mutex::new([None, None]),
            gap_seq: AtomicU64::new(0),
            wake: Notify::new(),
            task: Mutex::new(None),
            pass_lock: tokio::sync::Mutex::new(()),
            stats: ReconcileStats::default(),
            feeds: Mutex::new(Vec::new()),
            alert_sink: std::sync::OnceLock::new(),
            alerting: std::sync::atomic::AtomicBool::new(false),
            held_alerting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            gaps_seen: AtomicU64::new(0),
            relays_seen: AtomicU64::new(0),
        })
    }

    /// The held-order alert a pass hands its owners.
    fn held_alert(&self) -> HeldAlert {
        let sink = self.alert_sink.get().cloned();
        let flag = self.held_alerting.clone();
        Arc::new(move |message: String| {
            flag.store(true, Ordering::SeqCst);
            if let Some(s) = &sink {
                s(FactAlert::Held(message));
            }
        })
    }

    /// Every Live unconfirmed order held for the trader, across owners.
    pub fn held_orders(&self) -> Vec<HeldOrder> {
        let mut out = Vec::new();
        for o in self.owners() {
            for mut h in o.held_orders() {
                h.owner = o.name().to_string();
                out.push(h);
            }
        }
        out
    }

    /// The trader confirms that a held order was not placed (checked in the
    /// broker's order book and trade book): its owner releases the claim and
    /// manages the position again. Then a pass runs at once.
    pub async fn confirm_not_placed(&self, owner: &str, id: &str) -> Result<(), String> {
        let Some(o) = self.owners().into_iter().find(|o| o.name() == owner) else {
            return Err("That order is no longer held. Refresh the page.".into());
        };
        // Under the pass lock: no pass binds the order while it is released.
        let decided = {
            let _one = self.pass_lock.lock().await;
            let Some(held) = o.held_orders().into_iter().find(|h| h.id == id) else {
                return Err("That order is no longer held. Refresh the page.".into());
            };
            // The trader's word is checked against the broker's books once
            // more: an order or a trade that has turned up since keeps it
            // held (review P1-3).
            self.recheck_not_placed(&held).await?;
            o.confirm_not_placed(id).await
        };
        match decided {
            Ok(true) => {
                tracing::warn!("Trader confirmed held {} order {} as not placed", owner, id);
                self.wake.notify_one();
                Ok(())
            }
            Ok(false) => Err("That order is no longer held. Refresh the page.".into()),
            Err(e) => Err(e),
        }
    }

    /// Read the order book and the trade book once, and refuse "not placed"
    /// when either shows the held order (a matching order, several, or a
    /// matching trade) or cannot be read.
    async fn recheck_not_placed(&self, held: &HeldOrder) -> Result<(), String> {
        let u = held.unconfirmed();
        let owners = self.owners();
        let rows = self
            .gateway
            .order_book_rows(RunMode::Live)
            .await
            .map_err(|e| {
                tracing::warn!("Order book unreadable while checking a held order: {}", e);
                "OpenAlgo could not read the broker's order book to check this order. Nothing was changed. Try again.".to_string()
            })?;
        let bound = |oid: &str| owners.iter().any(|o| o.owns_order(oid));
        // Tags are not relied on here: any row of the same order counts.
        if !matches!(find_placed(&rows, &u, &bound, false), Found::Absent) {
            return Err("The broker's order book now shows an order that matches it, so it was not marked. OpenAlgo picks it up from the book; check the order book.".into());
        }
        let trades = self
            .gateway
            .book(RunMode::Live, crate::strategy::dispatch::Book::Trades)
            .await
            .map_err(|_| {
                "OpenAlgo could not read the broker's trade book to check this order. Nothing was changed. Try again.".to_string()
            })?;
        if Corroboration::from_books(&trades, &Value::Null).trade_seen(&u) {
            return Err("The broker's trade book shows a matching trade, so it was not marked. Check the position at the broker.".into());
        }
        Ok(())
    }

    /// Send fact-loss alerts here (set once, by the app).
    pub fn set_alert_sink(&self, sink: AlertSink) {
        let _ = self.alert_sink.set(sink);
    }

    /// An order fact may have been lost: raise the health alert and open a
    /// gap for each destination someone holds orders in.
    pub fn report_fact_loss(&self, relay: bool) {
        let cause = if relay {
            GapCause::RelayLagged
        } else {
            GapCause::EventLoss
        };
        if let Some(sink) = self.alert_sink.get() {
            self.alerting.store(true, Ordering::SeqCst);
            sink(FactAlert::Raised(cause));
        }
        if relay {
            // The broker relay carries Live updates only.
            self.open_gap(RunMode::Live, cause);
        } else {
            for m in MODES {
                self.open_gap(m, cause);
            }
        }
    }

    /// Add an owner of orders. Owners hold their module weakly, so this
    /// keeps nothing alive.
    pub fn register(&self, owner: Arc<dyn OrderOwner>) {
        self.owners.write().push(owner);
    }

    /// Watch an order-carrying feed: every connect opens a Live gap.
    pub fn watch_feed(&self, rx: tokio::sync::watch::Receiver<crate::websocket::FeedStatus>) {
        self.feeds.lock().push(rx);
    }

    fn owners(&self) -> Vec<Arc<dyn OrderOwner>> {
        self.owners.read().clone()
    }

    /// Open a gap for one destination (or keep the open one) and reconcile
    /// at once. A destination where nobody holds a working or unconfirmed
    /// order has nothing a lost fact could belong to: no gap, only a pass.
    pub fn open_gap(&self, mode: RunMode, cause: GapCause) {
        if !self.owners().iter().any(|o| o.has_open_orders(mode)) {
            self.wake.notify_one();
            return;
        }
        let seq = self.gap_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let now = self.clock.now();
        {
            let mut g = self.gaps.lock();
            let s = &mut g[slot(mode)];
            match s {
                Some(open) => {
                    // A later fault: the pass that closes it must start after.
                    open.seq = seq;
                }
                None => {
                    self.stats.gaps_opened.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        "Order facts for {} may be incomplete ({}); reconciling with the broker's order book",
                        mode.as_str(),
                        cause.as_str()
                    );
                    *s = Some(Gap {
                        cause,
                        since: now,
                        seq,
                    });
                }
            }
        }
        self.wake.notify_one();
    }

    /// Open gaps, for health: `(mode, cause, since)`.
    pub fn open_gaps(&self) -> Vec<(RunMode, GapCause, DateTime<Utc>)> {
        let g = self.gaps.lock();
        MODES
            .iter()
            .filter_map(|m| g[slot(*m)].as_ref().map(|x| (*m, x.cause, x.since)))
            .collect()
    }

    pub fn gap_open(&self, mode: RunMode) -> bool {
        self.gaps.lock()[slot(mode)].is_some()
    }

    /// Ask for a pass now (an uncertain placement was just recorded).
    pub fn request_pass(&self) {
        self.stats.requested.fetch_add(1, Ordering::Relaxed);
        self.wake.notify_one();
    }

    /// One pass: per destination with open orders, one book read, folded
    /// by every owner. Closes a destination's gap when its read succeeded
    /// (or nobody held anything there).
    pub async fn pass(&self) -> Value {
        let _one = self.pass_lock.lock().await;
        let seq_at_start = self.gap_seq.load(Ordering::SeqCst);
        self.stats.passes.fetch_add(1, Ordering::Relaxed);
        let owners = self.owners();
        let mut report = serde_json::Map::new();
        let mut unresolved_total = 0usize;
        for mode in MODES {
            let mut holders = Vec::new();
            for o in &owners {
                if o.has_open_orders(mode) {
                    holders.push(o.clone());
                }
            }
            let read_ok = if holders.is_empty() {
                true
            } else {
                self.stats.book_reads.fetch_add(1, Ordering::Relaxed);
                let tags = self.gateway.reports_order_tags(mode);
                match self.gateway.order_book_rows(mode).await {
                    Ok(rows) => {
                        let mut book = BookSnapshot::new(rows, self.clock.now())
                            .with_tags(tags)
                            .with_source(self.gateway.clone(), mode)
                            .with_held_alert(self.held_alert());
                        book.owners = owners.clone();
                        for o in &holders {
                            unresolved_total += o.reconcile(mode, &book).await;
                        }
                        true
                    }
                    Err(e) => {
                        self.stats
                            .book_read_failures
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            "Order book for {} could not be read to reconcile open orders: {}",
                            mode.as_str(),
                            e
                        );
                        false
                    }
                }
            };
            if read_ok {
                let closed = {
                    let mut g = self.gaps.lock();
                    let s = &mut g[slot(mode)];
                    if s.as_ref().is_some_and(|x| x.seq <= seq_at_start) {
                        s.take()
                    } else {
                        None
                    }
                };
                if let Some(gap) = closed {
                    self.stats.gaps_closed.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(
                        "Order facts for {} reconciled after {} (open {} s)",
                        mode.as_str(),
                        gap.cause.as_str(),
                        (self.clock.now() - gap.since).num_seconds()
                    );
                }
            }
            report.insert(
                mode.as_str().into(),
                json!({"owners": holders.len(), "read": read_ok}),
            );
        }
        self.stats
            .unresolved
            .store(unresolved_total as u64, Ordering::Relaxed);
        report.insert("unresolved".into(), json!(unresolved_total));
        let all_closed = self.gaps.lock().iter().all(Option::is_none);
        if all_closed && self.alerting.swap(false, Ordering::SeqCst) {
            if let Some(sink) = self.alert_sink.get() {
                sink(FactAlert::Resolved);
            }
        }
        // Every held order settled (found, released or confirmed). Read
        // from the owners, never from this pass's count: a pass whose book
        // read failed settled nothing (review P2-1).
        if self.held_orders().is_empty() && self.held_alerting.swap(false, Ordering::SeqCst) {
            if let Some(sink) = self.alert_sink.get() {
                sink(FactAlert::HeldResolved);
            }
        }
        Value::Object(report)
    }

    /// Health's view: open gaps, the counters, and whether management is
    /// degraded right now.
    pub fn health(&self) -> Value {
        let gaps: Vec<Value> = self
            .open_gaps()
            .into_iter()
            .map(|(m, c, since)| {
                json!({"mode": m.as_str(), "cause": c.as_str(), "since": since.to_rfc3339()})
            })
            .collect();
        let s = &self.stats;
        json!({
            "status": if gaps.is_empty() { "pass" } else { "fail" },
            "gaps": gaps,
            "running": self.is_running(),
            "passes": s.passes.load(Ordering::Relaxed),
            "book_reads": s.book_reads.load(Ordering::Relaxed),
            "book_read_failures": s.book_read_failures.load(Ordering::Relaxed),
            "gaps_opened": s.gaps_opened.load(Ordering::Relaxed),
            "gaps_closed": s.gaps_closed.load(Ordering::Relaxed),
            "unconfirmed_orders": s.unresolved.load(Ordering::Relaxed),
        })
    }

    // ------------------------------------------------------------ lifecycle

    /// Start the owned task (idempotent).
    pub fn start(self: &Arc<Self>) {
        let mut task = self.task.lock();
        if task.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        let weak = Arc::downgrade(self);
        let bus = self.bus.clone();
        // Clones: the feeds outlive a restart.
        let mut feeds: Vec<_> = self.feeds.lock().iter().cloned().collect();
        *task = Some(tokio::spawn(async move {
            let mut tick = tokio::time::interval(PASS_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                let Some(me) = weak.upgrade() else { break };
                let lost = async {
                    match &bus {
                        Some(b) => b.gap_notified().await,
                        None => std::future::pending().await,
                    }
                };
                let reconnect = feed_connected(&mut feeds);
                tokio::select! {
                    _ = tick.tick() => {}
                    _ = me.wake.notified() => {}
                    _ = lost => {}
                    _ = reconnect => {
                        me.open_gap(RunMode::Live, GapCause::OrderFeedReconnected);
                    }
                }
                if let Some(b) = &bus {
                    // Kept on the reconciler, not in this task: a loss
                    // reported while a restart was under way is still seen.
                    // The relay count is read first: a loss counted in the
                    // total but not yet as relay reads as an event loss,
                    // which opens both destinations' gaps.
                    let relay = b.relay_loss_count();
                    let total = b.gap_count();
                    let new_relay =
                        relay.saturating_sub(me.relays_seen.swap(relay, Ordering::SeqCst));
                    let new_total =
                        total.saturating_sub(me.gaps_seen.swap(total, Ordering::SeqCst));
                    if new_total > new_relay {
                        me.report_fact_loss(false);
                    }
                    if new_relay > 0 {
                        me.report_fact_loss(true);
                    }
                }
                me.pass().await;
                drop(me);
            }
        }));
    }

    /// Abort the owned task (shutdown).
    pub fn stop(&self) {
        if let Some(t) = self.task.lock().take() {
            t.abort();
        }
    }

    /// A broker session changed (logout, switch, sign-in): abort any pass in
    /// flight so it cannot apply the old session's facts, and start again.
    pub fn restart(self: &Arc<Self>) {
        self.stop();
        self.start();
    }

    pub fn is_running(&self) -> bool {
        self.task.lock().as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl EntryGate for OrderReconciler {
    fn entry_refusal(&self, mode: RunMode) -> Option<String> {
        self.gap_open(mode).then(|| ENTRIES_PAUSED.to_string())
    }
}

/// Resolves when any watched feed reports Connected; pending otherwise.
async fn feed_connected(feeds: &mut [tokio::sync::watch::Receiver<crate::websocket::FeedStatus>]) {
    if feeds.is_empty() {
        return std::future::pending().await;
    }
    let futs = feeds.iter_mut().map(|rx| {
        Box::pin(async move {
            loop {
                if rx.changed().await.is_err() {
                    return std::future::pending::<()>().await;
                }
                if matches!(
                    *rx.borrow_and_update(),
                    crate::websocket::FeedStatus::Connected { .. }
                ) {
                    return;
                }
            }
        })
    });
    futures_util::future::select_all(futs).await;
}

/// Restarts the reconciler on broker session changes and opens a Live gap
/// when a session starts.
struct Lifecycle {
    reconciler: Weak<OrderReconciler>,
}

#[async_trait::async_trait]
impl Subscriber for Lifecycle {
    fn name(&self) -> &'static str {
        "order-reconciler"
    }

    fn topics(&self) -> Vec<Topic> {
        vec![Topic::BrokerConnected, Topic::BrokerSessionEnded]
    }

    async fn handle(&self, event: Arc<Event>) {
        let Some(r) = self.reconciler.upgrade() else {
            return;
        };
        match event.as_ref() {
            Event::BrokerConnected { .. } => {
                // Fills may have happened while signed out: read the book
                // now. The order feed's own connect opens a gap if anything
                // is held.
                r.restart();
                r.wake.notify_one();
            }
            Event::BrokerSessionEnded { .. } => r.restart(),
            _ => {}
        }
    }
}

/// Subscribe the reconciler's lifecycle on the bus.
pub fn register(bus: &EventBus, reconciler: &Arc<OrderReconciler>) {
    bus.subscribe(
        Arc::new(Lifecycle {
            reconciler: Arc::downgrade(reconciler),
        }),
        Lane::Critical,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn row(
        id: &str,
        symbol: &str,
        action: &str,
        qty: i64,
        tag: Option<&str>,
        ts: &str,
    ) -> BookOrder {
        BookOrder {
            order_id: id.into(),
            symbol: symbol.into(),
            exchange: "NFO".into(),
            action: action.into(),
            quantity: qty,
            product: "NRML".into(),
            status: "complete".into(),
            filled_quantity: qty,
            average_price: 100.0,
            client_tag: tag.map(str::to_string),
            row: json!({"orderid": id, "timestamp": ts}),
            ..Default::default()
        }
    }

    fn u<'a>(tag: Option<&'a str>) -> Unconfirmed<'a> {
        Unconfirmed {
            symbol: "NIFTY24500CE",
            exchange: "NFO",
            action: "BUY",
            quantity: 75,
            product: Some("NRML"),
            client_tag: tag,
            placed_at: Some(Utc.with_ymd_and_hms(2026, 10, 7, 4, 30, 0).unwrap()),
            ..Default::default()
        }
    }

    #[test]
    fn a_tag_finds_exactly_its_order() {
        let book = vec![
            row("1", "NIFTY24500CE", "BUY", 75, Some("oaother"), ""),
            row("2", "NIFTY24500CE", "BUY", 75, Some("oamine"), ""),
        ];
        assert_eq!(
            find_placed(&book, &u(Some("oamine")), &|_| false, true),
            Found::Order(&book[1])
        );
        assert_eq!(
            find_placed(&book, &u(Some("oamissing")), &|_| false, true),
            Found::Absent
        );
    }

    /// P1-4: whether tags decide is the broker's capability, never the
    /// rows'. A broker that reports tags: only the tag matches, even when
    /// the rows carry none. One that does not: the order fields decide,
    /// and a row with another order's tag is never taken.
    #[test]
    fn tags_follow_the_broker_capability_not_the_rows() {
        let untagged = vec![row(
            "1",
            "NIFTY24500CE",
            "BUY",
            75,
            None,
            "2026-10-07 10:00:02",
        )];
        assert_eq!(
            find_placed(&untagged, &u(Some("oamine")), &|_| false, true),
            Found::Absent,
            "a tagging broker's untagged row is not this order"
        );
        assert_eq!(
            find_placed(&untagged, &u(Some("oamine")), &|_| false, false),
            Found::Order(&untagged[0]),
            "a broker without tags: the fields decide"
        );
        let foreign = vec![row(
            "2",
            "NIFTY24500CE",
            "BUY",
            75,
            Some("oaother"),
            "2026-10-07 10:00:02",
        )];
        assert_eq!(
            find_placed(&foreign, &u(None), &|_| false, false),
            Found::Absent,
            "another order's tag"
        );
        assert_eq!(
            find_placed(&foreign, &u(Some("oamine")), &|_| false, false),
            Found::Absent
        );
    }

    /// P1-4: a row whose time cannot be read could be any earlier order of
    /// the same fields; it is never bound to an unconfirmed placement.
    #[test]
    fn a_row_with_an_unreadable_time_is_never_taken() {
        for ts in ["", "10:00:03", "not a time"] {
            let book = vec![row("1", "NIFTY24500CE", "BUY", 75, None, ts)];
            assert_eq!(
                find_placed(&book, &u(None), &|_| false, false),
                Found::Absent,
                "timestamp {:?}",
                ts
            );
        }
    }

    /// P1-4: a row more than TIMESTAMP_SLACK_SECS before the placement is
    /// an earlier order (the slack was 120 s).
    #[test]
    fn an_earlier_order_inside_the_old_slack_is_not_taken() {
        // Placed at 10:00:00 IST; this row is from 09:58:30.
        let book = vec![row(
            "1",
            "NIFTY24500CE",
            "BUY",
            75,
            None,
            "2026-10-07 09:58:30",
        )];
        assert_eq!(
            find_placed(&book, &u(None), &|_| false, false),
            Found::Absent
        );
    }

    /// P1-4: the order's shape must match: price type (a MARKET order may
    /// show as LIMIT after market protection, an SL-M as SL), limit price
    /// and trigger price.
    #[test]
    fn the_order_shape_must_match() {
        let shaped = |pricetype: &str, price: f64, trigger: f64| {
            let mut r = row("1", "NIFTY24500CE", "BUY", 75, None, "2026-10-07 10:00:02");
            r.pricetype = pricetype.into();
            r.price = price;
            r.trigger_price = trigger;
            vec![r]
        };
        let sent = |pricetype: &'static str, price: f64, trigger: f64| Unconfirmed {
            pricetype: Some(pricetype),
            price: Some(price),
            trigger_price: Some(trigger),
            ..u(None)
        };
        let found = |book: &[BookOrder], s: &Unconfirmed<'_>| {
            matches!(find_placed(book, s, &|_| false, false), Found::Order(_))
        };
        assert!(found(
            &shaped("LIMIT", 100.0, 0.0),
            &sent("LIMIT", 100.0, 0.0)
        ));
        assert!(!found(
            &shaped("LIMIT", 101.0, 0.0),
            &sent("LIMIT", 100.0, 0.0)
        ));
        assert!(!found(&shaped("SL", 99.0, 98.0), &sent("MARKET", 0.0, 0.0)));
        assert!(found(
            &shaped("LIMIT", 104.5, 0.0),
            &sent("MARKET", 0.0, 0.0)
        ));
        assert!(found(&shaped("SL", 99.0, 98.0), &sent("SL-M", 0.0, 98.0)));
        assert!(!found(&shaped("SL-M", 0.0, 97.0), &sent("SL-M", 0.0, 98.0)));
        assert!(!found(
            &shaped("LIMIT", 100.0, 0.0),
            &sent("SL", 100.0, 98.0)
        ));
        // A book that does not say the type is not compared.
        assert!(found(&shaped("", 0.0, 0.0), &sent("LIMIT", 100.0, 0.0)));
    }

    #[test]
    fn without_a_tag_owned_and_older_orders_are_never_taken() {
        // 10:00 IST is 04:30 UTC: the placement time.
        let book = vec![
            row(
                "old",
                "NIFTY24500CE",
                "BUY",
                75,
                None,
                "2026-10-07 09:15:00",
            ),
            row(
                "owned",
                "NIFTY24500CE",
                "BUY",
                75,
                None,
                "2026-10-07 10:00:01",
            ),
            row(
                "new",
                "NIFTY24500CE",
                "BUY",
                75,
                None,
                "2026-10-07 10:00:02",
            ),
            row(
                "other",
                "NIFTY24500CE",
                "SELL",
                75,
                None,
                "2026-10-07 10:00:02",
            ),
        ];
        let found = find_placed(&book, &u(None), &|id| id == "owned", false);
        assert_eq!(found, Found::Order(&book[2]));
        // Two unowned candidates: never a guess.
        let found = find_placed(&book, &u(None), &|_| false, false);
        assert_eq!(found, Found::Ambiguous(2));
    }

    // ------------------------------------------------------------ service

    use crate::strategy::dispatch::{Book, DispatchResult, OrderPayload, OrderStatusResult};
    use std::sync::atomic::AtomicBool;

    #[derive(Default)]
    struct Gw {
        reads: AtomicU64,
    }

    #[async_trait::async_trait]
    impl OrderGateway for Gw {
        async fn place(&self, _: RunMode, _: &OrderPayload) -> DispatchResult {
            DispatchResult::refused("unused")
        }
        async fn cancel(&self, _: RunMode, _: &str) -> DispatchResult {
            DispatchResult::refused("unused")
        }
        async fn order_status(&self, _: RunMode, _: &str) -> OrderStatusResult {
            OrderStatusResult::default()
        }
        fn authorised(&self, _: RunMode) -> Result<(), String> {
            Ok(())
        }
        fn broker_name(&self, _: RunMode) -> String {
            "fake".into()
        }
        async fn book(&self, _: RunMode, _: Book) -> Result<Value, Value> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"status": "success", "data": {"orders": []}}))
        }
        async fn ltp(&self, _: &str, _: &str) -> Result<f64, String> {
            Err("unused".into())
        }
    }

    #[derive(Default)]
    struct Holder {
        holds: AtomicBool,
        /// Also holds Sandbox orders.
        sandbox: AtomicBool,
        folds: AtomicU64,
    }

    #[async_trait::async_trait]
    impl OrderOwner for Holder {
        fn name(&self) -> &'static str {
            "holder"
        }
        fn has_open_orders(&self, mode: RunMode) -> bool {
            match mode {
                RunMode::Live => self.holds.load(Ordering::SeqCst),
                RunMode::Sandbox => self.sandbox.load(Ordering::SeqCst),
            }
        }
        fn owns_order(&self, _: &str) -> bool {
            false
        }
        async fn reconcile(&self, _: RunMode, _: &BookSnapshot) -> usize {
            self.folds.fetch_add(1, Ordering::SeqCst);
            0
        }
    }

    async fn until(f: impl Fn() -> bool) -> bool {
        for _ in 0..400 {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        false
    }

    /// ARCH-01: an order feed that reconnects may have missed fills: the
    /// destination is reconciled at once (one book read for every owner),
    /// and the gap closes with that read.
    #[tokio::test]
    async fn an_order_feed_reconnect_triggers_one_book_read_for_every_owner() {
        let gw = Arc::new(Gw::default());
        let clock =
            crate::clock::ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 7, 4, 30, 0).unwrap());
        let r = OrderReconciler::new(gw.clone(), clock, None);
        let a = Arc::new(Holder::default());
        let b = Arc::new(Holder::default());
        a.holds.store(true, Ordering::SeqCst);
        b.holds.store(true, Ordering::SeqCst);
        r.register(a.clone());
        r.register(b.clone());
        let (tx, rx) = tokio::sync::watch::channel(crate::websocket::FeedStatus::Disconnected);
        r.watch_feed(rx);
        r.start();
        // The task's first pass (the interval's first tick).
        assert!(until(|| r.stats.passes.load(Ordering::SeqCst) >= 1).await);
        let reads = gw.reads.load(Ordering::SeqCst);
        let opened = r.stats.gaps_opened.load(Ordering::SeqCst);
        tx.send(crate::websocket::FeedStatus::Connected {
            broker: "zerodha".into(),
        })
        .unwrap();
        assert!(
            until(|| r.stats.gaps_closed.load(Ordering::SeqCst) > 0).await,
            "the reconnect's gap is repaired"
        );
        assert_eq!(r.stats.gaps_opened.load(Ordering::SeqCst), opened + 1);
        assert!(gw.reads.load(Ordering::SeqCst) > reads);
        // One read per pass, shared: both owners folded every read.
        assert_eq!(
            a.folds.load(Ordering::SeqCst),
            gw.reads.load(Ordering::SeqCst)
        );
        assert_eq!(
            b.folds.load(Ordering::SeqCst),
            gw.reads.load(Ordering::SeqCst)
        );
        assert!(!r.gap_open(RunMode::Live));
        r.stop();
        assert!(until(|| !r.is_running()).await);
    }

    /// A fact loss reported while the reconciler's task is being restarted
    /// (a broker session change) is still acted on by the new task.
    #[tokio::test]
    async fn a_loss_reported_across_a_restart_still_opens_a_gap() {
        let gw = Arc::new(Gw::default());
        let clock =
            crate::clock::ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 7, 4, 30, 0).unwrap());
        let bus = Arc::new(EventBus::new());
        let r = OrderReconciler::new(gw, clock, Some(bus.clone()));
        let a = Arc::new(Holder::default());
        a.holds.store(true, Ordering::SeqCst);
        r.register(a);
        r.start();
        assert!(until(|| r.stats.passes.load(Ordering::SeqCst) >= 1).await);
        // The task is stopped (a restart under way) when the loss arrives.
        r.stop();
        bus.report_fact_loss(crate::events::bus::FactLoss::Event);
        r.start();
        assert!(
            until(|| r.stats.gaps_opened.load(Ordering::SeqCst) >= 1).await,
            "the new task acts on the loss"
        );
        r.stop();
    }

    /// Review: an event loss and a relay lag seen in the same pass open
    /// both destinations' gaps (a relay lag alone opens Live only).
    #[tokio::test]
    async fn an_event_loss_and_a_relay_lag_in_one_pass_open_both_gaps() {
        let gw = Arc::new(Gw::default());
        let clock =
            crate::clock::ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 7, 4, 30, 0).unwrap());
        let bus = Arc::new(EventBus::new());
        let r = OrderReconciler::new(gw, clock, Some(bus.clone()));
        let a = Arc::new(Holder::default());
        a.holds.store(true, Ordering::SeqCst);
        a.sandbox.store(true, Ordering::SeqCst);
        r.register(a);
        // Both reported before the task looks.
        bus.report_fact_loss(crate::events::bus::FactLoss::Event);
        bus.report_relay_lag(1);
        r.start();
        assert!(
            until(|| r.stats.gaps_opened.load(Ordering::SeqCst) >= 2).await,
            "Live and Sandbox gaps: {}",
            r.stats.gaps_opened.load(Ordering::SeqCst)
        );
        r.stop();
    }

    #[tokio::test]
    async fn nothing_held_means_no_gap_and_no_book_read() {
        let gw = Arc::new(Gw::default());
        let clock =
            crate::clock::ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 7, 4, 30, 0).unwrap());
        let r = OrderReconciler::new(gw.clone(), clock, None);
        r.register(Arc::new(Holder::default()));
        r.open_gap(RunMode::Live, GapCause::EventLoss);
        assert!(!r.gap_open(RunMode::Live));
        assert!(r.entry_refusal(RunMode::Live).is_none());
        r.pass().await;
        assert_eq!(gw.reads.load(Ordering::SeqCst), 0);
    }

    /// P0-3: Live never releases on absence alone; Sandbox does once the
    /// absence is confirmed.
    #[tokio::test]
    async fn live_absence_holds_and_sandbox_absence_releases() {
        let t0 = Utc.with_ymd_and_hms(2026, 10, 7, 4, 30, 0).unwrap();
        let at = |s: i64| BookSnapshot::new(vec![], t0 + chrono::Duration::seconds(s));
        let order = u(None);
        let live = AbsenceTracker::new();
        let sandbox = AbsenceTracker::new();
        let mut verdicts = Vec::new();
        for i in 0..=ABSENT_READS {
            let b = at(i64::from(i) * ABSENT_SPAN_SECS);
            verdicts.push(live.judge("l", "7", RunMode::Live, &b, &order).await);
            let s = sandbox.judge("s", "8", RunMode::Sandbox, &b, &order).await;
            if i + 1 < ABSENT_READS {
                assert_eq!(s, Absence::Wait);
            } else {
                assert_eq!(s, Absence::NotPlaced);
            }
        }
        let last = verdicts.len() - 1;
        assert_eq!(verdicts[last - 1], Absence::Hold { alert: true });
        assert_eq!(verdicts[last], Absence::Hold { alert: false });
        // Far past the window, but with nothing to corroborate it: held.
        assert_eq!(
            live.judge("l", "7", RunMode::Live, &at(LIVE_RELEASE_SECS * 10), &order)
                .await,
            Absence::Hold { alert: false }
        );
        let held = live.held();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].id, "7");
        assert_eq!(live.held_key("7").as_deref(), Some("l"));
        assert!(sandbox.held().is_empty());
        live.forget("l");
        assert!(live.held().is_empty());
    }

    /// Review: a working order of the same instrument and side that the
    /// matcher did not take (its time cannot be read) blocks the automatic
    /// release, even with no trade and an unchanged position.
    #[tokio::test]
    async fn a_working_lookalike_in_the_book_blocks_the_automatic_release() {
        let t0 = Utc.with_ymd_and_hms(2026, 10, 7, 4, 30, 0).unwrap();
        let corr = Corroboration::from_books(
            &json!({"data": []}),
            &json!({"data": [{"symbol": "NIFTY24500CE", "exchange": "NFO", "product": "NRML", "quantity": 0}]}),
        );
        let at = |s: i64, rows: Vec<BookOrder>| {
            BookSnapshot::new(rows, t0 + chrono::Duration::seconds(s))
                .with_corroboration(corr.clone())
        };
        let mut working = row("9", "NIFTY24500CE", "BUY", 75, None, "");
        working.status = "open".into();
        let live = AbsenceTracker::new();
        // Flat before the order was sent, flat now.
        let order = Unconfirmed {
            net_before: Some(0),
            ..u(None)
        };
        for i in 0..=ABSENT_READS {
            let b = at(i64::from(i) * ABSENT_SPAN_SECS, vec![]);
            live.judge("l", "7", RunMode::Live, &b, &order).await;
        }
        let late = LIVE_RELEASE_SECS + 60;
        assert_eq!(
            live.judge("l", "7", RunMode::Live, &at(late, vec![working]), &order)
                .await,
            Absence::Hold { alert: false }
        );
        assert_eq!(
            live.judge("l", "7", RunMode::Live, &at(late + 30, vec![]), &order)
                .await,
            Absence::NotPlaced
        );
    }

    /// Review P1-2: a Live release needs the broker's position to be what
    /// the owner had before sending. An exit that filled before the first
    /// read (the position already moved) is never released, however long
    /// it stays absent and whatever the later reads show.
    #[tokio::test]
    async fn a_live_release_compares_with_the_position_before_sending() {
        let t0 = Utc.with_ymd_and_hms(2026, 10, 7, 4, 30, 0).unwrap();
        let positions = |q: i64| {
            Corroboration::from_books(
                &json!({"data": []}),
                &json!({"data": [{"symbol": "NIFTY24500CE", "exchange": "NFO", "product": "NRML", "quantity": q}]}),
            )
        };
        // Long 75 before a SELL 75 whose answer was lost; it filled, so the
        // book shows 0 from the first read on.
        let order = Unconfirmed {
            action: "SELL",
            net_before: Some(75),
            ..u(None)
        };
        let live = AbsenceTracker::new();
        let at = |s: i64, q: i64| {
            BookSnapshot::new(vec![], t0 + chrono::Duration::seconds(s))
                .with_corroboration(positions(q))
        };
        for i in 0..=ABSENT_READS {
            let b = at(i64::from(i) * ABSENT_SPAN_SECS, 0);
            live.judge("l", "7", RunMode::Live, &b, &order).await;
        }
        assert_eq!(
            live.judge(
                "l",
                "7",
                RunMode::Live,
                &at(LIVE_RELEASE_SECS + 60, 0),
                &order
            )
            .await,
            Absence::Hold { alert: false },
            "the position moved: never released without the trader"
        );
        // The position as it was before sending, nothing traded: released.
        assert_eq!(
            live.judge(
                "l",
                "7",
                RunMode::Live,
                &at(LIVE_RELEASE_SECS + 90, 75),
                &order
            )
            .await,
            Absence::NotPlaced
        );
        // Without the position before sending, never.
        let unknown = AbsenceTracker::new();
        let blind = Unconfirmed {
            net_before: None,
            ..order
        };
        for i in 0..=ABSENT_READS {
            let b = at(i64::from(i) * ABSENT_SPAN_SECS, 75);
            unknown.judge("u", "8", RunMode::Live, &b, &blind).await;
        }
        assert_eq!(
            unknown
                .judge(
                    "u",
                    "8",
                    RunMode::Live,
                    &at(LIVE_RELEASE_SECS * 2, 75),
                    &blind
                )
                .await,
            Absence::Hold { alert: false }
        );
    }

    #[test]
    fn corroboration_reads_trades_and_positions_conservatively() {
        let c = Corroboration::from_books(
            &json!({"data": [
                {"symbol": "NIFTY24500CE", "exchange": "NFO", "action": "BUY", "quantity": 75,
                 "timestamp": "2026-10-07 09:00:00"}
            ]}),
            &json!({"data": [
                {"symbol": "NIFTY24500CE", "exchange": "NFO", "product": "NRML", "quantity": "75"},
                {"symbol": "NIFTY24500CE", "exchange": "NFO", "product": "MIS", "quantity": 10}
            ]}),
        );
        // A trade from before the placement is not this order's.
        assert!(!c.trade_seen(&u(None)));
        assert_eq!(c.net("NIFTY24500CE", "NFO", Some("NRML")), Some(75));
        assert_eq!(c.net("NIFTY24500CE", "NFO", None), Some(85));
        assert_eq!(c.net("BANKNIFTY", "NFO", None), Some(0));
        // A trade whose time cannot be read counts.
        let c = Corroboration::from_books(
            &json!({"data": [{"symbol": "NIFTY24500CE", "exchange": "NFO", "action": "BUY",
                              "timestamp": "10:00:03"}]}),
            &json!({"data": [{"symbol": "NIFTY24500CE", "exchange": "NFO", "quantity": "x"}]}),
        );
        assert!(c.trade_seen(&u(None)));
        assert_eq!(c.net("NIFTY24500CE", "NFO", None), None, "unreadable");
    }

    #[test]
    fn absence_needs_several_reads_over_time() {
        let t = AbsenceTracker::new();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 7, 4, 30, 0).unwrap();
        for i in 0..ABSENT_READS {
            assert!(!t.absent("a", t0 + chrono::Duration::seconds(i as i64)));
        }
        assert!(t.absent("a", t0 + chrono::Duration::seconds(ABSENT_SPAN_SECS)));
        t.retain(&HashSet::new());
        assert!(t.is_empty());
    }
}
