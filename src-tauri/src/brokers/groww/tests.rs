//! Groww mapping, protocol and adapter tests against payloads built from
//! the web code and Groww's documented shapes
//! (`src-tauri/tests/fixtures/brokers/groww/`). No real account data.

use super::auth::{
    checksum, choose_variant, login_error, looks_like_jwt, token_from_response, Variant,
};
use super::data::*;
use super::funds::{day_m2m, funds_from_payload, margin_groups, margin_requests, parse_margin};
use super::mapping::*;
use super::master_contract::parse_instruments;
use super::nkeys::{self, KeyPair};
use super::order_poller::{clamp_interval, diff};
use super::orders::{modify_order_body, place_order_body, place_outcome, validate};
use super::proto;
use super::rate_limiter::{self, paced, retry_delay, ApiType, Attempt, GrowwLimiter};
use super::streaming::*;
use super::*;
use crate::brokers::common::mapping::{Action, PriceType, Validity};
use crate::brokers::common::streaming::{
    BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message,
};
use prost::Message as _;
use serde_json::{json, Value};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../../tests/fixtures/brokers/groww/", $name))
    };
}

fn fx(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_instruments(fixture!("instrument.csv")).unwrap());
    r
}

fn core() -> GrowwCore {
    GrowwCore::new(master(), "http://127.0.0.1:9")
}

fn orders_of(v: &Value) -> Vec<GrowwOrder> {
    serde_json::from_value(v["payload"]["order_list"].clone()).unwrap()
}

// ---------------------------------------------------------------------------
// Rate limiter (web test_groww_rate_limiter.py)
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn calls_of_one_type_are_spaced_by_the_documented_per_minute_limit() {
    for (t, per_minute) in [
        (ApiType::Order, 250.0),
        (ApiType::Live, 300.0),
        (ApiType::NonTrading, 500.0),
        (ApiType::Auth, 30.0),
    ] {
        let l = GrowwLimiter::default();
        let start = tokio::time::Instant::now();
        for _ in 0..3 {
            l.acquire(t).await.unwrap();
        }
        let want = std::time::Duration::from_secs_f64(2.0 * 60.0 / per_minute);
        let got = start.elapsed();
        assert!(
            got >= want && got < want + std::time::Duration::from_millis(5),
            "{:?}: {:?} vs {:?}",
            t,
            got,
            want
        );
    }
    assert_eq!(
        ApiType::History.min_interval(),
        std::time::Duration::from_secs(1)
    );
}

#[tokio::test(start_paused = true)]
async fn types_do_not_wait_for_each_other() {
    let l = GrowwLimiter::default();
    let start = tokio::time::Instant::now();
    l.acquire(ApiType::Live).await.unwrap();
    l.acquire(ApiType::Order).await.unwrap();
    l.acquire(ApiType::NonTrading).await.unwrap();
    l.acquire(ApiType::History).await.unwrap();
    assert_eq!(start.elapsed(), std::time::Duration::ZERO);
}

/// Drive `paced` with scripted statuses; returns (calls, final status or
/// the refusal, time slept beyond pacing).
async fn script(
    statuses: &[u16],
    retry_after: Option<&str>,
) -> (usize, Result<u16>, std::time::Duration) {
    let l = GrowwLimiter::default();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let start = tokio::time::Instant::now();
    let st = statuses.to_vec();
    let ra = retry_after.map(str::to_string);
    let c = calls.clone();
    let last = paced(&l, ApiType::Live, move || {
        let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let status = st[n.min(st.len() - 1)];
        let ra = ra.clone();
        async move {
            Ok(Attempt {
                status,
                retry_after: ra,
                value: status,
            })
        }
    })
    .await;
    (
        calls.load(std::sync::atomic::Ordering::SeqCst),
        last,
        start.elapsed(),
    )
}

#[tokio::test(start_paused = true)]
async fn a_429_is_retried_after_the_delay_groww_asks_for() {
    let (calls, last, slept) = script(&[429, 200], Some("2")).await;
    assert_eq!((calls, last.unwrap()), (2, 200));
    assert!(slept >= std::time::Duration::from_secs(2));
    assert!(slept < std::time::Duration::from_millis(2300));
}

#[tokio::test(start_paused = true)]
async fn retries_stop_after_max_retries_and_return_the_429() {
    let n = rate_limiter::MAX_RETRIES as usize + 1;
    let (calls, last, slept) = script(&vec![429; n], None).await;
    assert_eq!((calls, last.unwrap()), (n, 429));
    // Exponential fallback when Groww sends no Retry-After: 1, 2, 4.
    assert!(slept >= std::time::Duration::from_secs(7));
    assert!(slept < std::time::Duration::from_millis(7500));
    assert_eq!(
        (0..3)
            .map(|a| retry_delay(None, a, std::time::SystemTime::now()).as_secs())
            .collect::<Vec<_>>(),
        [1, 2, 4]
    );
}

#[test]
fn an_http_date_retry_after_is_honoured() {
    let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_760_000_000);
    let when = chrono::DateTime::from_timestamp(1_760_000_003, 0)
        .unwrap()
        .to_rfc2822()
        .replace("+0000", "GMT");
    let d = retry_delay(Some(&when), 0, now);
    assert!(
        d >= std::time::Duration::from_millis(2900) && d <= std::time::Duration::from_millis(3100),
        "{when}: {d:?}"
    );
    // A date in the past still waits the 50 ms floor; junk falls back.
    assert_eq!(
        retry_delay(Some("Thu, 01 Jan 1970 00:00:00 GMT"), 0, now),
        std::time::Duration::from_millis(50)
    );
    assert_eq!(
        retry_delay(Some("soon"), 1, now),
        std::time::Duration::from_secs(2)
    );
}

/// Web test_under_gthread_a_long_server_delay_is_refused_not_slept: a wait
/// Groww asks for beyond the 10 s ceiling (web `cap_server_delay`) is
/// refused with a message saying the request was not retried.
#[tokio::test(start_paused = true)]
async fn a_long_server_delay_is_refused_not_slept() {
    for ra in ["600", "15", "11", "1e300", "inf"] {
        let (calls, last, slept) = script(&[429, 200], Some(ra)).await;
        assert_eq!(calls, 1, "{ra}");
        assert_eq!(
            last.unwrap_err().client_message(),
            rate_limiter::SLOW_DOWN_MESSAGE,
            "{ra}"
        );
        assert!(slept < std::time::Duration::from_secs(1), "{ra}");
    }
    // Up to the ceiling it is honoured.
    let (calls, last, slept) = script(&[429, 200], Some("9")).await;
    assert_eq!((calls, last.unwrap()), (2, 200));
    assert!(slept >= std::time::Duration::from_secs(9));
}

#[test]
fn retry_after_values_never_panic() {
    let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_760_000_000);
    assert_eq!(retry_delay(Some("1e300"), 0, now), std::time::Duration::MAX);
    assert_eq!(retry_delay(Some("inf"), 0, now), std::time::Duration::MAX);
    assert_eq!(
        retry_delay(Some("-5"), 0, now),
        std::time::Duration::from_millis(50)
    );
    assert_eq!(
        retry_delay(Some("NaN"), 2, now),
        std::time::Duration::from_secs(4)
    );
    assert_eq!(
        retry_delay(Some("Wed, 31 Dec 1969 23:59:59 GMT"), 0, now),
        std::time::Duration::from_millis(50)
    );
}

/// The web's `check_queue_wait`: a call whose turn is more than 10 s away
/// is refused at once and books nothing, so the calls queued behind it
/// are not delayed by it.
#[tokio::test(start_paused = true)]
async fn a_call_whose_turn_is_too_far_away_is_refused_before_it_books() {
    let l = GrowwLimiter::default();
    let start = tokio::time::Instant::now();
    // History is one a second: slots 0..=10 s are within the ceiling.
    let results =
        futures_util::future::join_all((0..12).map(|_| l.acquire(ApiType::History))).await;
    let refused: Vec<_> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].client_message(), rate_limiter::BUSY_MESSAGE);
    assert!(results[..11].iter().all(Result::is_ok));
    assert_eq!(start.elapsed(), std::time::Duration::from_secs(10));
    // The refused call booked nothing: the next slot is 11 s, not 12 s.
    l.acquire(ApiType::History).await.unwrap();
    assert_eq!(start.elapsed(), std::time::Duration::from_secs(11));
    // Other types were never held up.
    let t = tokio::time::Instant::now();
    l.acquire(ApiType::Order).await.unwrap();
    assert_eq!(t.elapsed(), std::time::Duration::ZERO);
}

/// Books `n` slots of `t` on `core`'s limiter from tasks that then wait
/// their turn, so the next call of `t` finds the queue that long.
async fn fill_queue(c: &GrowwCore, t: ApiType, n: usize) -> Vec<tokio::task::JoinHandle<()>> {
    let tasks: Vec<_> = (0..n)
        .map(|_| {
            let l = c.limiter.clone();
            tokio::spawn(async move {
                let _ = l.acquire(t).await;
            })
        })
        .collect();
    for _ in 0..(n + 2) {
        tokio::task::yield_now().await;
    }
    tasks
}

