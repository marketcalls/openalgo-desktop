//! Pocketful master contract, ported from the web's
//! `test/test_pocketful_master_contract.py` (marketcalls/openalgo#2467,
//! a1d539764). The fixture files carry the web test's rows verbatim, in the
//! contract's own 14-column format, plus the desktop rows the book, order
//! and streaming tests resolve (SBIN-EQ 3045, NIFTY26OCTFUT 35003, ...).
//! `master_rows.json` there is what the web's own `process_pocketful_contract`
//! builds from those files (`scripts/gen_pocketful_master_golden.py`).

use super::{capture, Fake, Reply};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::pocketful::master_contract::{contract_request, parse_archive};
use crate::brokers::pocketful::{zip, PocketfulBroker, BASE_URL, MASTER_URL};
use crate::brokers::types::AuthToken;
use crate::brokers::Broker;
use crate::db::sqlite::{migrations, symbol};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(
            "../../../../tests/fixtures/brokers/pocketful/",
            $name
        ))
    };
}

const EXCHANGES: [&str; 5] = ["NSE", "BSE", "NFO", "BFO", "MCX"];

/// The web's failure sentence: the stored symbols stay in use.
const FAILED: &str = "Could not refresh the Pocketful symbol list. Symbols from the last successful download are still in use. Try again in a few minutes.";

fn fixture_text(exchange: &str) -> &'static str {
    match exchange {
        "NSE" => fixture!("NSECompactScrip.csv"),
        "BSE" => fixture!("BSECompactScrip.csv"),
        "NFO" => fixture!("NFOCompactScrip.csv"),
        "BFO" => fixture!("BFOCompactScrip.csv"),
        _ => fixture!("MCXCompactScrip.csv"),
    }
}

fn header() -> &'static str {
    fixture!("NSECompactScrip.csv").lines().next().unwrap()
}

/// The five files as the archive holds them; `overrides` replace a file's
/// rows (web `write_contract(directory, **overrides)`).
fn files_with(overrides: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
    EXCHANGES
        .iter()
        .map(|ex| {
            let text = match overrides.iter().find(|(o, _)| o == ex) {
                Some((_, rows)) => format!("{}\n{}", header(), rows),
                None => fixture_text(ex).to_string(),
            };
            (format!("{}CompactScrip.csv", ex), text.into_bytes())
        })
        .collect()
}

fn files() -> Vec<(String, Vec<u8>)> {
    files_with(&[])
}

fn rows() -> Vec<SymToken> {
    parse_archive(&files()).unwrap()
}

/// The one row on `exchange` with broker symbol `brsymbol`.
fn row<'a>(rows: &'a [SymToken], exchange: &str, brsymbol: &str) -> &'a SymToken {
    let found: Vec<&SymToken> = rows
        .iter()
        .filter(|r| r.exchange == exchange && r.brsymbol == brsymbol)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{} on {}: {} rows",
        brsymbol,
        exchange,
        found.len()
    );
    found[0]
}

fn symbols<'a>(rows: &'a [SymToken], exchange: &str) -> HashSet<&'a str> {
    rows.iter()
        .filter(|r| r.exchange == exchange)
        .map(|r| r.symbol.as_str())
        .collect()
}

/// Every row the web builds from the fixture files, column for column and
/// in its order (NSE, BSE, indices, NFO, BFO, MCX).
#[test]
fn the_contract_matches_the_web_row_for_row() {
    let golden: Value = serde_json::from_str(fixture!("master_rows.json")).unwrap();
    let want = golden["rows"].as_array().unwrap();
    let got = rows();
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want) {
        let text = |k: &str| w[k].as_str().unwrap().to_string();
        assert_eq!(g.symbol, text("symbol"), "{:?}", w);
        assert_eq!(g.brsymbol, text("brsymbol"), "{:?}", w);
        assert_eq!(g.name, text("name"), "{:?}", w);
        assert_eq!(g.exchange, text("exchange"), "{:?}", w);
        assert_eq!(g.brexchange, text("brexchange"), "{:?}", w);
        assert_eq!(g.token, text("token"), "{:?}", w);
        assert_eq!(g.expiry, text("expiry"), "{:?}", w);
        assert_eq!(g.strike, w["strike"].as_f64().unwrap(), "{:?}", w);
        assert_eq!(
            i64::from(g.lot_size),
            w["lotsize"].as_i64().unwrap(),
            "{:?}",
            w
        );
        assert_eq!(g.instrument_type, text("instrumenttype"), "{:?}", w);
        assert_eq!(g.tick_size, w["tick_size"].as_f64().unwrap(), "{:?}", w);
    }
}

