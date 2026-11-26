//! Flattrade's ticks from Shoonya's symbol files (web
//! `broker/flattrade/database/master_contract_db.py`
//! `download_shoonya_tick_sizes` and `apply_shoonya_tick_sizes`,
//! marketcalls/openalgo#2508, 702153110). The web commit has no test; these
//! take its example (NATGASMINI on MCX ticks in 0.10, not 0.05) and its
//! rules: matched on exchange and token, an index by its exchange less
//! `_INDEX`, a tick of zero or less left out, unlisted rows kept.

use super::{apply, parse_into, then_wait, Ticks, WAIT};
use crate::brokers::families::noren::master_contract::parse_file;
use crate::brokers::{flattrade, shoonya};
use crate::error::{AppError, Result};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{sleep, Instant};

const SHOONYA_MCX: &str = "Exchange,Token,LotSize,GNGD,Symbol,TradingSymbol,Expiry,Instrument,OptionType,StrikePrice,TickSize,\n\
MCX,451669,250,1,NATGASMINI,NATGASMINI26DEC26,26-DEC-2026,FUTCOM,XX,0,0.10,\n\
MCX,440001,100,1,CRUDEOIL,CRUDEOIL19DEC26,19-DEC-2026,FUTCOM,XX,0,1,\n";

const SHOONYA_NSE: &str = "Exchange,Token,LotSize,Symbol,TradingSymbol,Instrument,TickSize,\n\
NSE,2885,1,RELIANCE,RELIANCE-EQ,EQ,10,\n\
NSE,3045,1,SBIN,SBIN-EQ,EQ,5,\n\
NSE,26000,0,NIFTY,Nifty 50,INDEX,0,\n\
NSE,26009,0,BANKNIFTY,Nifty Bank,INDEX,,\n\
NSE,11536,1,TCS,TCS-EQ,EQ,abc,\n";

const FLATTRADE_MCX: &str =
    "Exchange,Token,Lotsize,Symbol,Tradingsymbol,Instrument,Expiry,Strike,Optiontype\n\
MCX,451669,250,NATGASMINI,NATGASMINI26DEC26,FUTCOM,26-DEC-2026,0.00,XX\n\
MCX,999999,100,GOLDM,GOLDM05DEC26,FUTCOM,05-DEC-2026,0.00,XX\n";

const FLATTRADE_NSE: &str =
    "Exchange,Token,Lotsize,Symbol,Tradingsymbol,Instrument,Expiry,Strike,Optiontype\n\
NSE,2885,1,RELIANCE,RELIANCE-EQ,EQ,,,\n\
NSE,3045,1,SBIN,SBIN-EQ,EQ,,,\n\
NSE,26000,,NIFTY,Nifty 50,INDEX,,,\n";

fn shoonya_ticks(files: &[&str]) -> Ticks {
    let mut ticks = Ticks::new();
    for text in files {
        parse_into(shoonya::config(), text, &mut ticks).expect("the tick columns");
    }
    ticks
}

fn tick_of(rows: &[crate::brokers::common::symbols::SymToken], symbol: &str) -> f64 {
    rows.iter()
        .find(|r| r.symbol == symbol)
        .unwrap_or_else(|| panic!("{symbol} in the master"))
        .tick_size
}

/// The web commit's case: Flattrade's file gives NATGASMINI the fixed
/// 0.05, Shoonya's gives 0.10, and the stored row ticks in 0.10. A token
/// Shoonya does not list keeps the fixed tick.
#[test]
fn natgasmini_takes_shoonyas_tick_and_an_unlisted_contract_keeps_the_fixed_one() {
    let mut rows = parse_file(flattrade::config(), "MCX", FLATTRADE_MCX);
    assert_eq!(tick_of(&rows, "NATGASMINI26DEC26FUT"), 0.05);
    apply(
        flattrade::config(),
        &mut rows,
        &shoonya_ticks(&[SHOONYA_MCX]),
    );
    assert_eq!(tick_of(&rows, "NATGASMINI26DEC26FUT"), 0.10);
    assert_eq!(tick_of(&rows, "GOLDM05DEC26FUT"), 0.05);
}

/// Shoonya's NSE and BSE files give the tick in paise, as Shoonya's own
/// master reads them (`TickRule::CashInPaise`): RELIANCE's 10 is 0.10 and
/// SBIN's 5 is 0.05, never 10 and 5 (the web's Flattrade reads them as
/// rupees, a web defect not copied). A derivative file's tick is in
/// rupees. Zero, blank and unreadable ticks are no ticks.
#[test]
fn shoonyas_cash_ticks_are_paise_and_a_zero_or_unreadable_tick_is_left_out() {
    let mut ticks = Ticks::new();
    assert_eq!(
        parse_into(shoonya::config(), SHOONYA_NSE, &mut ticks),
        Some(2)
    );
    assert_eq!(
        parse_into(shoonya::config(), SHOONYA_MCX, &mut ticks),
        Some(2)
    );
    assert_eq!(ticks["NSE"]["2885"], 0.10);
    assert_eq!(ticks["NSE"]["3045"], 0.05);
    for token in ["26000", "26009", "11536"] {
        assert!(!ticks["NSE"].contains_key(token), "{token}");
    }
    assert_eq!(ticks["MCX"]["451669"], 0.10);
    assert_eq!(ticks["MCX"]["440001"], 1.0);

    let mut rows = parse_file(flattrade::config(), "NSE", FLATTRADE_NSE);
    apply(flattrade::config(), &mut rows, &ticks);
    assert_eq!(tick_of(&rows, "RELIANCE"), 0.10);
    assert_eq!(tick_of(&rows, "SBIN"), 0.05);
    assert_eq!(tick_of(&rows, "NIFTY"), 0.05, "Shoonya gives the index 0");
}