/// Web test_login_rate_limit_refusal_is_not_reported_as_unreachable: a
/// login OpenAlgo's own pacing refused is reported as that, not as Groww
/// being unreachable (nothing was sent).
#[tokio::test(start_paused = true)]
async fn login_pacing_refusal_is_not_reported_as_unreachable() {
    let c = core();
    // Authentication is 30 a minute (one every 2 s): six queued logins put
    // the seventh 12 s away.
    let held = fill_queue(&c, ApiType::Auth, 6).await;
    let e = super::auth::authenticate(
        &c,
        BrokerCredentials {
            api_key: "KEY".into(),
            api_secret: Some("SECRET".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(e.client_message(), rate_limiter::BUSY_MESSAGE);
    held.iter().for_each(|h| h.abort());
}

/// Web test_place_order_passes_broker_busy_through and
/// test_get_api_response_passes_broker_busy_through: OpenAlgo's own pacing
/// refusal reaches the caller as such, not as a Groww or network error.
#[tokio::test(start_paused = true)]
async fn pacing_refusals_pass_through_orders_and_data() {
    let c = core();
    let auth = AuthToken::new("good");
    // Orders are 250 a minute (0.24 s apart): 43 queued put the next one
    // more than 10 s away.
    let held = fill_queue(&c, ApiType::Order, 43).await;
    let o = resolved("SBIN", "NSE", "MARKET", 0.0, 0.0);
    let e = super::orders::place_order(&c, &auth, &o).await.unwrap_err();
    assert_eq!(e.client_message(), rate_limiter::BUSY_MESSAGE);
    held.iter().for_each(|h| h.abort());
    // Live data: 300 a minute (0.2 s apart): 52 queued.
    let held = fill_queue(&c, ApiType::Live, 52).await;
    let e = super::data::get_quote(&c, &auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap_err();
    assert_eq!(e.client_message(), rate_limiter::BUSY_MESSAGE);
    held.iter().for_each(|h| h.abort());
}

/// A call that fails in transit: an order call says to check the order
/// book before retrying (the order may have reached Groww), a login says
/// Groww could not be reached.
#[tokio::test]
async fn transport_failures_name_the_next_step() {
    // Port 9 on loopback is closed: every call fails in transit.
    let c = core();
    let auth = AuthToken::new("good");
    let o = resolved("SBIN", "NSE", "MARKET", 0.0, 0.0);
    let e = super::orders::place_order(&c, &auth, &o).await.unwrap_err();
    assert_eq!(
        e.client_message(),
        "Could not reach Groww to place the order. Check the order book before retrying."
    );
    let e = super::auth::authenticate(
        &c,
        BrokerCredentials {
            api_key: "KEY".into(),
            api_secret: Some("SECRET".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        e.client_message(),
        "Could not reach Groww to log in. Check your connection and try again."
    );
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[test]
fn master_contract_rows_and_exchanges() {
    let rows = parse_instruments(fixture!("instrument.csv")).unwrap();
    // 21 data rows; the one with a blank trading symbol is dropped.
    assert_eq!(rows.len(), 20);
    let r = master();
    let sbin = r.by_symbol("NSE", "SBIN").unwrap();
    assert_eq!(
        (
            sbin.brexchange.as_str(),
            sbin.token.as_str(),
            sbin.instrument_type.as_str()
        ),
        ("NSE", "3045", "EQ")
    );
    assert_eq!(sbin.name, "STATE BANK OF INDIA");
    assert_eq!(sbin.expiry, "");
    assert_eq!(r.by_symbol("BSE", "RELIANCE").unwrap().token, "500325");
    // ETF reads as EQ; a quoted name with a comma keeps the columns.
    let bees = r.by_symbol("NSE", "NIFTYBEES").unwrap();
    assert_eq!(bees.instrument_type, "EQ");
    assert_eq!(bees.token, "0532");
    assert_eq!(bees.name, "NIPPON INDIA ETF NIFTY 50 BEES, GROWTH");
    // NaN lot size -> 1, blank tick -> 0.05.
    let infy = r.by_symbol("NSE", "INFY").unwrap();
    assert_eq!((infy.lot_size, infy.tick_size), (1, 0.05));
}

#[test]
fn master_contract_indices_are_eq_rows_on_index_exchanges() {
    let r = master();
    // MC-04: no INDEX instrument type; the exchange marks the index.
    let nifty = r.by_symbol("NSE_INDEX", "NIFTY").unwrap();
    assert_eq!(nifty.instrument_type, "EQ");
    assert_eq!(nifty.brexchange, "NSE");
    assert_eq!(nifty.name, "NIFTY 50");
    let jr = r.by_symbol("NSE_INDEX", "NIFTYNXT50").unwrap();
    assert_eq!(jr.brsymbol, "NIFTYJR");
    let sensex = r.by_symbol("BSE_INDEX", "SENSEX").unwrap();
    assert_eq!(
        (sensex.token.as_str(), sensex.instrument_type.as_str()),
        ("1", "EQ")
    );
    assert!(r.by_symbol("NSE_INDEX", "BANKNIFTY").is_some());
}

#[test]
fn master_contract_derivative_symbols() {
    let r = master();
    let ce = r.by_symbol("NFO", "NIFTY28OCT2524500CE").unwrap();
    assert_eq!(ce.brsymbol, "NIFTY25OCT24500CE");
    assert_eq!(ce.expiry, "28-OCT-25");
    assert_eq!((ce.strike, ce.lot_size), (24500.0, 75));
    assert_eq!(ce.name, "NIFTY");
    assert_eq!(ce.instrument_type, "CE");
    let fut = r.by_symbol("NFO", "NIFTY28OCT25FUT").unwrap();
    assert_eq!(fut.brsymbol, "NIFTY25OCTFUT");
    assert_eq!(fut.tick_size, 0.1);
    assert!(r.by_symbol("NFO", "VEDL28OCT25292.5CE").is_some());
    // Broker symbol with spaces: spaces stripped (web post-step).
    let fin = r.by_symbol("NFO", "FINNIFTY28OCT2526000PE").unwrap();
    assert_eq!(fin.brsymbol, "FINNIFTY 28 OCT 25 26000 PE");
    // Missing instrument type with a strike -> OPT, symbol not rebuilt.
    let opt = r.by_symbol("NFO", "SBIN25OCT800PE").unwrap();
    assert_eq!(opt.instrument_type, "OPT");
    // MC-06/10: BSE F&O is rebuilt in OpenAlgo format too.
    let bfo = r.by_symbol("BFO", "SENSEX30OCT2582000CE").unwrap();
    assert_eq!(
        (
            bfo.brsymbol.as_str(),
            bfo.expiry.as_str(),
            bfo.name.as_str()
        ),
        ("SENSEX25OCT82000CE", "30-OCT-25", "SENSEX")
    );
    assert!(r.by_symbol("BFO", "SENSEX25OCT82000CE").is_none());
    let bfut = r.by_symbol("BFO", "SENSEX30OCT25FUT").unwrap();
    assert_eq!(bfut.brsymbol, "SENSEX25OCTFUT");
    // MC-12: NSE commodities are rebuilt, stay on NSE and carry their
    // underlying as name.
    let gold = r.by_symbol("NSE", "GOLD05NOV25FUT").unwrap();
    assert_eq!(
        (
            gold.brsymbol.as_str(),
            gold.instrument_type.as_str(),
            gold.name.as_str()
        ),
        ("GOLD25NOVFUT", "FUT", "GOLD")
    );
    // Option chains work off `name`.
    assert_eq!(r.expiries("NFO", "NIFTY", None), ["28-OCT-25"]);
}

#[test]
fn master_contract_series_sharing_a_trading_symbol_are_unique() {
    let rows = parse_instruments(fixture!("instrument.csv")).unwrap();
    // MC-03: internal_trading_symbol first, else SYMBOL-SERIES.
    let imc: Vec<(&str, &str, &str)> = rows
        .iter()
        .filter(|r| r.brsymbol.starts_with("IMC"))
        .map(|r| (r.symbol.as_str(), r.brsymbol.as_str(), r.token.as_str()))
        .collect();
    assert_eq!(
        imc,
        [
            ("IMC1-N1", "IMC1", "7001"),
            ("IMC1-N2", "IMC1", "7002"),
            ("IMC2", "IMC2", "7003")
        ]
    );
    let mut seen = std::collections::HashSet::new();
    for r in &rows {
        assert!(
            seen.insert((r.symbol.clone(), r.exchange.clone())),
            "duplicate {} {}",
            r.symbol,
            r.exchange
        );
    }
}

#[test]
fn master_contract_rejects_unknown_header() {
    let e = parse_instruments("a,b\n1,2\n").unwrap_err();
    assert!(e.client_message().contains("unexpected format"));
    assert!(parse_instruments("").is_err());
}

// ---------------------------------------------------------------------------
// Field maps
// ---------------------------------------------------------------------------

#[test]
fn outbound_maps_refuse_instead_of_defaulting() {
    assert_eq!(order_exchange("NFO").unwrap(), "NSE");
    assert_eq!(order_exchange("BFO").unwrap(), "BSE");
    assert_eq!(order_segment("NFO").unwrap(), "FNO");
    assert_eq!(order_segment("BSE").unwrap(), "CASH");
    let e = order_exchange("MCX").unwrap_err();
    assert_eq!(
        e.client_message(),
        "Groww's trading API does not support the MCX exchange. Orders can be placed on NSE, BSE, NFO and BFO only."
    );
    assert!(order_segment("CDS").is_err());
    assert!(order_exchange("NSE_INDEX").is_err());
    assert_eq!(validity(Validity::Day).unwrap(), "DAY");
    assert!(validity(Validity::Ioc)
        .unwrap_err()
        .client_message()
        .contains("DAY validity only"));
    // Market data: indices on their own exchange's CASH segment (QT-05/07).
    assert_eq!(groww_exchange("BSE_INDEX"), "BSE");
    assert_eq!(groww_segment("NSE_INDEX"), "CASH");
    assert!(check_data_exchange("BSE_INDEX").is_ok());
    assert!(check_data_exchange("MCX").is_err());
    // Annexure "Order Type": SL and SL_M.
    assert_eq!(order_type(PriceType::Sl), "SL");
    assert_eq!(order_type(PriceType::SlM), "SL_M");
    assert_eq!(product(Product::Nrml), "NRML");
}

#[test]
fn inbound_maps() {
    assert_eq!(reverse_order_type("SL_M"), "SL-M");
    assert_eq!(reverse_order_type("SL"), "SL");
    assert_eq!(reverse_order_type("LIMIT"), "LIMIT");
    assert_eq!(reverse_product("INTRADAY"), "MIS");
    assert_eq!(reverse_product("MARGIN"), "NRML");
    assert_eq!(reverse_product("CNC"), "CNC");
    for (s, want) in [
        ("NEW", "open"),
        ("ACKED", "open"),
        ("APPROVED", "open"),
        ("OPEN", "open"),
        ("MODIFICATION_REQUESTED", "open"),
        ("CANCELLATION_REQUESTED", "open"),
        ("TRIGGER_PENDING", "trigger pending"),
        ("EXECUTED", "complete"),
        ("DELIVERY_AWAITED", "complete"),
        ("COMPLETED", "complete"),
        ("CANCELLED", "cancelled"),
        ("REJECTED", "rejected"),
        ("FAILED", "rejected"),
        // Unknown statuses are shown as sent, not as "open".
        ("SOMETHING_NEW", "something_new"),
    ] {
        assert_eq!(map_status(s), want, "{}", s);
    }
    assert!(is_cancellable("modification_requested"));
    assert!(is_cancellable("TRIGGER_PENDING"));
    assert!(!is_cancellable("EXECUTED"));
    assert!(!is_cancellable("CANCELLATION_REQUESTED"));
    assert_eq!(oa_exchange("NSE", "FNO"), "NFO");
    assert_eq!(oa_exchange("BSE", "FNO"), "BFO");
    // The segment decides, never a C or P in the symbol.
    assert_eq!(oa_exchange("NSE", "CASH"), "NSE");
    assert_eq!(oa_exchange("BSE", "CASH"), "BSE");
}

#[test]
fn reference_ids_follow_web_rules() {
    assert_eq!(sanitize_reference_id("ab"), "ab000000");
    assert_eq!(sanitize_reference_id("a-b-c-d!e"), "a-b-cde0");
    assert_eq!(sanitize_reference_id(&"x".repeat(30)).len(), 20);
    let id = new_reference_id(chrono::NaiveDate::from_ymd_opt(2026, 10, 3).unwrap());
    assert!(id.starts_with("20261003-"));
    assert_eq!(id.len(), 17);
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[test]
fn order_book_is_normalised_to_openalgo() {
    let r = master();
    let mut raw = orders_of(&fx(fixture!("order_list_cash.json")));
    raw.extend(orders_of(&fx(fixture!("order_list_fno.json"))));
    let o = map_orders(&raw, &r);
    assert_eq!(o.len(), 6);
    assert_eq!(
        (
            o[0].symbol.as_str(),
            o[0].exchange.as_str(),
            o[0].status.as_str()
        ),
        ("SBIN", "NSE", "complete")
    );
    assert_eq!((o[0].filled_quantity, o[0].average_price), (10, 812.35));
    assert_eq!(o[0].order_type, "MARKET");
    // Not in the master: Groww symbol kept; ITC stays on NSE.
    assert_eq!(
        (o[1].symbol.as_str(), o[1].exchange.as_str()),
        ("ITC", "NSE")
    );
    assert_eq!((o[1].product.as_str(), o[1].pending_quantity), ("MIS", 5));
    let rej = &o[2];
    assert_eq!(
        (
            rej.exchange.as_str(),
            rej.status.as_str(),
            rej.order_type.as_str()
        ),
        ("BSE", "rejected", "SL")
    );
    assert_eq!(rej.rejection_reason.as_deref(), Some("Insufficient funds"));
    assert_eq!((rej.quantity, rej.trigger_price), (3, 1395.0));
    // A trigger-pending stop-loss is shown as open so it can be cancelled.
    assert_eq!(o[3].status, "open");
    assert_eq!(o[3].order_type, "SL-M");
    assert_eq!(o[3].pending_quantity, 2);
    assert_eq!(
        (o[4].symbol.as_str(), o[4].exchange.as_str()),
        ("NIFTY28OCT2524500CE", "NFO")
    );
    assert_eq!(
        (
            o[5].symbol.as_str(),
            o[5].exchange.as_str(),
            o[5].product.as_str()
        ),
        ("SENSEX30OCT2582000CE", "BFO", "NRML")
    );
    // Statistics read the mapped status: trigger pending counts as open.
    let s = order_stats(&raw);
    assert_eq!(
        (
            s.total_buy_orders,
            s.total_sell_orders,
            s.total_completed_orders,
            s.total_open_orders,
            s.total_rejected_orders
        ),
        (3, 3, 2, 2, 1)
    );
}

#[test]
fn trades_map_from_their_own_exchange_and_segment() {
    let r = master();
    let trades: Vec<GrowwTrade> =
        serde_json::from_value(fx(fixture!("trades.json"))["payload"]["trade_list"].clone())
            .unwrap();
    let t = map_trade(&trades[0], "GMK39038RDT490CCVRO", "CASH", &r);
    assert_eq!((t.symbol.as_str(), t.exchange.as_str()), ("SBIN", "NSE"));
    // Rupees, never divided (web test_groww_tradebook_price).
    assert_eq!((t.quantity, t.average_price), (6, 812.3));
    assert!((t.trade_value - 4873.8).abs() < 1e-9);
    assert_eq!(
        (t.trade_id.as_str(), t.product.as_str()),
        ("GMKT1001", "CNC")
    );
    assert_eq!(t.timestamp, "2025-10-06T09:20:12");
    // A trade without its own segment takes the segment it was read in.
    let fno = GrowwTrade {
        trading_symbol: "SENSEX25OCT82000CE".into(),
        exchange: "BSE".into(),
        quantity: 20,
        price: 150.0,
        ..Default::default()
    };
    let t = map_trade(&fno, "GLTFO1", "FNO", &r);
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.order_id.as_str()),
        ("SENSEX30OCT2582000CE", "BFO", "GLTFO1")
    );
}

#[test]
fn positions_use_groww_documented_fields() {
    let r = master();
    let cash: Vec<GrowwPosition> =
        serde_json::from_value(fx(fixture!("positions_cash.json"))["payload"]["positions"].clone())
            .unwrap();
    let p = map_position(&cash[0], "CASH", &r);
    assert_eq!((p.symbol.as_str(), p.exchange.as_str()), ("SBIN", "NSE"));
    assert_eq!((p.quantity, p.buy_quantity, p.sell_quantity), (15, 15, 0));
    assert_eq!(p.average_price, 808.23);
    assert!((p.buy_value - 812.35 * 15.0).abs() < 1e-6);
    // No net quantity: buy - sell; prices are rupees as Groww sends them;
    // P&L starts as realised_pnl.
    let q = map_position(&cash[1], "CASH", &r);
    assert_eq!((q.exchange.as_str(), q.quantity), ("BSE", 0));
    assert_eq!(q.average_price, 1405.0);
    assert_eq!(q.product, "MIS");
    assert_eq!((q.pnl, q.realized_pnl, q.ltp), (120.5, 120.5, 0.0));
    let fno: Vec<GrowwPosition> =
        serde_json::from_value(fx(fixture!("positions_fno.json"))["payload"]["positions"].clone())
            .unwrap();
    let mut f = map_position(&fno[0], "FNO", &r);
    assert_eq!(
        (f.symbol.as_str(), f.exchange.as_str(), f.quantity),
        ("NIFTY28OCT2524500CE", "NFO", 75)
    );
    // Rupees as sent: 1400 bought and 1410 sold per unit.
    assert!((q.buy_value - 1400.0 * q.buy_quantity as f64).abs() < 1e-6);
    assert!((q.sell_value - 1410.0 * q.sell_quantity as f64).abs() < 1e-6);
    // LTP adds the open quantity's move from the average.
    attach_ltp(&mut f, Some(120.4));
    assert_eq!(f.ltp, 120.4);
    assert!((f.unrealized_pnl - 8.0 * 75.0).abs() < 1e-6);
    assert!((f.pnl - 600.0).abs() < 1e-6);
    // No price: the row stays with P&L as realised.
    let mut g = map_position(&cash[0], "CASH", &r);
    attach_ltp(&mut g, None);
    assert_eq!((g.ltp, g.pnl), (0.0, 0.0));
    assert!(says_no_positions("No positions found for user"));
    assert!(says_no_positions("Data not found"));
    assert!(!says_no_positions("Internal error"));
    // Only the web's empty-book phrases count: a refusal naming something
    // else missing is a failed read, never a flat book for a smart order.
    assert!(!says_no_positions("Instrument not found"));
    assert!(!says_no_positions("User not found"));
    assert!(!says_no_positions(&format!("no data {}", "x".repeat(2000))));
}

#[test]
fn holdings_resolve_exchange_and_price_from_ltp() {
    let r = master();
    let rows: Vec<GrowwHolding> =
        serde_json::from_value(fx(fixture!("holdings.json"))["payload"]["holdings"].clone())
            .unwrap();
    let a = map_holding(&rows[0], &r, Some(820.0));
    assert_eq!(
        (
            a.symbol.as_str(),
            a.exchange.as_str(),
            a.quantity,
            a.t1_quantity
        ),
        ("SBIN", "NSE", 20, 2)
    );
    assert_eq!(a.isin.as_deref(), Some("INE062A01020"));
    assert_eq!((a.ltp, a.pnl), (820.0, 3390.0));
    assert_eq!(a.pnl_percentage, 26.06);
    // Unpriced: no P&L rather than a made-up one; valued at the average.
    let b = map_holding(&rows[1], &r, None);
    assert_eq!((b.quantity, b.average_price), (100, 245.1));
    assert_eq!((b.ltp, b.pnl, b.pnl_percentage), (0.0, 0.0, 0.0));
    assert_eq!(b.product, "CNC");
    let s = holdings_stats(&[a, b]);
    assert_eq!(s.totalinvvalue, 37520.0);
    assert_eq!(s.totalholdingvalue, 40910.0);
    assert_eq!(s.totalprofitandloss, 3390.0);
    // A symbol only on BSE resolves to BSE; one on neither has no exchange.
    let bse_only = GrowwHolding {
        trading_symbol: "RELIANCE".into(),
        ..Default::default()
    };
    // RELIANCE is on NSE and BSE: NSE first.
    assert_eq!(map_holding(&bse_only, &r, None).exchange, "NSE");
    let unknown = GrowwHolding {
        trading_symbol: "ZZZ".into(),
        ..Default::default()
    };
    let u = map_holding(&unknown, &r, None);
    assert_eq!((u.symbol.as_str(), u.exchange.as_str()), ("ZZZ", ""));
}

#[test]
fn funds_and_margin_parse() {
    let f = funds_from_payload(&fx(fixture!("funds.json"))["payload"], 120.5, 600.0);
    assert_eq!(f.available_cash, 125000.5);
    assert_eq!(f.collateral, 15000.0);
    assert_eq!(f.utilised_debits, 23500.25);
    assert_eq!((f.m2m_realized, f.m2m_unrealized), (120.5, 600.0));
    let m = parse_margin(&fx(fixture!("margin.json"))["payload"]);
    assert_eq!(
        (m.total_margin_required, m.span_margin, m.exposure_margin),
        (121000.75, 98000.25, 23000.5)
    );
    let cash: Vec<GrowwPosition> =
        serde_json::from_value(fx(fixture!("positions_cash.json"))["payload"]["positions"].clone())
            .unwrap();
    let mut p = map_position(&cash[0], "CASH", &master());
    p.realized_pnl = 5.0;
    p.unrealized_pnl = 7.0;
    assert_eq!(day_m2m(std::slice::from_ref(&p)), (5.0, 7.0));
    p.ltp = 1.0;
    assert_eq!(day_m2m(&[p]), (5.0, 7.0));
}

fn leg(symbol: &str, exchange: &str, price: f64) -> MarginLeg {
    MarginLeg {
        key: QuoteKey::new(exchange, symbol),
        action: Action::Buy,
        quantity: 75,
        product: Product::Nrml,
        pricetype: PriceType::Market,
        price,
        trigger_price: 0.0,
    }
}

#[test]
fn margin_items_carry_segment_and_group_per_segment() {
    let c = core();
    let groups = margin_groups(
        &c,
        &[
            leg("NIFTY28OCT2524500CE", "NFO", 0.0),
            leg("SBIN", "NSE", 0.0),
            leg("SENSEX30OCT2582000CE", "BFO", 150.0),
            leg("RELIANCE", "NSE", 0.0),
        ],
    )
    .unwrap();
    let reqs = margin_requests(groups);
    // FNO as one basket; each CASH order on its own (no CASH basket).
    let shape: Vec<(&str, usize)> = reqs.iter().map(|(s, v)| (*s, v.len())).collect();
    assert_eq!(shape, [("FNO", 2), ("CASH", 1), ("CASH", 1)]);
    let fno = &reqs[0].1;
    assert_eq!(fno[0]["trading_symbol"], "NIFTY25OCT24500CE");
    assert_eq!(fno[0]["exchange"], "NSE");
    assert_eq!(fno[0]["segment"], "FNO");
    assert_eq!(fno[0]["order_type"], "MARKET");
    assert!(fno[0].get("price").is_none());
    assert_eq!(fno[1]["exchange"], "BSE");
    assert_eq!(fno[1]["price"], 150.0);
    assert_eq!(reqs[1].1[0]["segment"], "CASH");
    // A position that cannot be sent refuses the request, naming it.
    let e = margin_groups(&c, &[leg("UNKNOWN", "NSE", 0.0)]).unwrap_err();
    assert!(e.client_message().contains("UNKNOWN"));
    let e = margin_groups(&c, &[leg("CRUDEOIL", "MCX", 0.0)]).unwrap_err();
    assert!(e.client_message().contains("MCX"));
}

// ---------------------------------------------------------------------------
// Order payloads
// ---------------------------------------------------------------------------

fn resolved(symbol: &str, exchange: &str, pricetype: &str, price: f64, trig: f64) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: 75,
        price,
        order_type: pricetype.into(),
        product: "NRML".into(),
        validity: "DAY".into(),
        trigger_price: Some(trig),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[test]
fn place_body_matches_web_payload() {
    let o = resolved("NIFTY28OCT2524500CE", "NFO", "LIMIT", 110.5, 0.0);
    let b = place_order_body(&o, "20261003-1a2b3c4d").unwrap();
    assert_eq!(
        b,
        json!({
            "trading_symbol": "NIFTY25OCT24500CE",
            "quantity": 75,
            "validity": "DAY",
            "exchange": "NSE",
            "segment": "FNO",
            "product": "NRML",
            "order_type": "LIMIT",
            "transaction_type": "BUY",
            "order_reference_id": "20261003-1a2b3c4d",
            "price": 110.5
        })
    );
    let m = place_order_body(&resolved("SBIN", "NSE", "MARKET", 0.0, 0.0), "x0000000").unwrap();
    assert!(m.get("price").is_none() && m.get("trigger_price").is_none());
    let slm = place_order_body(&resolved("SBIN", "NSE", "SL-M", 0.0, 790.0), "x0000000").unwrap();
    assert_eq!(slm["order_type"], "SL_M");
    assert_eq!(slm["trigger_price"], 790.0);
    assert!(slm.get("price").is_none());
    // SL is a stop-limit order: it carries its limit price.
    let sl = place_order_body(&resolved("SBIN", "NSE", "SL", 791.0, 790.0), "x0000000").unwrap();
    assert_eq!(
        (sl["order_type"].as_str(), sl["price"].as_f64()),
        (Some("SL"), Some(791.0))
    );
}

#[test]
fn unsupported_order_inputs_are_refused_before_sending() {
    let c = core();
    assert!(validate(&c, &resolved("SBIN", "NSE", "MARKET", 0.0, 0.0)).is_ok());
    let mut ioc = resolved("SBIN", "NSE", "MARKET", 0.0, 0.0);
    ioc.validity = Validity::Ioc;
    assert!(validate(&c, &ioc)
        .unwrap_err()
        .client_message()
        .contains("DAY validity only"));
    let idx = resolved("NIFTY", "NSE_INDEX", "MARKET", 0.0, 0.0);
    assert!(validate(&c, &idx)
        .unwrap_err()
        .client_message()
        .contains("does not support the NSE_INDEX exchange"));
    // An NSE bond whose Groww trading symbol is shared by several series.
    let bond = resolved("IMC1-N2", "NSE", "MARKET", 0.0, 0.0);
    let e = validate(&c, &bond).unwrap_err().client_message();
    assert!(e.contains("IMC1") && e.contains("series"), "{e}");
    assert!(validate(&c, &resolved("IMC2", "NSE", "MARKET", 0.0, 0.0)).is_ok());
}

#[test]
fn place_replies_report_failures_as_errors() {
    let reply = |status: u16, body: Value| Reply {
        status: reqwest::StatusCode::from_u16(status).unwrap(),
        body,
    };
    let ok = place_outcome(&reply(
        200,
        json!({"status": "SUCCESS", "payload": {"groww_order_id": "GMK1", "order_status": "OPEN"}}),
    ))
    .unwrap();
    assert_eq!(ok.order_id, "GMK1");
    // Accepted by the API but failed straight away: Groww's remark.
    let e = place_outcome(&reply(
        200,
        json!({"status": "SUCCESS", "payload": {"groww_order_id": "GMK2", "order_status": "FAILED",
               "remark": "Retry within the allowed price range for stop-loss trigger."}}),
    ))
    .unwrap_err();
    assert_eq!(
        e.client_message(),
        "Retry within the allowed price range for stop-loss trigger."
    );
    // Never success without an order id.
    let e = place_outcome(&reply(200, json!({"status": "SUCCESS", "payload": {}}))).unwrap_err();
    assert!(e.client_message().contains("did not return an order ID"));
    // HTTP 200 + FAILURE is an error with Groww's reason.
    let e = place_outcome(&reply(
        200,
        json!({"status": "FAILURE", "error": {"code": "GA001", "message": "Insufficient margin"}}),
    ))
    .unwrap_err();
    assert_eq!(e.client_message(), "Insufficient margin");
    // A server error does not say the order was not taken: no blind retry.
    let e = place_outcome(&reply(503, Value::Null)).unwrap_err();
    assert_eq!(
        e.client_message(),
        "Groww did not confirm the order. Check the order book before placing it again."
    );
    // A 403 with its own reason keeps it (only the session's 401/403 is
    // "session expired").
    let e = place_outcome(&reply(
        403,
        json!({"status": "FAILURE", "error": {"message": "Order not allowed from this IP"}}),
    ))
    .unwrap_err();
    assert_eq!(e.client_message(), "Order not allowed from this IP");
}

#[test]
fn modify_body_and_types() {
    let m = ResolvedModify::resolve(
        "GLTFO1",
        &ModifyOrderRequest {
            symbol: "NIFTY28OCT2524500CE".into(),
            exchange: "NFO".into(),
            action: "BUY".into(),
            product: "NRML".into(),
            pricetype: "SL".into(),
            quantity: 150,
            price: 101.0,
            trigger_price: 100.0,
            disclosed_quantity: 0,
        },
        &master(),
    )
    .unwrap();
    assert_eq!(
        modify_order_body(&m).unwrap(),
        json!({"groww_order_id": "GLTFO1", "order_type": "SL", "segment": "FNO",
               "quantity": 150, "price": 101.0, "trigger_price": 100.0})
    );
    let mut z = m.clone();
    z.quantity = 0;
    assert!(modify_order_body(&z).is_err());
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[test]
fn quote_from_string_ohlc_and_aliases() {
    let k = QuoteKey::new("NSE", "SBIN");
    let q = to_quote(&k, &fx(fixture!("quote_cash.json"))["payload"]);
    assert_eq!(
        (q.ltp, q.open, q.high, q.low, q.close),
        (812.35, 809.0, 815.5, 806.25, 808.0)
    );
    assert_eq!(
        (q.bid, q.ask, q.bid_qty, q.ask_qty),
        (812.3, 812.4, 120, 80)
    );
    assert_eq!((q.volume, q.oi), (5123400, 0));
    assert_eq!((q.change, q.change_percent), (4.35, 0.54));
    let d = to_depth(&k, &fx(fixture!("quote_cash.json"))["payload"]);
    assert_eq!((d.bids.len(), d.asks.len()), (5, 5));
    assert_eq!((d.bids[2].price, d.bids[2].quantity), (812.2, 45));
    assert_eq!(d.asks[4], DepthLevel::default());
    assert_eq!((d.ltq, d.prev_close, d.total_buy_qty), (7, 808.0, 245000));
}

#[test]
fn fno_quote_falls_back_to_top_of_book() {
    let k = QuoteKey::new("NFO", "NIFTY28OCT2524500CE");
    let p = fx(fixture!("quote_fno.json"));
    let q = to_quote(&k, &p["payload"]);
    assert_eq!(
        (q.bid, q.bid_qty, q.ask, q.ask_qty),
        (112.35, 1500, 112.5, 2250)
    );
    assert_eq!((q.oi, q.volume, q.close), (4567800, 12345675, 101.2));
    let d = to_depth(&k, &p["payload"]);
    assert_eq!(d.oi, 4567800);
}

#[test]
fn multiquote_rows_take_ltp_from_the_ltp_endpoint() {
    let p = fx(fixture!("ohlc.json"));
    let k = QuoteKey::new("NSE", "SBIN");
    // ohlc.close is the previous close; the live price is the LTP.
    let q = quote_from_ohlc(&k, p["payload"].get("NSE_SBIN"), 815.0);
    assert_eq!(
        (q.open, q.high, q.low, q.ltp, q.close),
        (809.0, 815.5, 806.25, 815.0, 812.35)
    );
    let r = quote_from_ohlc(&k, p["payload"].get("BSE_RELIANCE"), 1406.0);
    assert_eq!((r.ltp, r.close), (1406.0, 1405.1));
    // A priced symbol whose OHLC entry is a bare number keeps its price
    // (web test_multiquote_keeps_a_priced_symbol_whose_ohlc_is_a_bare_number).
    let n = quote_from_ohlc(&k, p["payload"].get("NSE_NIFTY"), 101.5);
    assert_eq!((n.ltp, n.open, n.close), (101.5, 0.0, 0.0));
    let none = quote_from_ohlc(&k, None, 7.0);
    assert_eq!((none.ltp, none.open), (7.0, 0.0));
    assert_eq!(
        invalid_symbol(r#"{"error":{"message":"Invalid trading symbol: FOO-BE in request"}}"#)
            .as_deref(),
        Some("FOO-BE")
    );
    assert_eq!(invalid_symbol("other"), None);
    let c = core();
    assert_eq!(
        exchange_symbol(&c, &QuoteKey::new("BFO", "SENSEX30OCT2582000CE")),
        "BSE_SENSEX25OCT82000CE"
    );
    assert_eq!(
        exchange_symbol(&c, &QuoteKey::new("NSE_INDEX", "NIFTYNXT50")),
        "NSE_NIFTYJR"
    );
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

#[test]
fn history_intervals_symbols_and_chunks() {
    assert_eq!(candle_interval("2m").unwrap(), "2minute");
    assert_eq!(candle_interval("4h").unwrap(), "4hour");
    assert_eq!(candle_interval("W").unwrap(), "1week");
    assert!(candle_interval("M").is_err());
    assert_eq!(
        ["1minute", "5minute", "10minute", "30minute", "1hour", "4hour", "1day", "1week"]
            .map(max_days),
        [30, 30, 90, 90, 180, 180, 1080, 3650]
    );
    let d = |m, dd| chrono::NaiveDate::from_ymd_opt(2025, m, dd).unwrap();
    assert_eq!(
        date_chunks(d(1, 1), d(3, 5), 30),
        [(d(1, 1), d(1, 30)), (d(1, 31), d(3, 1)), (d(3, 2), d(3, 5))]
    );
    assert_eq!(date_chunks(d(3, 5), d(3, 5), 30), [(d(3, 5), d(3, 5))]);
    // groww_symbol from the master contract (backtesting "Groww Symbol").
    let c = core();
    let t = |s: &str, e: &str| history_target(&c, &QuoteKey::new(e, s)).unwrap();
    assert_eq!(
        t("SBIN", "NSE"),
        ("NSE".into(), "CASH", "NSE-SBIN".into(), "SBIN".into())
    );
    assert_eq!(t("NIFTYNXT50", "NSE_INDEX").2, "NSE-NIFTYJR");
    assert_eq!(t("SENSEX", "BSE_INDEX").2, "BSE-SENSEX");
    assert_eq!(
        t("NIFTY28OCT2524500CE", "NFO"),
        (
            "NSE".into(),
            "FNO",
            "NSE-NIFTY-28Oct25-24500-CE".into(),
            "NIFTY25OCT24500CE".into()
        )
    );
    assert_eq!(t("NIFTY28OCT25FUT", "NFO").2, "NSE-NIFTY-28Oct25-FUT");
    assert_eq!(
        t("VEDL28OCT25292.5CE", "NFO").2,
        "NSE-VEDL-28Oct25-292.5-CE"
    );
    assert_eq!(
        t("SENSEX30OCT2582000CE", "BFO").2,
        "BSE-SENSEX-30Oct25-82000-CE"
    );
    assert!(history_target(&c, &QuoteKey::new("NSE", "NOPE")).is_err());
    assert_eq!(fno_underlying("NIFTY28OCT2524500CE"), Some("NIFTY"));
    assert_eq!(fno_underlying("M&M28OCT25FUT"), Some("M&M"));
    assert_eq!(fno_underlying("SBIN"), None);
}

#[test]
fn intraday_history_drops_pre_open_and_keeps_volume_numeric() {
    let rows = fx(fixture!("history_intraday.json"))["payload"]["candles"]
        .as_array()
        .unwrap()
        .clone();
    let c = to_candles(session_candles(&rows, 5));
    let c = crate::brokers::common::history::sort_dedupe(c);
    let ts: Vec<i64> = c.iter().map(|c| c.timestamp).collect();
    // 09:00 and 09:10 lie wholly in the pre-open, 09:30 has no open; the
    // day starts at 09:15 IST (1759722300); duplicates dropped.
    assert_eq!(ts, [1759722300, 1759722600, 1759722900, 1759744500]);
    // Volume is per candle; a null volume reads as 0.
    assert_eq!(
        c.iter().map(|c| c.volume).collect::<Vec<_>>(),
        [98000, 120000, 0, 4000]
    );
    assert!(c.iter().all(|c| c.oi == 0));
}

#[test]
fn thirty_minute_and_hourly_are_built_from_15m_aligned_to_0915() {
    let rows: Vec<Value> = [
        json!(["2025-10-06T09:00:00", null, 801.0, 799.0, 800.0, 900]),
        json!(["2025-10-06T09:15:00", 800.0, 805.0, 798.0, 804.0, 100]),
        json!(["2025-10-06T09:30:00", 804.0, 810.0, 803.0, 809.0, 200]),
        json!(["2025-10-06T09:45:00", 809.0, 812.0, 806.0, 807.0, null]),
        json!(["2025-10-06T10:00:00", 807.0, 808.0, 801.0, 802.0, 400]),
    ]
    .to_vec();
    let s = session_candles(&rows, 15);
    assert_eq!(s.len(), 4, "the 09:00 pre-open candle is left out");
    let half = to_candles(rebucket(s.clone(), 30));
    assert_eq!(
        half.iter()
            .map(|c| (c.timestamp, c.open, c.high, c.low, c.close, c.volume))
            .collect::<Vec<_>>(),
        [
            (1759722300, 800.0, 810.0, 798.0, 809.0, 300),
            (1759724100, 809.0, 812.0, 801.0, 802.0, 400)
        ]
    );
    let hour = to_candles(rebucket(s, 60));
    assert_eq!(hour.len(), 1);
    assert_eq!(
        (
            hour[0].timestamp,
            hour[0].open,
            hour[0].high,
            hour[0].low,
            hour[0].close,
            hour[0].volume
        ),
        (1759722300, 800.0, 812.0, 798.0, 802.0, 700)
    );
}

#[test]
fn eod_candles_are_stamped_at_midnight_utc_of_the_ist_date() {
    let rows = fx(fixture!("history_daily.json"));
    let d: Vec<Candle> = rows["payload"]["candles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(eod_candle)
        .collect();
    let ts: Vec<i64> = d.iter().map(|c| c.timestamp).collect();
    assert_eq!(ts, [1759708800, 1759795200, 1759881600, 1760313600]);
    assert_eq!(
        (d[0].open, d[0].close, d[0].volume),
        (800.0, 812.0, 5_000_000)
    );
    // Milliseconds read as seconds.
    let ms = eod_candle(&json!([1759689000000_i64, 1.0, 2.0, 0.5, 1.5, 10])).unwrap();
    assert_eq!(ms.timestamp, 1759708800);
    // A missing price leaves the candle out rather than drawing it at 0; a
    // missing volume reads as 0.
    assert!(eod_candle(&json!([1759689000, null, 2.0, 0.5, 1.5, 10])).is_none());
    assert!(eod_candle(&json!([1759689000, 1.0, 2.0, 0.5, null, 10])).is_none());
    let nv = eod_candle(&json!([1759689000, 1.0, 2.0, 0.5, 1.5, null])).unwrap();
    assert_eq!((nv.close, nv.volume), (1.5, 0));
    // A symbol with non-ASCII text after its letters is left as is, never
    // sliced inside a character.
    assert_eq!(
        derivative_symbol_fallback("SBIN\u{20ac}30SEP25FUT"),
        "SBIN\u{20ac}30SEP25FUT"
    );
}

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

/// Web test_groww_review_fixes.py: login advice matches why Groww refused.
#[test]
fn login_advice_matches_why_groww_refused() {
    let refused = json!({"status": "FAILURE", "error": {"code": "GA003", "message": "Unable to serve request currently"}});
    for (status, expect, avoid) in [
        (429, "Wait a minute", "API key and secret"),
        (503, "not the problem", "API key and secret"),
        (401, "API key and secret", "Wait a minute"),
    ] {
        let e = login_error(&refused, Some(status)).client_message();
        assert!(e.contains(expect) && !e.contains(avoid), "{status}: {e}");
        assert!(e.contains("Unable to serve request currently"));
    }
    assert_eq!(login_error(&refused, Some(401)).code(), "AUTH_ERROR");
}

#[test]
fn inactive_or_expired_tokens_are_refused() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-08T04:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(
        token_from_response(
            &json!({"token": "t", "isActive": true, "expiry": "2026-10-09T06:00:00"}),
            now
        )
        .unwrap(),
        "t"
    );
    let e = token_from_response(&json!({"token": "t", "isActive": false}), now).unwrap_err();
    assert!(e.client_message().contains("not active"));
    // A naive expiry is IST: 09:00 IST on 8 Oct is 03:30 UTC, already past.
    let e = token_from_response(&json!({"token": "t", "expiry": "2026-10-08T09:00:00"}), now)
        .unwrap_err();
    assert!(e.client_message().contains("expired"));
    assert!(token_from_response(
        &json!({"token": "t", "expiry": "2026-10-08T10:00:00+05:30"}),
        now
    )
    .is_ok());
    assert!(token_from_response(&json!({"tokenRefId": "r"}), now).is_err());
}

#[test]
fn checksum_and_variants() {
    // sha256("secret" + "1700000000")
    let mut h = sha2::Sha256::new();
    sha2::Digest::update(&mut h, b"secret1700000000");
    assert_eq!(
        checksum("secret", "1700000000"),
        hex::encode(sha2::Digest::finalize(h))
    );
    assert!(looks_like_jwt("eyJa.b.c"));
    assert!(!looks_like_jwt("abc.def"));
    let base = BrokerCredentials {
        api_key: "KEY".into(),
        ..Default::default()
    };
    let with = |f: &dyn Fn(&mut BrokerCredentials)| {
        let mut c = base.clone();
        f(&mut c);
        choose_variant(&c)
    };
    assert!(matches!(
        with(&|c| c.totp = Some("123456".into())).unwrap(),
        Variant::Totp { .. }
    ));
    assert_eq!(
        with(&|c| c.password = Some(" tok ".into())).unwrap(),
        Variant::PastedToken("tok".into())
    );
    assert!(matches!(
        with(&|c| c.api_secret = Some("s".into())).unwrap(),
        Variant::Approval { .. }
    ));
    assert_eq!(
        with(&|c| c.api_key = "eyJh.x.y".into()).unwrap(),
        Variant::PastedToken("eyJh.x.y".into())
    );
    assert!(with(&|_| {}).is_err());
}

use sha2::Digest as _;

// ---------------------------------------------------------------------------
// nkeys, NATS and protobuf
// ---------------------------------------------------------------------------

#[test]
fn nkeys_encode_and_sign() {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    assert_eq!(nkeys::crc16(b"123456789"), 0x31C3);
    let kp = KeyPair::from_seed(&[7u8; 32]);
    let public = kp.public_key();
    assert!(public.starts_with('U'));
    assert_eq!(public.len(), 56);
    let seed = kp.seed();
    assert!(seed.starts_with("SU"));
    assert!(nkeys::decode(&seed).is_some());
    let mut broken = public.clone().into_bytes();
    broken[10] = if broken[10] == b'A' { b'B' } else { b'A' };
    assert!(nkeys::decode(std::str::from_utf8(&broken).unwrap()).is_none());
    let pk = nkeys::public_key_bytes(&public).unwrap();
    let sig_b64 = kp.sign_nonce("abc-nonce");
    let sig = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, sig_b64).unwrap();
    let vk = VerifyingKey::from_bytes(&pk).unwrap();
    let sig = Signature::from_slice(&sig).unwrap();
    assert!(vk.verify(b"abc-nonce", &sig).is_ok());
    assert!(!format!("{:?}", kp).contains(seed.as_str()));
    assert_ne!(KeyPair::generate().public_key(), public);
}

#[test]
fn nats_ops_parse_including_split_frames() {
    assert_eq!(next_op(b"PING\r\n"), Some((Op::Ping, 6)));
    assert_eq!(next_op(b"+OK\r\nPONG\r\n"), Some((Op::Ok, 5)));
    let (op, _) = next_op(b"-ERR 'Authorization Violation'\r\n").unwrap();
    assert_eq!(op, Op::Err("Authorization Violation".into()));
    let (op, _) = next_op(b"INFO {\"server_id\":\"x\",\"nonce\":\"n1\"}\r\n").unwrap();
    assert!(matches!(op, Op::Info(v) if v["nonce"] == "n1"));
    let msg = b"MSG /ld/eq/nse/price.3045 7 5\r\nhello\r\n";
    // Partial: wait for more bytes.
    assert_eq!(next_op(&msg[..20]), None);
    assert_eq!(next_op(&msg[..msg.len() - 1]), None);
    let (op, n) = next_op(msg).unwrap();
    assert_eq!(n, msg.len());
    assert_eq!(
        op,
        Op::Msg {
            subject: "/ld/eq/nse/price.3045".into(),
            sid: 7,
            payload: b"hello".to_vec()
        }
    );
    let with_reply = b"MSG subj 3 inbox.1 2\r\nab\r\n";
    assert!(matches!(
        next_op(with_reply).unwrap().0,
        Op::Msg { sid: 3, .. }
    ));
    let hmsg = b"HMSG subj 4 12 14\r\nNATS/1.0\r\n\r\nxy\r\n";
    match next_op(hmsg).unwrap().0 {
        Op::Msg { payload, sid, .. } => assert_eq!((payload, sid), (b"xy".to_vec(), 4)),
        o => panic!("{:?}", o),
    }
}

#[test]
fn connect_frame_matches_web() {
    let f = connect_frame("jwt1", Some("UKEY"), Some("SIG"));
    assert!(f.starts_with("CONNECT {") && f.ends_with("}\r\n"));
    let v: Value = serde_json::from_str(&f[8..f.len() - 2]).unwrap();
    assert_eq!(v["jwt"], "jwt1");
    assert_eq!(
        (v["nkey"].as_str(), v["sig"].as_str()),
        (Some("UKEY"), Some("SIG"))
    );
    assert_eq!(v["name"], "nats.py");
    assert_eq!(v["verbose"], false);
    let plain = connect_frame("jwt1", None, None);
    assert!(!plain.contains("nkey") && !plain.contains("sig"));
}

fn text(m: &Message) -> String {
    match m {
        Message::Text(t) => t.clone(),
        Message::Binary(b) => String::from_utf8_lossy(b).to_string(),
        other => format!("{:?}", other),
    }
}

fn replies(ev: &[FeedEvent]) -> Vec<String> {
    ev.iter()
        .filter_map(|e| match e {
            FeedEvent::Reply(m) => Some(text(m)),
            _ => None,
        })
        .collect()
}

#[test]
fn nats_handshake_replies_through_the_feed() {
    let kp = KeyPair::from_seed(&[9u8; 32]);
    let public = kp.public_key();
    let mut f = GrowwFeed::new(
        crate::brokers::common::http::client(),
        "tok",
        FeedEndpoints::default(),
    );
    f.set_minted("JWT", Some(kp));
    assert!(f.awaits_auth_ack());
    assert_eq!(f.auth_ack_timeout(), Some(ASSUME_READY_AFTER));
    f.on_connected();
    // INFO -> CONNECT (signed over the nonce) and PING, sent by the manager.
    let ev = f.parse(&Message::Text("INFO {\"nonce\":\"N0\"}\r\n".into()));
    let ups = replies(&ev);
    assert!(ups[0].starts_with("CONNECT "));
    assert!(ups[0].contains(&public));
    assert!(ups[0].contains("\"sig\""));
    assert!(ups[0].contains("\"jwt\":\"JWT\""));
    assert_eq!(ups[1], "PING\r\n");
    assert!(!ev.contains(&FeedEvent::AuthOk));
    // The PONG after CONNECT accepts the session; a server PING gets PONG.
    let ev = f.parse(&Message::Text("PONG\r\nPING\r\n".into()));
    assert!(ev.contains(&FeedEvent::AuthOk));
    assert!(ev.contains(&FeedEvent::Heartbeat));
    assert_eq!(replies(&ev), vec!["PONG\r\n".to_string()]);
    // An op split across two frames is handled once, whole.
    f.subscribe_frames(&[sub("SBIN", "NSE", "3045", FeedMode::Ltp)]);
    let whole = msg_frame(1, &ltp_payload(812.35));
    let Message::Binary(bytes) = whole else {
        panic!()
    };
    let (a, b) = bytes.split_at(10);
    assert!(f.parse(&Message::Binary(a.to_vec())).is_empty());
    let ev = f.parse(&Message::Binary(b.to_vec()));
    assert!(matches!(&ev[0], FeedEvent::Tick(t) if t.ltp == 812.35));
    let ev = f.parse(&Message::Text("-ERR 'Authorization Violation'\r\n".into()));
    assert!(matches!(ev[0], FeedEvent::AuthFailed(_)));
    assert_eq!(f.heartbeat().map(|(d, _)| d), Some(NATS_PING_EVERY));
    // A server that does not echo the subprotocol: retried once without it.
    assert!(f.on_connect_failed("Protocol error: SubProtocol error"));
    assert!(!f.on_connect_failed("Protocol error: SubProtocol error"));
}

fn sub(symbol: &str, exchange: &str, token: &str, mode: FeedMode) -> FeedSubscription {
    FeedSubscription {
        symbol: symbol.into(),
        exchange: exchange.into(),
        token: token.into(),
        brsymbol: symbol.into(),
        brexchange: "NSE".into(),
        mode,
        depth: 5,
    }
}

#[test]
fn subjects_follow_web_topics() {
    assert_eq!(
        subjects(&sub("SBIN", "NSE", "3045", FeedMode::Ltp)),
        ["/ld/eq/nse/price.3045"]
    );
    assert_eq!(
        subjects(&sub("X", "NFO", "35001", FeedMode::Depth)),
        ["/ld/fo/nse/price.35001", "/ld/fo/nse/book.35001"]
    );
    assert_eq!(
        subjects(&sub("SENSEX25OCT82000CE", "BFO", "825001", FeedMode::Quote)),
        ["/ld/fo/bse/price.825001"]
    );
    // NSE index: symbol as token; depth falls back to price.
    assert_eq!(
        subjects(&sub("NIFTY", "NSE_INDEX", "NIFTY", FeedMode::Depth)),
        ["/ld/eq/nse/price.NIFTY"]
    );
    assert_eq!(
        subjects(&sub("SENSEX", "BSE_INDEX", "1", FeedMode::Ltp)),
        ["/ld/eq/bse/price.1"]
    );
}

fn ltp_payload(ltp: f64) -> Vec<u8> {
    proto::LiveData {
        symbol: "SBIN".into(),
        segment: 0,
        exchange: 1,
        ltp_data: Some(proto::StocksLivePrice {
            ts_in_millis: 1759722011000.0,
            open: 809.0,
            high: 815.5,
            low: 806.25,
            close: 808.0,
            volume: 5_123_400.0,
            value: 0.0,
            ltp,
        }),
        ..Default::default()
    }
    .encode_to_vec()
}

fn depth_payload() -> Vec<u8> {
    let lvl = |o, p, q| proto::DepthLevel {
        orders: o,
        price_qty: Some(proto::PriceQty {
            price: p,
            quantity: q,
        }),
    };
    proto::LiveData {
        depth_data: Some(proto::MarketDepth {
            ts_in_millis: 1759722012000.0,
            buy: vec![lvl(3, 812.3, 120.0), lvl(0, 0.0, 0.0)],
            sell: vec![lvl(2, 812.4, 80.0)],
        }),
        ..Default::default()
    }
    .encode_to_vec()
}

fn msg_frame(sid: u64, payload: &[u8]) -> Message {
    let mut b = format!("MSG /ld/x {} {}\r\n", sid, payload.len()).into_bytes();
    b.extend_from_slice(payload);
    b.extend_from_slice(b"\r\n");
    Message::Binary(b)
}

#[test]
fn protobuf_decodes_hand_built_messages() {
    let d = proto::decode(&ltp_payload(812.35)).unwrap();
    assert_eq!(d.symbol, "SBIN");
    assert_eq!(d.exchange, 1);
    assert_eq!(d.ltp_data.unwrap().ltp, 812.35);
    let i = proto::LiveIndex {
        ts_in_millis: 1.0,
        value: 24890.15,
    };
    let bytes = proto::LiveData {
        index_data: Some(i),
        ..Default::default()
    }
    .encode_to_vec();
    assert_eq!(
        proto::decode(&bytes).unwrap().index_data.unwrap().value,
        24890.15
    );
}

#[test]
fn feed_frames_and_ticks() {
    let mut f = GrowwFeed::new(
        crate::brokers::common::http::client(),
        "tok",
        FeedEndpoints::default(),
    );
    assert!(f.awaits_auth_ack());
    assert_eq!(
        f.parse(&Message::Text("+OK\r\n".into())),
        vec![FeedEvent::AuthOk]
    );
    let frames = f.subscribe_frames(&[
        sub("SBIN", "NSE", "3045", FeedMode::Quote),
        sub("NIFTY28OCT2524500CE", "NFO", "35001", FeedMode::Depth),
    ]);
    assert_eq!(
        text(&frames[0]),
        "SUB /ld/eq/nse/price.3045 1\r\nSUB /ld/fo/nse/price.35001 2\r\nSUB /ld/fo/nse/book.35001 3\r\nPING\r\n"
    );
    // Quote tick.
    let ev = f.parse(&msg_frame(1, &ltp_payload(812.35)));
    let FeedEvent::Tick(t) = &ev[0] else {
        panic!("{:?}", ev)
    };
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str(), t.mode),
        ("SBIN", "NSE", 2)
    );
    assert_eq!(
        (t.ltp, t.open, t.close, t.volume),
        (812.35, 809.0, 808.0, 5_123_400)
    );
    assert_eq!(t.last_trade_time_ms, 1759722011000);
    assert_eq!(t.change, 4.35);
    // Quote subscriptions ignore book ticks; depth subscriptions merge.
    assert!(f.parse(&msg_frame(1, &depth_payload())).is_empty());
    let ev = f.parse(&msg_frame(3, &depth_payload()));
    assert_eq!(ev.len(), 2);
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!("{:?}", ev)
    };
    assert_eq!(d.symbol, "NIFTY28OCT2524500CE");
    assert_eq!(d.buy.len(), 1); // the zero placeholder level is dropped
    assert_eq!((d.buy[0].price, d.buy[0].orders), (812.3, 3));
    let ev = f.parse(&msg_frame(2, &ltp_payload(112.4)));
    let FeedEvent::Depth(d) = &ev[1] else {
        panic!("{:?}", ev)
    };
    assert_eq!((d.ltp, d.sell[0].price), (112.4, 812.4));
    // Unknown sid, garbage payload: nothing.
    assert!(f.parse(&msg_frame(99, &ltp_payload(1.0))).is_empty());
    assert!(f.parse(&msg_frame(1, b"\xff\xff")).is_empty());
    // Unsubscribe removes every sid of the instrument.
    let un = f.unsubscribe_frames(&[sub("NIFTY28OCT2524500CE", "NFO", "35001", FeedMode::Depth)]);
    assert_eq!(text(&un[0]), "UNSUB 2\r\nUNSUB 3\r\n");
    assert_eq!(f.instrument_count(), 1);
    assert!(f.parse(&msg_frame(3, &depth_payload())).is_empty());
    // A new connection forgets per-connection sids.
    f.on_connected();
    assert_eq!(f.instrument_count(), 0);
    assert!(matches!(
        f.parse(&Message::Text("-ERR 'Authentication Timeout'\r\n".into()))[0],
        FeedEvent::AuthFailed(_)
    ));
}

#[test]
fn index_ticks_carry_ltp() {
    let mut f = GrowwFeed::new(
        crate::brokers::common::http::client(),
        "tok",
        FeedEndpoints::default(),
    );
    f.subscribe_frames(&[sub("NIFTY", "NSE_INDEX", "NIFTY", FeedMode::Depth)]);
    let bytes = proto::LiveData {
        index_data: Some(proto::LiveIndex {
            ts_in_millis: 5.0,
            value: 24890.15,
        }),
        ..Default::default()
    }
    .encode_to_vec();
    let ev = f.parse(&msg_frame(1, &bytes));
    assert_eq!(ev.len(), 1);
    let FeedEvent::Tick(t) = &ev[0] else { panic!() };
    assert_eq!((t.ltp, t.exchange.as_str()), (24890.15, "NSE_INDEX"));
}

// ---------------------------------------------------------------------------
// Order poller
// ---------------------------------------------------------------------------

#[test]
fn poller_diff_seeds_then_reports_changes() {
    let r = master();
    let raw = orders_of(&fx(fixture!("order_list_cash.json")));
    let mut book = map_orders(&raw, &r);
    let (snap, changed) = diff(None, &book);
    assert!(changed.is_empty());
    assert_eq!(snap.len(), 4);
    book[1].status = "complete".into();
    book[1].filled_quantity = 5;
    let (_, changed) = diff(Some(&snap), &book);
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].orderid, "GMK39038RDT490CCVRP");
    assert_eq!(
        (changed[0].order_status.as_str(), changed[0].filled_quantity),
        ("complete", 5)
    );
    assert_eq!(clamp_interval(std::time::Duration::ZERO).as_secs(), 1);
    assert_eq!(
        clamp_interval(std::time::Duration::from_secs(600)).as_secs(),
        60
    );
}

// ---------------------------------------------------------------------------
// HTTP round trips against a local fake Groww (ephemeral port)
// ---------------------------------------------------------------------------

mod http_round_trip {
    use super::*;
    use axum::extract::{Path, Query};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::Arc;

    type Seen = Arc<Mutex<Vec<String>>>;

    #[derive(Clone, Copy, Default)]
    struct Faults {
        fno_positions_fail: bool,
        fno_trades_fail: bool,
        cash_orders_fail: bool,
    }

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{}", addr)
    }

    fn bearer(h: &HeaderMap) -> String {
        h.get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    }

    fn api_version(h: &HeaderMap) -> bool {
        h.get("x-api-version").and_then(|v| v.to_str().ok()) == Some("1.0")
    }

    /// Live prices the fake LTP endpoint knows.
    fn ltp_of(key: &str) -> Option<f64> {
        match key {
            "NSE_SBIN" => Some(820.0),
            "NSE_NIFTY25OCT24500CE" => Some(120.4),
            "NSE_NIFTYBEES" => None,
            _ => Some(106.5),
        }
    }

    fn fake(seen: Seen, f: Faults) -> Router {
        let log = move |s: &Seen, line: String| s.lock().push(line);
        let s = seen.clone();
        let s2 = seen.clone();
        let s3 = seen.clone();
        let s4 = seen.clone();
        let s5 = seen.clone();
        let s6 = seen.clone();
        let s7 = seen.clone();
        let s8 = seen.clone();
        let s9 = seen.clone();
        let s10 = seen.clone();
        let s11 = seen;
        Router::new()
            .route(
                "/v1/token/api/access",
                post(move |h: HeaderMap, Json(b): Json<Value>| async move {
                    log(&s, format!("token|{}|{}|{}", bearer(&h), b["key_type"], api_version(&h)));
                    let ok = match b["key_type"].as_str() {
                        Some("totp") => b["totp"] == "123456",
                        Some("approval") => {
                            let ts = b["timestamp"].as_str().unwrap_or("");
                            b["checksum"] == checksum("SECRET", ts)
                        }
                        _ => false,
                    };
                    if ok {
                        (StatusCode::OK, Json(json!({"token": "good", "tokenRefId": "r", "isActive": true})))
                    } else {
                        (StatusCode::BAD_REQUEST, Json(json!({"status": "FAILURE", "error": {"code": "GA001", "message": "bad"}})))
                    }
                }),
            )
            .route(
                "/v1/margins/detail/user",
                get(|h: HeaderMap| async move {
                    if bearer(&h) == "Bearer good" && api_version(&h) {
                        (StatusCode::OK, Json(fx(fixture!("funds.json"))))
                    } else {
                        (StatusCode::UNAUTHORIZED, Json(json!({"status": "FAILURE"})))
                    }
                }),
            )
            .route(
                "/v1/order/list",
                get(move |h: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                    log(&s2, format!("list|{}|{}|{}|{}", q["segment"], q["page"], q["page_size"], api_version(&h)));
                    if q["segment"] == "CASH" && f.cash_orders_fail {
                        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"status": "FAILURE", "error": {"message": "Unable to serve request currently"}})));
                    }
                    if q["page"] != "0" {
                        return (StatusCode::OK, Json(json!({"status": "SUCCESS", "payload": {"order_list": []}})));
                    }
                    if q["segment"] == "FNO" {
                        (StatusCode::OK, Json(fx(fixture!("order_list_fno.json"))))
                    } else {
                        (StatusCode::OK, Json(fx(fixture!("order_list_cash.json"))))
                    }
                }),
            )
            .route(
                "/v1/order/create",
                post(move |h: HeaderMap, Json(b): Json<Value>| async move {
                    log(&s3, format!("create|{}|{}", b, api_version(&h)));
                    if b["quantity"] == 7 {
                        return (StatusCode::OK, Json(json!({"status": "SUCCESS", "payload": {"groww_order_id": "GMK7", "order_status": "FAILED", "remark": "Retry within the allowed price range for stop-loss trigger."}})));
                    }
                    if b["quantity"] == 8 {
                        return (StatusCode::BAD_REQUEST, Json(json!({"status": "FAILURE", "error": {"code": "GA001", "message": "Insufficient margin"}})));
                    }
                    (StatusCode::OK, Json(json!({"status": "SUCCESS", "payload": {"groww_order_id": "GMK1", "order_status": "OPEN", "order_reference_id": b["order_reference_id"]}})))
                }),
            )
            .route(
                "/v1/order/modify",
                post(move |Json(b): Json<Value>| async move {
                    log(&s4, format!("modify|{}", b));
                    if b["groww_order_id"] == "BAD" {
                        return (StatusCode::BAD_REQUEST, Json(json!({"status": "FAILURE", "error": {"message": "Order not modifiable"}})));
                    }
                    if b["groww_order_id"] == "SOFT" {
                        // HTTP 200 but FAILURE: not a success.
                        return (StatusCode::OK, Json(json!({"status": "FAILURE", "error": {"message": "Order already executed"}})));
                    }
                    (StatusCode::OK, Json(json!({"status": "SUCCESS", "payload": {"groww_order_id": b["groww_order_id"], "order_status": "MODIFICATION_REQUESTED"}})))
                }),
            )
            .route(
                "/v1/order/cancel",
                post(move |h: HeaderMap, Json(b): Json<Value>| async move {
                    log(&s5, format!("cancel|{}|{}|{}", b["groww_order_id"], b["segment"], api_version(&h)));
                    let id = b["groww_order_id"].as_str().unwrap_or("");
                    // An F&O order sent to CASH is refused by Groww.
                    if id.starts_with("GLTFO") && b["segment"] == "CASH" {
                        return (StatusCode::BAD_REQUEST, Json(json!({"status": "FAILURE", "error": {"message": "Order not found"}})));
                    }
                    if id == "GONE" {
                        return (StatusCode::BAD_REQUEST, Json(json!({"status": "FAILURE", "error": {"message": "Order not found"}})));
                    }
                    (StatusCode::OK, Json(json!({"status": "SUCCESS", "payload": {"groww_order_id": id, "order_status": "CANCELLATION_REQUESTED"}})))
                }),
            )
            .route(
                "/v1/order/trades/{id}",
                get(move |Path(id): Path<String>, Query(q): Query<HashMap<String, String>>| async move {
                    log(&s6, format!("trades|{}|{}|{}|{}", id, q["segment"], q["page"], q["page_size"]));
                    if id == "GMK39038RDT490CCVRO" && q["page"] == "0" {
                        return (StatusCode::OK, Json(fx(fixture!("trades.json"))));
                    }
                    if id == "GLTFO25100600001" && q["segment"] == "FNO" && !f.fno_trades_fail {
                        if q["page"] != "0" {
                            return (StatusCode::OK, Json(json!({"status": "SUCCESS", "payload": {"trade_list": []}})));
                        }
                        // A full page: the next page must be read too.
                        let list: Vec<Value> = (0..50)
                            .map(|i| json!({"groww_trade_id": format!("GLT{}", i), "groww_order_id": id,
                                            "trading_symbol": "NIFTY25OCT24500CE", "exchange": "NSE", "segment": "FNO",
                                            "quantity": 1, "price": 112.4, "product": "NRML", "transaction_type": "BUY",
                                            "trade_date_time": "2025-10-06T10:00:01"}))
                            .collect();
                        return (StatusCode::OK, Json(json!({"status": "SUCCESS", "payload": {"trade_list": list}})));
                    }
                    if q["page"] != "0" {
                        return (StatusCode::OK, Json(json!({"status": "SUCCESS", "payload": {"trade_list": []}})));
                    }
                    (StatusCode::NOT_FOUND, Json(json!({"status": "FAILURE", "error": {"message": "No trades"}})))
                }),
            )
            .route(
                "/v1/positions/user",
                get(move |Query(q): Query<HashMap<String, String>>| async move {
                    if q["segment"] == "FNO" {
                        if f.fno_positions_fail {
                            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"status": "FAILURE"})));
                        }
                        return (StatusCode::OK, Json(fx(fixture!("positions_fno.json"))));
                    }
                    (StatusCode::OK, Json(fx(fixture!("positions_cash.json"))))
                }),
            )
            .route(
                "/v1/holdings/user",
                get(|h: HeaderMap| async move {
                    if !api_version(&h) {
                        return Json(json!({"status": "FAILURE"}));
                    }
                    Json(fx(fixture!("holdings.json")))
                }),
            )
            .route(
                "/v1/live-data/quote",
                get(move |Query(q): Query<HashMap<String, String>>| async move {
                    log(&s7, format!("quote|{}|{}|{}", q["exchange"], q["segment"], q["trading_symbol"]));
                    if q["trading_symbol"] == "BANKEX" {
                        return (StatusCode::BAD_REQUEST, Json(json!({"status": "FAILURE", "error": {"message": "No data retrieved"}})));
                    }
                    if q["segment"] == "FNO" {
                        (StatusCode::OK, Json(fx(fixture!("quote_fno.json"))))
                    } else {
                        (StatusCode::OK, Json(fx(fixture!("quote_cash.json"))))
                    }
                }),
            )
            .route(
                "/v1/live-data/ohlc",
                get(move |Query(q): Query<HashMap<String, String>>| async move {
                    let syms = q["exchange_symbols"].clone();
                    log(&s8, format!("ohlc|{}|{}", q["segment"], syms));
                    if syms.contains("NSE_BOGUS") {
                        return (StatusCode::BAD_REQUEST, Json(json!({"status": "FAILURE", "error": {"message": "Invalid trading symbol: BOGUS"}})));
                    }
                    let mut p = serde_json::Map::new();
                    for s in syms.split(',') {
                        if s == "NSE_RELIANCE" {
                            // An OHLC entry Groww sends as a bare number.
                            p.insert(s.to_string(), json!(101.5));
                        } else {
                            p.insert(s.to_string(), json!("{open: 100.0,high: 110.0,low: 95.0,close: 105.0}"));
                        }
                    }
                    (StatusCode::OK, Json(json!({"status": "SUCCESS", "payload": p})))
                }),
            )
            .route(
                "/v1/live-data/ltp",
                get(move |Query(q): Query<HashMap<String, String>>| async move {
                    let syms = q["exchange_symbols"].clone();
                    log(&s9, format!("ltp|{}|{}", q["segment"], syms));
                    let mut p = serde_json::Map::new();
                    for s in syms.split(',') {
                        if let Some(v) = ltp_of(s) {
                            p.insert(s.to_string(), json!(v));
                        }
                    }
                    Json(json!({"status": "SUCCESS", "payload": p}))
                }),
            )
            .route(
                "/v1/historical/candles",
                get(move |Query(q): Query<HashMap<String, String>>| async move {
                    log(&s10, format!(
                        "candles|{}|{}|{}|{}|{}|{}",
                        q["exchange"], q["segment"], q["groww_symbol"], q["candle_interval"], q["start_time"], q["end_time"]
                    ));
                    Json(fx(fixture!("history_intraday.json")))
                }),
            )
            .route(
                "/v1/historical/candle/range",
                get(move |Query(q): Query<HashMap<String, String>>| async move {
                    log(&s11, format!(
                        "range|{}|{}|{}|{}",
                        q["exchange"], q["segment"], q["trading_symbol"], q["interval_in_minutes"]
                    ));
                    Json(fx(fixture!("history_daily.json")))
                }),
            )
            .route(
                "/v1/margins/detail/orders",
                post(|Query(q): Query<HashMap<String, String>>, Json(b): Json<Value>| async move {
                    let items = b.as_array().cloned().unwrap_or_default();
                    assert!(items.iter().all(|i| i["segment"] == q["segment"]));
                    if q["segment"] == "CASH" {
                        assert_eq!(items.len(), 1, "CASH has no basket");
                    }
                    Json(fx(fixture!("margin.json")))
                }),
            )
            .fallback(|| async { StatusCode::NOT_FOUND.into_response() })
    }

    async fn broker_with(f: Faults) -> (GrowwBroker, Seen) {
        let seen: Seen = Arc::default();
        let base = serve(fake(seen.clone(), f)).await;
        (GrowwBroker::with_base_url(master(), base), seen)
    }

    async fn broker(fno_fail: bool) -> (GrowwBroker, Seen) {
        broker_with(Faults {
            fno_positions_fail: fno_fail,
            ..Default::default()
        })
        .await
    }

    fn creds() -> BrokerCredentials {
        BrokerCredentials {
            api_key: "APIKEY".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn auth_variants() {
        let (b, seen) = broker(false).await;
        let mut c = creds();
        c.totp = Some("123456".into());
        assert_eq!(b.authenticate(c).await.unwrap().auth_token, "good");
        let mut c = creds();
        c.api_secret = Some("SECRET".into());
        assert_eq!(b.authenticate(c).await.unwrap().auth_token, "good");
        let mut c = creds();
        c.password = Some("good".into());
        assert_eq!(b.authenticate(c).await.unwrap().auth_token, "good");
        let mut c = creds();
        c.password = Some("stale".into());
        assert_eq!(b.authenticate(c).await.unwrap_err().code(), "AUTH_ERROR");
        let mut c = creds();
        c.api_secret = Some("WRONG".into());
        let e = b.authenticate(c).await.unwrap_err();
        assert_eq!(e.code(), "AUTH_ERROR");
        assert!(!e.client_message().contains("400"));
        assert!(e.client_message().contains("bad"));
        let seen = seen.lock().clone();
        assert_eq!(seen[0], "token|Bearer APIKEY|\"totp\"|true");
        assert_eq!(seen[1], "token|Bearer APIKEY|\"approval\"|true");
    }

    #[tokio::test]
    async fn orders_and_books() {
        let (b, seen) = broker(false).await;
        let auth = AuthToken::new("good");
        let r = b
            .place_order(
                &auth,
                &resolved("NIFTY28OCT2524500CE", "NFO", "LIMIT", 110.5, 0.0),
            )
            .await
            .unwrap();
        assert_eq!(r.order_id, "GMK1");
        let book = b.get_order_book(&auth).await.unwrap();
        assert_eq!(book.len(), 6);
        assert_eq!(book[4].symbol, "NIFTY28OCT2524500CE");
        // Cancel takes the segment from the book.
        b.cancel_order(&auth, "GMK39038RDT490CCVRP").await.unwrap();
        // Not in the book: CASH first, then FNO.
        b.cancel_order(&auth, "GLTFO25100600009").await.unwrap();
        // Refused in both: Groww's reason, never a success.
        let e = b.cancel_order(&auth, "GONE").await.unwrap_err();
        assert_eq!(e.client_message(), "Order not found");
        let all = b.cancel_all_orders(&auth).await.unwrap();
        assert_eq!(
            all.cancelled,
            ["GMK39038RDT490CCVRP", "GMK39038RDT490CCVRR"]
        );
        assert!(all.failed.is_empty());
        let trades = b.get_trade_book(&auth).await.unwrap();
        // Two CASH fills plus every page of the F&O order's fills; nothing
        // synthesised.
        assert_eq!(trades.len(), 52);
        assert_eq!(
            (trades[2].symbol.as_str(), trades[2].exchange.as_str()),
            ("NIFTY28OCT2524500CE", "NFO")
        );
        assert!(trades.iter().all(|t| !t.trade_id.starts_with("synthetic")));
        let seen = seen.lock().clone();
        let create = seen.iter().find(|s| s.starts_with("create|")).unwrap();
        assert!(create.contains("\"trading_symbol\":\"NIFTY25OCT24500CE\""));
        assert!(create.contains("\"segment\":\"FNO\""));
        assert!(create.ends_with("|true"), "X-API-VERSION on create");
        assert!(seen.contains(&"list|CASH|0|100|true".to_string()));
        assert!(seen.contains(&"list|FNO|0|100|true".to_string()));
        assert!(seen.contains(&"cancel|\"GMK39038RDT490CCVRP\"|\"CASH\"|true".to_string()));
        assert!(seen.contains(&"cancel|\"GLTFO25100600009\"|\"CASH\"|true".to_string()));
        assert!(seen.contains(&"cancel|\"GLTFO25100600009\"|\"FNO\"|true".to_string()));
        assert!(seen.contains(&"trades|GLTFO25100600001|FNO|0|50".to_string()));
        assert!(seen.contains(&"trades|GLTFO25100600001|FNO|1|50".to_string()));
        // Only filled orders are read for trades.
        assert!(!seen
            .iter()
            .any(|s| s.starts_with("trades|GMK39038RDT490CCVRP")));
    }

    /// Web test_multiquote_keeps_a_priced_symbol_whose_ohlc_is_a_bare_number.
    #[tokio::test]
    async fn multiquote_keeps_a_priced_symbol_whose_ohlc_is_a_bare_number() {
        let (b, seen) = broker(false).await;
        let auth = AuthToken::new("good");
        let mq = b
            .get_multiquotes(&auth, &[QuoteKey::new("NSE", "RELIANCE")])
            .await
            .unwrap();
        assert!(mq[0].error.is_none(), "{:?}", mq[0].error);
        let q = mq[0].data.as_ref().unwrap();
        // The live price stands; the bare number gives no OHLC breakdown.
        assert_eq!((q.ltp, q.open, q.close), (106.5, 0.0, 0.0));
        assert!(seen.lock().contains(&"ohlc|CASH|NSE_RELIANCE".to_string()));
    }

    /// Close-all reads the position book without live prices (web
    /// `close_all_positions` uses `include_ltp=False`) and exits each open
    /// row with the OpenAlgo symbol.
    #[tokio::test]
    async fn close_all_reads_positions_without_prices() {
        let (b, seen) = broker(false).await;
        let auth = AuthToken::new("good");
        let r = b.close_all_positions(&auth).await.unwrap();
        assert!(!r.placed.is_empty() || !r.failed.is_empty());
        let seen = seen.lock().clone();
        assert!(!seen.iter().any(|s| s.starts_with("ltp|")), "{seen:?}");
        assert!(seen.iter().any(|s| s.starts_with("create|")), "{seen:?}");
    }

    #[tokio::test]
    async fn place_refusals_are_errors() {
        let (b, seen) = broker(false).await;
        let auth = AuthToken::new("good");
        let mut o = resolved("SBIN", "NSE", "MARKET", 0.0, 0.0);
        o.quantity = 7;
        let e = b.place_order(&auth, &o).await.unwrap_err();
        assert_eq!(
            e.client_message(),
            "Retry within the allowed price range for stop-loss trigger."
        );
        o.quantity = 8;
        let e = b.place_order(&auth, &o).await.unwrap_err();
        assert_eq!(e.client_message(), "Insufficient margin");
        // Refused before sending: nothing reaches Groww.
        let before = seen.lock().len();
        let bond = resolved("IMC1-N1", "NSE", "MARKET", 0.0, 0.0);
        assert!(b.place_order(&auth, &bond).await.is_err());
        let mut ioc = resolved("SBIN", "NSE", "MARKET", 0.0, 0.0);
        ioc.validity = Validity::Ioc;
        assert!(b.place_order(&auth, &ioc).await.is_err());
        assert_eq!(seen.lock().len(), before);
    }

    #[tokio::test]
    async fn unreadable_cash_order_book_is_an_error() {
        let (b, _) = broker_with(Faults {
            cash_orders_fail: true,
            ..Default::default()
        })
        .await;
        let auth = AuthToken::new("good");
        let e = b.get_order_book(&auth).await.unwrap_err();
        assert!(e
            .client_message()
            .contains("Unable to serve request currently"));
        // Not "nothing to cancel".
        assert!(b.cancel_all_orders(&auth).await.is_err());
    }

    #[tokio::test]
    async fn trade_book_reports_unread_fills_instead_of_inventing_them() {
        let (b, _) = broker_with(Faults {
            fno_trades_fail: true,
            ..Default::default()
        })
        .await;
        let e = b.get_trade_book(&AuthToken::new("good")).await.unwrap_err();
        assert!(e.client_message().contains("1 filled order"));
    }

    #[tokio::test]
    async fn modify_refusal_is_an_error() {
        let (b, _) = broker(false).await;
        let auth = AuthToken::new("good");
        let req = ModifyOrderRequest {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "CNC".into(),
            pricetype: "LIMIT".into(),
            quantity: 1,
            price: 800.0,
            trigger_price: 0.0,
            disclosed_quantity: 0,
        };
        let ok = ResolvedModify::resolve("GMK9", &req, &master()).unwrap();
        assert_eq!(b.modify_order(&auth, &ok).await.unwrap().order_id, "GMK9");
        let bad = ResolvedModify::resolve("BAD", &req, &master()).unwrap();
        let e = b.modify_order(&auth, &bad).await.unwrap_err();
        assert_eq!(e.client_message(), "Order not modifiable");
        let soft = ResolvedModify::resolve("SOFT", &req, &master()).unwrap();
        let e = b.modify_order(&auth, &soft).await.unwrap_err();
        assert_eq!(e.client_message(), "Order already executed");
    }

    #[tokio::test]
    async fn positions_holdings_funds_margin() {
        let (b, seen) = broker(false).await;
        let auth = AuthToken::new("good");
        let p = b.get_positions(&auth).await.unwrap();
        assert_eq!(p.len(), 3);
        // Open positions carry the live price and the open move.
        assert_eq!(p[0].ltp, 820.0);
        assert!((p[0].pnl - (820.0 - 808.23) * 15.0).abs() < 1e-6);
        // Closed: realised P&L only, no price read.
        assert_eq!((p[1].ltp, p[1].pnl), (0.0, 120.5));
        assert_eq!(p[2].ltp, 120.4);
        // Web test_position_read_failure (Groww no longer reads flat): the
        // smart-order read sees a held position.
        assert_eq!(
            b.get_open_position(&auth, "NIFTY28OCT2524500CE", Exchange::Nfo, Product::Nrml)
                .await
                .unwrap(),
            75
        );
        assert_eq!(
            b.get_open_position(&auth, "SBIN", Exchange::Nse, Product::Cnc)
                .await
                .unwrap(),
            15
        );
        let h = b.get_holdings(&auth).await.unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(
            (h[0].exchange.as_str(), h[0].ltp, h[0].pnl),
            ("NSE", 820.0, 3390.0)
        );
        assert_eq!((h[1].ltp, h[1].pnl), (0.0, 0.0));
        let book = b.get_holdings_with_totals(&auth).await.unwrap();
        assert_eq!(book.statistics().totalholdingvalue, 40910.0);
        let f = b.get_funds(&auth).await.unwrap();
        assert_eq!(f.available_cash, 125000.5);
        assert_eq!(f.m2m_realized, 120.5);
        let unrealised = (820.0 - 808.23) * 15.0 + (120.4 - 112.4) * 75.0;
        assert!((f.m2m_unrealized - unrealised).abs() < 1e-6);
        let m = b
            .calculate_margin(
                &auth,
                &[
                    leg("NIFTY28OCT2524500CE", "NFO", 0.0),
                    leg("SBIN", "NSE", 0.0),
                    leg("RELIANCE", "NSE", 0.0),
                ],
            )
            .await
            .unwrap();
        // One FNO basket plus two CASH orders, added.
        assert!((m.total_margin_required - 3.0 * 121000.75).abs() < 1e-6);
        let e = b.get_funds(&AuthToken::new("expired")).await.unwrap_err();
        assert_eq!(e.code(), "AUTH_ERROR");
        let seen = seen.lock().clone();
        // The smart-order read takes no prices.
        let ltp_calls = seen.iter().filter(|s| s.starts_with("ltp|")).count();
        assert!(ltp_calls >= 1);
        assert!(seen.contains(&"ltp|CASH|NSE_SBIN".to_string()));
        assert!(seen.contains(&"ltp|FNO|NSE_NIFTY25OCT24500CE".to_string()));
    }

    #[tokio::test]
    async fn failed_fno_positions_block_open_position() {
        let (b, _) = broker(true).await;
        let auth = AuthToken::new("good");
        // The book still shows cash positions.
        assert_eq!(b.get_positions(&auth).await.unwrap().len(), 2);
        assert_eq!(
            b.get_open_position(&auth, "SBIN", Exchange::Nse, Product::Cnc)
                .await
                .unwrap(),
            15
        );
        let e = b
            .get_open_position(&auth, "NIFTY28OCT2524500CE", Exchange::Nfo, Product::Nrml)
            .await
            .unwrap_err();
        assert_eq!(e.client_message(), super::super::orders::POSITION_UNREAD);
    }

    #[tokio::test]
    async fn quotes_depth_multiquotes_history() {
        let (b, seen) = broker(false).await;
        let auth = AuthToken::new("good");
        let q = b
            .get_quote(&auth, &QuoteKey::new("NSE", "SBIN"))
            .await
            .unwrap();
        assert_eq!(q.ltp, 812.35);
        // Indices go to their own exchange's CASH segment (QT-05/07).
        b.get_quote(&auth, &QuoteKey::new("BSE_INDEX", "SENSEX"))
            .await
            .unwrap();
        // A failed quote is an error with Groww's reason, not zeros.
        let e = b
            .get_quote(&auth, &QuoteKey::new("BSE", "BANKEX"))
            .await
            .unwrap_err();
        assert_eq!(e.client_message(), "No data retrieved");
        // Exchanges Groww has no data for are refused, not sent as NSE.
        assert!(b
            .get_quote(&auth, &QuoteKey::new("MCX", "CRUDEOIL"))
            .await
            .is_err());
        let d = b
            .get_market_depth(&auth, &QuoteKey::new("NFO", "NIFTY28OCT2524500CE"))
            .await
            .unwrap();
        assert_eq!(d.bids.len(), 5);
        assert_eq!(d.oi, 4567800);
        let mq = b
            .get_multiquotes(
                &auth,
                &[
                    QuoteKey::new("NSE", "SBIN"),
                    QuoteKey::new("NSE", "BOGUS"),
                    QuoteKey::new("NFO", "NIFTY28OCT2524500CE"),
                    QuoteKey::new("MCX", "CRUDEOIL"),
                    QuoteKey::new("NSE", "NIFTYBEES"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(mq.len(), 5);
        // Live price from the LTP endpoint; ohlc.close is the prev close.
        let sbin = mq[0].data.as_ref().unwrap();
        assert_eq!((sbin.ltp, sbin.close), (820.0, 105.0));
        assert_eq!(
            mq[1].error.as_deref(),
            Some("Invalid trading symbol in Groww")
        );
        // F&O overlay adds bid/ask and OI.
        let fo = mq[2].data.as_ref().unwrap();
        assert_eq!((fo.ltp, fo.bid, fo.oi), (120.4, 112.35, 4567800));
        assert!(mq[3].error.as_deref().unwrap().contains("MCX"));
        // No live price: an error, not yesterday's close.
        assert_eq!(
            mq[4].error.as_deref(),
            Some("Groww returned no live price for this symbol")
        );
        let hist = |interval: &str| HistoryRequest {
            key: QuoteKey::new("NSE", "SBIN"),
            interval: interval.into(),
            start: chrono::NaiveDate::from_ymd_opt(2025, 10, 6).unwrap(),
            end: chrono::NaiveDate::from_ymd_opt(2025, 10, 6).unwrap(),
        };
        let h = b.get_history(&auth, &hist("5m")).await.unwrap();
        assert_eq!(h.len(), 4);
        b.get_history(&auth, &hist("1h")).await.unwrap();
        let day = b.get_history(&auth, &hist("D")).await.unwrap();
        assert_eq!(day[0].timestamp, 1759708800);
        let fno = HistoryRequest {
            key: QuoteKey::new("NFO", "NIFTY28OCT2524500CE"),
            ..hist("15m")
        };
        b.get_history(&auth, &fno).await.unwrap();
        let seen = seen.lock().clone();
        assert!(seen.contains(&"quote|NSE|CASH|SBIN".to_string()));
        assert!(seen.contains(&"quote|BSE|CASH|SENSEX".to_string()));
        assert!(seen.contains(&"quote|NSE|FNO|NIFTY25OCT24500CE".to_string()));
        assert!(!seen.iter().any(|s| s.contains("CRUDEOIL")));
        assert!(seen.contains(&"ohlc|CASH|NSE_SBIN,NSE_BOGUS,NSE_NIFTYBEES".to_string()));
        assert!(seen.contains(&"ohlc|CASH|NSE_SBIN,NSE_NIFTYBEES".to_string()));
        assert!(seen.contains(&"ltp|CASH|NSE_SBIN,NSE_NIFTYBEES".to_string()));
        assert!(seen.contains(&"ohlc|FNO|NSE_NIFTY25OCT24500CE".to_string()));
        assert!(seen.contains(
            &"candles|NSE|CASH|NSE-SBIN|5minute|2025-10-06 00:00:00|2025-10-06 23:59:59"
                .to_string()
        ));
        // 1h is built from 15m candles.
        assert!(seen.contains(
            &"candles|NSE|CASH|NSE-SBIN|15minute|2025-10-06 00:00:00|2025-10-06 23:59:59"
                .to_string()
        ));
        assert!(seen.contains(&"range|NSE|CASH|SBIN|1440".to_string()));
        assert!(seen.contains(
            &"candles|NSE|FNO|NSE-NIFTY-28Oct25-24500-CE|15minute|2025-10-06 00:00:00|2025-10-06 23:59:59"
                .to_string()
        ));
    }

    #[tokio::test]
    async fn empty_token_is_refused_before_any_call() {
        let b = GrowwBroker::with_base_url(master(), "http://127.0.0.1:9");
        let e = b.get_order_book(&AuthToken::new("")).await.unwrap_err();
        assert_eq!(e.code(), "AUTH_ERROR");
        assert!(b.create_feed(&AuthToken::new(" ")).is_err());
    }

    #[tokio::test]
    async fn poller_publishes_changes_and_stops() {
        let (b, _) = broker(false).await;
        let auth = AuthToken::new("good");
        let mut rx = b
            .start_order_updates(&auth, std::time::Duration::from_secs(1))
            .unwrap();
        assert!(b.order_updates_running());
        // Seed poll publishes nothing.
        let r = tokio::time::timeout(std::time::Duration::from_millis(1500), rx.recv()).await;
        assert!(r.is_err());
        b.stop_order_updates();
        assert!(!b.order_updates_running());
        // The task is gone, so the channel closes.
        let end = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
        assert!(matches!(end, Ok(None)));
    }
}

/// Web test_groww_positions_price.py (#2173): every position price is the
/// rupees Groww sent; no paise conversion, no step at 1000.
#[test]
fn position_prices_are_the_rupees_groww_sent() {
    let r = master();
    let pos = |v: serde_json::Value| -> Position {
        let mut base = serde_json::json!({
            "trading_symbol": "SBIN", "exchange": "NSE", "segment": "CASH",
            "product": "CNC", "credit_quantity": 1, "debit_quantity": 1,
            "net_price": 433.0, "credit_price": 433.0, "debit_price": 0.0
        });
        for (k, val) in v.as_object().unwrap() {
            base[k] = val.clone();
        }
        map_position(&serde_json::from_value(base).unwrap(), "CASH", &r)
    };
    for price in [0.05, 4.33, 433.0, 999.99, 1000.0, 1001.0, 25_000.5] {
        assert_eq!(
            pos(serde_json::json!({"net_price": price})).average_price,
            price
        );
        assert_eq!(
            pos(serde_json::json!({"credit_price": price})).buy_value,
            price
        );
        assert_eq!(
            pos(serde_json::json!({"debit_price": price})).sell_value,
            price
        );
    }
    let below = pos(serde_json::json!({"net_price": 1000.0})).average_price;
    let above = pos(serde_json::json!({"net_price": 1001.0})).average_price;
    assert!((above - below - 1.0).abs() < 1e-9, "no step in the scale");
    assert_eq!(pos(serde_json::json!({"debit_price": 0.0})).sell_value, 0.0);
    let nulls = pos(serde_json::json!({"credit_price": null, "debit_price": null}));
    assert_eq!((nulls.buy_value, nulls.sell_value), (0.0, 0.0));
    assert_eq!(
        pos(serde_json::json!({"net_price": "433.0"})).average_price,
        433.0
    );
}

#[tokio::test]
async fn order_feed_is_the_poller_and_logout_stops_it() {
    use crate::brokers::common::streaming::OrderFeed;
    let b = GrowwBroker::with_base_url(master(), "http://127.0.0.1:9");
    let auth = AuthToken::new("tok");
    let feed = b.create_order_feed(&auth).unwrap();
    assert!(b.order_updates_running());
    b.on_logout().await;
    assert!(!b.order_updates_running());
    // The update channel closes with the poller.
    let OrderFeed::Stream(mut rx) = feed else {
        panic!("expected the poller stream")
    };
    let closed = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
    assert!(matches!(closed, Ok(None)));
    assert!(b.create_order_feed(&AuthToken::new(" ")).is_err());
}