// --- NSE and BSE cash ---------------------------------------------------

/// Web test_pocketful_master_contract.py::test_nse_equity_series_give_the_base_symbol
#[test]
fn nse_equity_series_give_the_base_symbol() {
    let rows = rows();
    for (brsymbol, symbol) in [
        ("RELIANCE-EQ", "RELIANCE"),
        ("M&M-EQ", "M&M"),
        // Only the series is removed, never the company's own hyphen.
        ("BAJAJ-AUTO-EQ", "BAJAJ-AUTO"),
        // The series outside EQ that a Nifty 500 stock can trade in.
        ("HFCL-BE", "HFCL"),
        ("FELDVR-BZ", "FELDVR"),
        ("EMBASSY-RR", "EMBASSY"),
        ("IRBIT-IV", "IRBIT"),
        ("DYNAMIC-SM", "DYNAMIC"),
        ("FASCINATE-ST", "FASCINATE"),
    ] {
        let found = row(&rows, "NSE", brsymbol);
        assert_eq!(found.symbol, symbol);
        assert_eq!(found.brsymbol, brsymbol);
        assert_eq!(found.instrument_type, "EQ", "{}", brsymbol);
        assert_eq!(found.brexchange, "NSE");
    }
}

/// Web test_pocketful_master_contract.py::test_nse_debt_bonds_and_partly_paid_keep_their_trading_symbol
#[test]
fn nse_debt_bonds_and_partly_paid_keep_their_trading_symbol() {
    let rows = rows();
    for brsymbol in ["664PY32-SG", "SGBAUG27-GB", "OLAPP-E1"] {
        assert_eq!(row(&rows, "NSE", brsymbol).symbol, brsymbol);
    }
}

/// Web test_pocketful_master_contract.py::test_a_series_suffix_stays_when_the_bare_name_is_taken:
/// EQ wins the bare name; two other series claiming one name both keep theirs.
#[test]
fn a_series_suffix_stays_when_the_bare_name_is_taken() {
    let nse = "900001,ACMECO-EQ,ACME CO LTD,10,,0.0000,0.0100,1,EQ,,EQ,NSE,,\n\
               900002,ACMECO-BE,ACME CO LTD,10,,0.0000,0.0100,1,BE,,BE,NSE,,\n\
               900003,ZEDCO-BE,ZED CO LTD,10,,0.0000,0.0100,1,BE,,BE,NSE,,\n\
               900004,ZEDCO-ST,ZED CO LTD,10,,0.0000,0.0500,800,ST,,ST,NSE,,\n";
    let out = parse_archive(&files_with(&[("NSE", nse)])).unwrap();
    let by_brsymbol: HashMap<&str, &str> = out
        .iter()
        .filter(|r| r.exchange == "NSE")
        .map(|r| (r.brsymbol.as_str(), r.symbol.as_str()))
        .collect();
    assert_eq!(
        by_brsymbol,
        HashMap::from([
            ("ACMECO-EQ", "ACMECO"),
            ("ACMECO-BE", "ACMECO-BE"),
            ("ZEDCO-BE", "ZEDCO-BE"),
            ("ZEDCO-ST", "ZEDCO-ST"),
        ])
    );
}