/// An index row is matched by its exchange less `_INDEX` (Flattrade's
/// NIFTY is `NSE_INDEX`, Shoonya lists it under NSE), and on the token as
/// well as the exchange: the same token on another exchange is another
/// contract.
#[test]
fn an_index_row_matches_by_its_exchange_less_index_and_the_exchange_counts() {
    let mut rows = parse_file(flattrade::config(), "NSE", FLATTRADE_NSE);
    let nifty = rows.iter().find(|r| r.symbol == "NIFTY").expect("NIFTY");
    assert_eq!(nifty.brexchange, "NSE_INDEX");
    let mut ticks = Ticks::new();
    ticks
        .entry("NSE".into())
        .or_default()
        .insert("26000".into(), 0.5);
    ticks
        .entry("BSE".into())
        .or_default()
        .insert("2885".into(), 2.0);
    apply(flattrade::config(), &mut rows, &ticks);
    assert_eq!(tick_of(&rows, "NIFTY"), 0.5);
    assert_eq!(tick_of(&rows, "RELIANCE"), 0.05);
}

/// A file without the three columns gives no ticks (the web's `usecols`
/// fails it and skips it), and an empty file none either.
#[test]
fn a_file_without_the_tick_columns_gives_nothing() {
    let mut ticks = Ticks::new();
    let no_tick = "Exchange,Token,LotSize,Symbol,TradingSymbol,Instrument\nNSE,2885,1,RELIANCE,RELIANCE-EQ,EQ\n";
    assert_eq!(parse_into(shoonya::config(), no_tick, &mut ticks), None);
    assert_eq!(parse_into(shoonya::config(), "", &mut ticks), None);
    assert!(ticks.is_empty());
}

/// Shoonya, Zebu and TradeSmart read their own files' ticks: none of them
/// has a tick source, and rows of a member without one are never touched,
/// whatever ticks are at hand.
#[test]
fn only_flattrade_has_a_tick_source_and_no_other_member_is_touched() {
    use crate::brokers::{tradesmart, zebu};
    assert_eq!(
        flattrade::config().tick_source.map(|s| s.id),
        Some("shoonya")
    );
    for cfg in [shoonya::config(), zebu::config(), tradesmart::config()] {
        assert!(cfg.tick_source.is_none(), "{}", cfg.id);
    }
    let mut ticks = shoonya_ticks(&[SHOONYA_NSE]);
    ticks
        .entry("NSE".into())
        .or_default()
        .insert("2885".into(), 9.0);
    let mut rows = parse_file(shoonya::config(), "NSE", SHOONYA_NSE);
    let before: Vec<f64> = rows.iter().map(|r| r.tick_size).collect();
    apply(shoonya::config(), &mut rows, &ticks);
    let after: Vec<f64> = rows.iter().map(|r| r.tick_size).collect();
    assert_eq!(before, after);
}

/// `value` after `secs` on the test's paused clock.
async fn after<V>(secs: u64, value: V) -> V {
    sleep(Duration::from_secs(secs)).await;
    value
}

/// The member's own files, in after `secs`.
fn own_files_in(secs: u64) -> impl Future<Output = Result<()>> {
    after(secs, Ok(()))
}

/// Review of web #2508: the web waits `_TICK_WAIT_SECONDS` on its tick
/// worker only after Flattrade's own files are downloaded and processed.
/// The desktop started its 120 s with the download, so on a slow line
/// Shoonya's files, still coming at 120 s while Flattrade's own took
/// longer, were dropped. Files read before the member's own are in, or
/// within 120 s after, are kept.
#[tokio::test(start_paused = true)]
async fn the_wait_for_the_tick_files_starts_once_the_members_own_files_are_in() {
    let start = Instant::now();
    let (_, read) = then_wait(own_files_in(200), after(150, "ticks"))
        .await
        .expect("own files");
    assert_eq!(read, Some("ticks"), "read at 150 s, own files in at 200 s");
    assert_eq!(start.elapsed(), Duration::from_secs(200));

    let start = Instant::now();
    let (_, read) = then_wait(own_files_in(10), after(125, "ticks"))
        .await
        .expect("own files");
    assert_eq!(read, Some("ticks"), "read 115 s after the own files");
    assert_eq!(start.elapsed(), Duration::from_secs(125));
}

/// The wait is bounded: tick files not read within `WAIT` of the member's
/// own files are left out, and the download goes on without them.
#[tokio::test(start_paused = true)]
async fn tick_files_not_read_within_the_wait_after_the_own_files_are_left_out() {
    assert_eq!(
        WAIT,
        Duration::from_secs(120),
        "the web's _TICK_WAIT_SECONDS"
    );
    let start = Instant::now();
    let (_, read) = then_wait(own_files_in(10), after(131, "ticks"))
        .await
        .expect("own files");
    assert_eq!(read, None);
    assert_eq!(start.elapsed(), Duration::from_secs(10) + WAIT);
}

/// Set when the future holding it is dropped.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A refused download waits for no tick file: its error comes as soon as
/// the member's own files fail, and the task reading the source's files
/// ends with it (fd-audit: nothing is left running).
#[tokio::test(start_paused = true)]
async fn a_refused_download_ends_the_tick_read_at_once() {
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = DropFlag(dropped.clone());
    let read = async move {
        let _flag = flag;
        after(1_000, "ticks").await
    };
    let start = Instant::now();
    let refused = then_wait(
        after(5, Err::<(), _>(AppError::Broker("down".into()))),
        read,
    )
    .await;
    assert!(refused.is_err());
    assert_eq!(start.elapsed(), Duration::from_secs(5));
    for _ in 0..10 {
        if dropped.load(Ordering::SeqCst) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        dropped.load(Ordering::SeqCst),
        "the tick read was left running"
    );
}