/// Web test_pocketful_master_contract.py::test_bse_group_suffix_is_removed
#[test]
fn bse_group_suffix_is_removed() {
    let rows = rows();
    for (brsymbol, symbol) in [
        ("RELIANCE-A", "RELIANCE"),
        ("M&M-A", "M&M"),
        ("BAJAJ-AUTO-A", "BAJAJ-AUTO"),
        ("HFCL-T", "HFCL"),
        ("EMBASSY-IF", "EMBASSY"),
        // A rights entitlement: only the group suffix "-R" goes.
        ("SPPL-RE1-R", "SPPL-RE1"),
        ("731PFCL36-F", "731PFCL36"),
    ] {
        let found = row(&rows, "BSE", brsymbol);
        assert_eq!(found.symbol, symbol);
        assert_eq!(found.instrument_type, "EQ", "{}", brsymbol);
    }
}

// --- indices ------------------------------------------------------------

/// Web test_pocketful_master_contract.py::test_indices_carry_the_openalgo_index_symbol
#[test]
fn indices_carry_the_openalgo_index_symbol() {
    let rows = rows();
    for (exchange, brsymbol, symbol) in [
        ("NSE_INDEX", "Nifty 50", "NIFTY"),
        ("NSE_INDEX", "Nifty Bank", "BANKNIFTY"),
        ("NSE_INDEX", "Nifty Fin Service", "FINNIFTY"),
        ("NSE_INDEX", "NIFTY MID SELECT", "MIDCPNIFTY"),
        ("NSE_INDEX", "Nifty Next 50", "NIFTYNXT50"),
        ("NSE_INDEX", "India VIX", "INDIAVIX"),
        // The NFO underlying is NIFTYFPI; the index must carry the same name.
        ("NSE_INDEX", "Nifty FPI 150", "NIFTYFPI"),
        ("NSE_INDEX", "Nifty GS 4 8Yr", "NIFTYGS48YR"),
        ("NSE_INDEX", "BHARATBOND-APR30", "BHARATBONDAPR30"),
        ("NSE_INDEX", "NIFTY100 Qualty30", "NIFTY100QUALTY30"),
        ("BSE_INDEX", "SENSEX", "SENSEX"),
        ("BSE_INDEX", "BANKEX", "BANKEX"),
        ("BSE_INDEX", "SNSX50", "SENSEX50"),
        ("BSE_INDEX", "BSE CG", "BSECAPITALGOODS"),
        ("BSE_INDEX", "MID150", "BSE150MIDCAPINDEX"),
        ("BSE_INDEX", "OILGAS", "BSEOIL&GAS"),
        ("BSE_INDEX", "AUTO", "BSEAUTO"),
        ("BSE_INDEX", "FOCIT", "BSEFOCUSEDIT"),
        // A code symbol-format.md does not list keeps its own name.
        ("BSE_INDEX", "BS1000", "BS1000"),
    ] {
        let found = row(&rows, exchange, brsymbol);
        assert_eq!(found.symbol, symbol, "{}", brsymbol);
        assert_eq!(found.instrument_type, "EQ", "{}", brsymbol);
        assert_eq!(found.brexchange, exchange.split('_').next().unwrap());
        assert_eq!(found.name, brsymbol);
    }
}

/// Web test_pocketful_master_contract.py::test_index_rows_stay_off_the_cash_exchanges
#[test]
fn index_rows_stay_off_the_cash_exchanges() {
    let rows = rows();
    let nse = symbols(&rows, "NSE");
    assert!(!nse.contains("Nifty 50") && !nse.contains("NIFTY"));
    let bse = symbols(&rows, "BSE");
    assert!(["SENSEX", "AUTO", "BSEAUTO"]
        .iter()
        .all(|s| !bse.contains(s)));
}

/// Web test_pocketful_master_contract.py::test_exchange_test_indices_are_not_loaded
#[test]
fn exchange_test_indices_are_not_loaded() {
    assert!(rows().iter().all(|r| r.brsymbol != "INDEX1 NSETEST"));
}

// --- derivatives ----------------------------------------------------------

/// Web test_pocketful_master_contract.py::test_derivatives_follow_the_openalgo_format
#[test]
fn derivatives_follow_the_openalgo_format() {
    let rows = rows();
    for (exchange, brsymbol, symbol, expiry, strike, instrument_type, name) in [
        (
            "NFO",
            "NIFTY27JANFUT",
            "NIFTY25JAN27FUT",
            "25-JAN-27",
            0.0,
            "FUT",
            "NIFTY",
        ),
        // Weekly: Pocketful writes 26D01 for 1 December 2026.
        (
            "NFO",
            "NIFTY26D0125850PE",
            "NIFTY01DEC2625850PE",
            "01-DEC-26",
            25850.0,
            "PE",
            "NIFTY",
        ),
        (
            "NFO",
            "BANKNIFTY26NOV39200PE",
            "BANKNIFTY23NOV2639200PE",
            "23-NOV-26",
            39200.0,
            "PE",
            "BANKNIFTY",
        ),
        (
            "NFO",
            "NIFTYFPI26NOVFUT",
            "NIFTYFPI23NOV26FUT",
            "23-NOV-26",
            0.0,
            "FUT",
            "NIFTYFPI",
        ),
        (
            "NFO",
            "CROMPTON26NOV192.5CE",
            "CROMPTON23NOV26192.5CE",
            "23-NOV-26",
            192.5,
            "CE",
            "CROMPTON",
        ),
        (
            "NFO",
            "M&M26NOVFUT",
            "M&M23NOV26FUT",
            "23-NOV-26",
            0.0,
            "FUT",
            "M&M",
        ),
        (
            "NFO",
            "BAJAJ-AUTO26NOVFUT",
            "BAJAJ-AUTO23NOV26FUT",
            "23-NOV-26",
            0.0,
            "FUT",
            "BAJAJ-AUTO",
        ),
        (
            "BFO",
            "SENSEX26N1975000CE",
            "SENSEX19NOV2675000CE",
            "19-NOV-26",
            75000.0,
            "CE",
            "SENSEX",
        ),
        (
            "BFO",
            "SENSEX5027JANFUT",
            "SENSEX5028JAN27FUT",
            "28-JAN-27",
            0.0,
            "FUT",
            "SENSEX50",
        ),
        (
            "BFO",
            "BANKEX27JANFUT",
            "BANKEX28JAN27FUT",
            "28-JAN-27",
            0.0,
            "FUT",
            "BANKEX",
        ),
        (
            "BFO",
            "FOCIT26NOV31100PE",
            "FOCIT26NOV2631100PE",
            "26-NOV-26",
            31100.0,
            "PE",
            "FOCIT",
        ),
        // MCX has no company_name: the underlying comes from the symbol, and
        // SILVER100 is not SILVER.
        (
            "MCX",
            "SILVER10026NOVFUT",
            "SILVER10030NOV26FUT",
            "30-NOV-26",
            0.0,
            "FUT",
            "SILVER100",
        ),
        (
            "MCX",
            "SILVER26DECFUT",
            "SILVER04DEC26FUT",
            "04-DEC-26",
            0.0,
            "FUT",
            "SILVER",
        ),
        (
            "MCX",
            "CRUDEOIL26DECFUT",
            "CRUDEOIL18DEC26FUT",
            "18-DEC-26",
            0.0,
            "FUT",
            "CRUDEOIL",
        ),
        (
            "MCX",
            "GOLD27JAN144500PE",
            "GOLD29JAN27144500PE",
            "29-JAN-27",
            144500.0,
            "PE",
            "GOLD",
        ),
        (
            "MCX",
            "MCXBULLDEX26NOV29400PE",
            "MCXBULLDEX27NOV2629400PE",
            "27-NOV-26",
            29400.0,
            "PE",
            "MCXBULLDEX",
        ),
        (
            "MCX",
            "SILVERM27APR173000PE",
            "SILVERM23APR27173000PE",
            "23-APR-27",
            173000.0,
            "PE",
            "SILVERM",
        ),
    ] {
        let found = row(&rows, exchange, brsymbol);
        assert_eq!(found.symbol, symbol, "{}", brsymbol);
        assert_eq!(found.expiry, expiry, "{}", brsymbol);
        // Futures arrive with the placeholder strike -0.01.
        assert_eq!(found.strike, strike, "{}", brsymbol);
        assert!(found.strike.is_sign_positive(), "{}: -0.0", brsymbol);
        assert_eq!(found.instrument_type, instrument_type, "{}", brsymbol);
        assert_eq!(found.name, name, "{}", brsymbol);
        assert_eq!(found.brexchange, exchange);
    }
}

/// Web test_pocketful_master_contract.py::test_mcx_spot_definitions_are_not_loaded.
/// The web's file has 7 contracts; the desktop's adds CRUDEOIL26OCTFUT,
/// SILVERM26NOV131500CE and MCXBULLDEX26OCTFUT and one more spot row (GOLD).
#[test]
fn mcx_spot_definitions_are_not_loaded() {
    let rows = rows();
    let mcx: Vec<&SymToken> = rows.iter().filter(|r| r.exchange == "MCX").collect();
    assert!(mcx
        .iter()
        .all(|r| !["SILVER", "MCXBULLDEX", "GOLD"].contains(&r.brsymbol.as_str())));
    assert_eq!(mcx.len(), 10);
}

// --- lot and tick sizes -------------------------------------------------

/// Web test_pocketful_master_contract.py::test_lot_and_tick_sizes_are_kept_as_given
#[test]
fn lot_and_tick_sizes_are_kept_as_given() {
    let rows = rows();
    for (exchange, brsymbol, lot_size, tick_size) in [
        ("NSE", "RELIANCE-EQ", 1, 0.1),
        ("NSE", "BAJAJ-AUTO-EQ", 1, 1.0),
        ("NSE", "IRBIT-IV", 25000, 0.01),
        ("NSE", "DYNAMIC-SM", 500, 0.05),
        ("BSE", "RELIANCE-A", 1, 0.05),
        ("NFO", "NIFTY27JANFUT", 65, 0.1),
        ("NFO", "CROMPTON26NOV192.5CE", 2150, 0.01),
        ("BFO", "SENSEX26N1975000CE", 20, 0.05),
        // MCX lots are already units (CRUDEOIL 100 barrels, GOLD 1 kg) and
        // ticks already rupees.
        ("MCX", "CRUDEOIL26DECFUT", 100, 1.0),
        ("MCX", "NATURALGAS26DECFUT", 1250, 0.1),
        ("MCX", "GOLD27JAN144500PE", 1, 0.5),
        ("MCX", "MCXBULLDEX26NOV29400PE", 15, 0.05),
        ("NSE_INDEX", "Nifty 50", 0, 0.0),
    ] {
        let found = row(&rows, exchange, brsymbol);
        assert_eq!(found.lot_size, lot_size, "{}", brsymbol);
        assert!((found.tick_size - tick_size).abs() < 1e-9, "{}", brsymbol);
    }
}

// --- the whole contract -------------------------------------------------

/// Web test_pocketful_master_contract.py::test_the_processed_contract_has_the_symtoken_shape
#[test]
fn the_processed_contract_has_the_symtoken_shape() {
    let rows = rows();
    let types: BTreeSet<&str> = rows.iter().map(|r| r.instrument_type.as_str()).collect();
    assert_eq!(types, BTreeSet::from(["CE", "EQ", "FUT", "PE"]));
    let exchanges: BTreeSet<&str> = rows.iter().map(|r| r.exchange.as_str()).collect();
    assert_eq!(
        exchanges,
        BTreeSet::from(["BFO", "BSE", "BSE_INDEX", "MCX", "NFO", "NSE", "NSE_INDEX"])
    );
    let by_symbol: HashSet<(&str, &str)> = rows
        .iter()
        .map(|r| (r.symbol.as_str(), r.exchange.as_str()))
        .collect();
    assert_eq!(by_symbol.len(), rows.len(), "a repeated (symbol, exchange)");
    let by_token: HashSet<(&str, &str)> = rows
        .iter()
        .map(|r| (r.token.as_str(), r.exchange.as_str()))
        .collect();
    assert_eq!(by_token.len(), rows.len(), "a repeated (token, exchange)");
    assert!(rows.iter().all(|r| r.strike >= 0.0));
}

/// Web test_pocketful_master_contract.py::test_a_repeated_symbol_on_one_exchange_keeps_only_the_first_row:
/// two contracts that build one symbol would make the token lookup ambiguous.
#[test]
fn a_repeated_symbol_on_one_exchange_keeps_only_the_first_row() {
    let nfo = format!(
        "{}99999,NIFTY27JANFUT-X,NIFTY,0,25-Jan-2027,-0.0100,0.1000,65,FUTIDX,XX,XX,NFO,,\n",
        fixture!("NFOCompactScrip.csv").split_once('\n').unwrap().1
    );
    let out = parse_archive(&files_with(&[("NFO", &nfo)])).unwrap();
    let futures: Vec<&str> = out
        .iter()
        .filter(|r| r.exchange == "NFO" && r.symbol == "NIFTY25JAN27FUT")
        .map(|r| r.token.as_str())
        .collect();
    assert_eq!(futures, ["54749"]);
}

/// Web test_pocketful_master_contract.py::test_a_file_without_an_expected_column_is_refused.
/// The trader reads the web's failure sentence; the log names the column.
#[test]
fn a_file_without_an_expected_column_is_refused() {
    let (logs, _guard) = capture();
    let mut files = files();
    let nfo = fixture!("NFOCompactScrip.csv").replace("option_type", "opt_type");
    files[2].1 = nfo.into_bytes();
    let err = parse_archive(&files).unwrap_err();
    assert_eq!(err.client_message(), FAILED);
    let logs = logs.text();
    assert!(
        logs.contains("NFOCompactScrip.csv has no column option_type"),
        "{}",
        logs
    );
}

/// Web test_pocketful_master_contract.py::test_rows_sharing_a_token_on_two_exchanges_are_all_stored:
/// NSE ABB and the BSE index AUTO are both token 13; BSE KARURVYSYA and an
/// MCX SILVERM option are both 590003. Each is its own instrument, in the
/// symtoken table and in the resolver loaded from it.
#[test]
fn rows_sharing_a_token_on_two_exchanges_are_all_stored() {
    let rows = rows();
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    migrations::run_migrations(&conn).unwrap();
    symbol::store_symbols(&mut conn, "pocketful", chrono::Utc::now(), &rows).unwrap();
    let stored = symbol::load_symbols(&conn).unwrap();
    assert_eq!(stored.len(), rows.len());
    let by_key: HashMap<(&str, &str), &str> = stored
        .iter()
        .map(|r| ((r.exchange.as_str(), r.token.as_str()), r.symbol.as_str()))
        .collect();
    let resolver = SymbolResolver::new();
    assert_eq!(resolver.load(stored.clone()), rows.len());
    for (exchange, token, symbol) in [
        ("NSE", "13", "ABB"),
        ("BSE_INDEX", "13", "BSEAUTO"),
        ("BSE", "590003", "KARURVYSYA"),
        ("MCX", "590003", "SILVERM23APR27173000PE"),
    ] {
        assert_eq!(by_key[&(exchange, token)], symbol);
        assert_eq!(resolver.by_token(exchange, token).unwrap().symbol, symbol);
    }
}

// --- desktop rules (MC-02 and the resolver's token rule) -----------------

/// MC-02: every one of the five files is required and must yield
/// instruments, so a missing, empty or instrument-less file refuses the
/// archive by name and the stored master is kept rather than replaced by a
/// partial one (the web refuses only an empty whole).
#[test]
fn every_file_must_yield_instruments() {
    for (i, exchange) in EXCHANGES.iter().enumerate() {
        let mut fs = files();
        fs.remove(i);
        let msg = parse_archive(&fs).unwrap_err().client_message();
        assert!(
            msg.contains(&format!("Pocketful {} instrument list", exchange)),
            "{}",
            msg
        );
        assert!(msg.contains("existing symbols were kept"), "{}", msg);
    }
    let mut empty = files();
    empty[4].1 = b"  \n".to_vec();
    let msg = parse_archive(&empty).unwrap_err().client_message();
    assert!(msg.contains("Pocketful MCX instrument list"), "{}", msg);
    let header_only = parse_archive(&files_with(&[("NFO", "")])).unwrap_err();
    assert!(header_only
        .client_message()
        .contains("Pocketful NFO instrument list"));
    // MCX spot rows alone are no instruments.
    let spot = "115,SILVER,,0,01-Jan-0001,0.0000,1.0000,30,COM,,XX,MCX,,\n";
    let msg = parse_archive(&files_with(&[("MCX", spot)]))
        .unwrap_err()
        .client_message();
    assert!(msg.contains("Pocketful MCX instrument list"), "{}", msg);
}

/// A repeated `(exchange, token)` keeps its first row, as the resolver does
/// with what it loads, so the stored and the loaded master agree.
#[test]
fn a_repeated_token_on_one_exchange_keeps_the_first_row() {
    let nse = format!(
        "{}3045,SBIN2-EQ,DUP,0,,0.0000,0.0500,1,EQ,,EQ,NSE,,\n",
        fixture!("NSECompactScrip.csv").split_once('\n').unwrap().1
    );
    let out = parse_archive(&files_with(&[("NSE", &nse)])).unwrap();
    assert_eq!(out.len(), rows().len());
    assert!(out.iter().all(|r| r.symbol != "SBIN2"));
    assert_eq!(row(&out, "NSE", "SBIN-EQ").token, "3045");
}

/// web `pd.to_datetime(expiry, format="%d-%b-%Y", errors="coerce")`: a
/// contract whose expiry is in another form is skipped and logged, never
/// given a date read some other way.
#[test]
fn a_contract_with_an_unreadable_expiry_is_skipped() {
    let (logs, _guard) = capture();
    let nfo = format!(
        "{}99998,NIFTY26NOVFUT,NIFTY,0,2026-11-24,-0.0100,0.1000,65,FUTIDX,XX,XX,NFO,,\n",
        fixture!("NFOCompactScrip.csv").split_once('\n').unwrap().1
    );
    let out = parse_archive(&files_with(&[("NFO", &nfo)])).unwrap();
    assert!(out.iter().all(|r| r.token != "99998"));
    assert_eq!(out.len(), rows().len());
    let logs = logs.text();
    assert!(
        logs.contains("Skipping 1 NFO contracts with no readable expiry"),
        "{}",
        logs
    );
}

// --- download -----------------------------------------------------------

/// A broker on `fake` whose contract address is the documented one there.
fn contract_broker(fake: &Fake) -> PocketfulBroker {
    PocketfulBroker::with_base_url(
        SymbolResolver::new(),
        &fake.base,
        "ws://127.0.0.1:9",
        MASTER_URL.replace(BASE_URL, &fake.base),
    )
}

/// The archive of `members`, as Pocketful's `CompactFile.zip` holds them.
fn archive(members: &[(String, Vec<u8>)]) -> Vec<u8> {
    let refs: Vec<(&str, &[u8])> = members
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    zip::build(&refs)
}

/// Answer the contract path with `reply` and download through it.
async fn download_with(reply: Reply) -> (Fake, crate::error::Result<Vec<SymToken>>) {
    let fake = Fake::start().await;
    fake.on("/api/v1/contract/Compact", vec![reply]);
    let got = contract_broker(&fake)
        .download_master_contract(&session())
        .await;
    (fake, got)
}

const SECRET_TOKEN: &str = "SENTINEL-pf-access-7Qz";
const SECRET_ACCOUNT: &str = "SENTINELPF42";

fn session() -> AuthToken {
    AuthToken::new(SECRET_TOKEN).with_user_id(SECRET_ACCOUNT)
}

/// Web test_pocketful_master_contract.py::test_the_download_asks_for_the_documented_file_and_sends_no_credentials,
/// and ::test_a_successful_download_replaces_the_contract. The web test's
/// timeout check is `the_download_waits_the_webs_180_seconds`.
#[tokio::test]
async fn the_download_asks_for_the_documented_file_and_sends_no_credentials() {
    assert_eq!(
        MASTER_URL,
        "https://algo.pocketful.in/api/v1/contract/Compact?info=download&exchanges=NSE,NFO,BSE,BFO,MCX"
    );
    let (fake, got) = download_with(Reply::Binary(200, archive(&files()))).await;
    let got = got.unwrap();

    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    let call = &seen[0];
    assert_eq!(call.method, "GET");
    assert_eq!(call.path, "/api/v1/contract/Compact");
    // The commas reach the server as the page has them.
    assert_eq!(call.query, "info=download&exchanges=NSE,NFO,BSE,BFO,MCX");
    // Public file: no token, no account, nothing else naming the trader.
    for header in [
        "authorization",
        "x-authorization-token",
        "clientid",
        "cookie",
    ] {
        assert_eq!(call.header(header), "", "{}", header);
    }
    let sent = format!("{:?}{}{}", call.headers, call.query, call.body);
    assert!(!sent.contains(SECRET_TOKEN) && !sent.contains(SECRET_ACCOUNT));

    assert_eq!(got, rows());
    assert!(["NIFTY", "BANKNIFTY"]
        .iter()
        .all(|s| symbols(&got, "NSE_INDEX").contains(s)));
    assert!(symbols(&got, "NSE").contains("HFCL"));
}

/// Web test_pocketful_master_contract.py::test_the_download_asks_for_the_documented_file_and_sends_no_credentials,
/// its timeout (`assert kwargs["timeout"] == mcd.DOWNLOAD_TIMEOUT`, 180 s):
/// the archive is about 4 MB compressed, and the shared client's 30 s
/// would fail it on a slow link. The GET carries no header of its own.
#[tokio::test]
async fn the_download_waits_the_webs_180_seconds() {
    let broker = PocketfulBroker::new(SymbolResolver::new());
    let request = contract_request(&broker).build().unwrap();
    assert_eq!(request.timeout(), Some(&Duration::from_secs(180)));
    assert_eq!(request.method(), reqwest::Method::GET);
    assert_eq!(request.url().as_str(), MASTER_URL);
    assert!(request.headers().is_empty(), "{:?}", request.headers());
}

/// Web test_pocketful_master_contract.py::test_the_download_writes_only_the_five_contract_files:
/// the desktop writes no file at all (the archive is read in memory), and
/// members besides the five, a path outside the folder among them, change
/// nothing.
#[tokio::test]
async fn members_besides_the_five_files_change_nothing() {
    let mut members = files();
    members.push(("../outside.csv".into(), b"x".to_vec()));
    members.push(("extra/notes.txt".into(), b"y".to_vec()));
    let (_fake, got) = download_with(Reply::Binary(200, archive(&members))).await;
    assert_eq!(got.unwrap(), rows());
}

/// Web test_pocketful_master_contract.py::test_an_archive_missing_a_file_is_refused
#[tokio::test]
async fn an_archive_missing_a_file_is_refused() {
    let mut members = files();
    members.retain(|(n, _)| n != "MCXCompactScrip.csv");
    let (_fake, got) = download_with(Reply::Binary(200, archive(&members))).await;
    assert!(got
        .unwrap_err()
        .client_message()
        .contains("Pocketful MCX instrument list"));
}

/// Web test_pocketful_master_contract.py::test_an_http_error_is_raised and
/// ::test_a_failed_download_keeps_the_stored_contract_and_says_so: an error
/// status, a refused connection, an answer cut off and an archive that does
/// not read all answer the web's sentence (the service keeps the stored
/// master on any error), and the cause reaches the log only.
#[tokio::test]
async fn a_failed_download_answers_the_webs_sentence() {
    let (logs, _guard) = capture();
    for reply in [
        Reply::Text(503, "busy"),
        Reply::Cut,
        Reply::Binary(200, b"not an archive".to_vec()),
    ] {
        let (_fake, got) = download_with(reply.clone()).await;
        assert_eq!(got.unwrap_err().client_message(), FAILED, "{:?}", reply);
    }
    let closed = PocketfulBroker::with_base_url(
        SymbolResolver::new(),
        "http://127.0.0.1:1",
        "ws://127.0.0.1:1",
        "http://127.0.0.1:1/api/v1/contract/Compact?info=download",
    );
    let err = closed
        .download_master_contract(&session())
        .await
        .unwrap_err();
    assert_eq!(err.client_message(), FAILED);
    let logs = logs.text();
    assert!(logs.contains("HTTP 503"), "{}", logs);
    assert!(
        logs.contains("Pocketful master contract download failed"),
        "{}",
        logs
    );
}
