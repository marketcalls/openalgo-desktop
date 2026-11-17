//! Web `test/test_zerodha_series_symbols.py` (openalgo#2463, `6716dd6f2`):
//! Zerodha NSE rows traded outside the EQ series get OpenAlgo's bare symbol.
//!
//! Kite's instrument list names a stock in the BE series HFCL-BE and a REIT
//! EMBASSY-RR, and the master stored those as the OpenAlgo symbol. OpenAlgo's
//! equity symbol is the base symbol (`docs/prompt/symbol-format.md`), which
//! is how NSE lists them (Symbol HFCL, Series BE), so a Nifty 500 list
//! pasted into Historify as SYMBOL,NSE found 495 of its 500 members.
//!
//! The rows go through the real `parse_instruments`, as the web's go
//! through `process_zerodha_csv`.

use super::*;

const KITE_HEADER: &str = "instrument_token,exchange_token,tradingsymbol,name,last_price,\
                           expiry,strike,tick_size,lot_size,instrument_type,segment,exchange";

/// Web `_row`: an NSE cash row unless the exchange says otherwise.
fn row(token: u32, tradingsymbol: &str, name: &str, exchange: &str) -> String {
    format!("{token},{token},{tradingsymbol},{name},0,,0,0.05,1,EQ,{exchange},{exchange}")
}

/// Web `ROWS`.
fn kite_csv() -> String {
    let nse = |t, s, n| row(t, s, n, "NSE");
    let rows = [
        nse(1, "RELIANCE", "RELIANCE INDUSTRIES"),
        nse(2, "BAJAJ-AUTO", "BAJAJ AUTO"),
        nse(3, "HFCL-BE", "HFCL"),
        nse(4, "STLTECH-BE", "STERLITE TECHNOLOGIES"),
        nse(5, "EMBASSY-RR", "EMBASSY OFFICE PARKS REIT"),
        nse(6, "PGINVIT-IV", "POWERGRID INFRA INVESTMENT TRUST - INVIT"),
        nse(7, "SANWARIA-BZ", "SANWARIA CONSUMER"),
        nse(8, "OMFURN-SM", "OMFURN INDIA"),
        nse(9, "GOLDSTAR-ST", "GOLDSTAR POWER"),
        nse(10, "IL&FSENGG-BZ", "IL&FS ENG AND CONS CO"),
        // Debt and government securities keep their suffix.
        nse(11, "656KA30-SG", "SDL KA 6.56% 2030"),
        nse(12, "622GS2035-GS", "GOI LOAN 6.22% 2035"),
        // The bare name is another row's symbol, so -RL is kept.
        nse(13, "HDFCSIINAV", "HDFCSIINAV"),
        nse(14, "HDFCSIINAV-RL", "HDFCSIINAV-RL"),
        // Two suffixed rows would claim one bare name: both are kept.
        nse(15, "TWOSER-BE", "TWO SERIES"),
        nse(16, "TWOSER-BZ", "TWO SERIES"),
        // A bare name that is already an EQ symbol keeps the suffixed row apart.
        nse(17, "DUAL", "DUAL LTD"),
        nse(18, "DUAL-BE", "DUAL LTD"),
        // The same shape on BSE is not an NSE series and is untouched.
        row(19, "RAMAPPR-B", "RAMA PAPER MILLS", "BSE"),
        row(20, "XYZ-BE", "XYZ", "BSE"),
    ];
    format!("{KITE_HEADER}\n{}\n", rows.join("\n"))
}

/// Web fixture `processed`: `(brsymbol, exchange) -> symbol`.
fn processed() -> HashMap<(String, String), String> {
    let rows = parse_instruments(&kite_csv()).unwrap();
    assert_eq!(rows.len(), 20, "every Kite row is kept");
    rows.into_iter()
        .map(|r| ((r.brsymbol, r.exchange), r.symbol))
        .collect()
}

fn symbol_of(p: &HashMap<(String, String), String>, brsymbol: &str, exchange: &str) -> String {
    p.get(&(brsymbol.to_string(), exchange.to_string()))
        .unwrap_or_else(|| panic!("no row {brsymbol} on {exchange}"))
        .clone()
}

/// Web test/test_zerodha_series_symbols.py::test_series_suffix_leaves_the_openalgo_symbol_and_stays_on_brsymbol.
#[test]
fn series_suffix_leaves_the_openalgo_symbol_and_stays_on_brsymbol() {
    let p = processed();
    for (brsymbol, symbol) in [
        ("HFCL-BE", "HFCL"),
        ("STLTECH-BE", "STLTECH"),
        ("EMBASSY-RR", "EMBASSY"),
        ("PGINVIT-IV", "PGINVIT"),
        ("SANWARIA-BZ", "SANWARIA"),
        ("OMFURN-SM", "OMFURN"),
        ("GOLDSTAR-ST", "GOLDSTAR"),
        ("IL&FSENGG-BZ", "IL&FSENGG"),
    ] {
        assert_eq!(symbol_of(&p, brsymbol, "NSE"), symbol, "{brsymbol}");
    }
}

/// Web test/test_zerodha_series_symbols.py::test_other_rows_keep_the_tradingsymbol_as_their_symbol.
#[test]
fn other_rows_keep_the_tradingsymbol_as_their_symbol() {
    let p = processed();
    for (brsymbol, exchange) in [
        ("RELIANCE", "NSE"),
        ("BAJAJ-AUTO", "NSE"),
        ("656KA30-SG", "NSE"),
        ("622GS2035-GS", "NSE"),
        ("HDFCSIINAV", "NSE"),
        ("HDFCSIINAV-RL", "NSE"),
        ("TWOSER-BE", "NSE"),
        ("TWOSER-BZ", "NSE"),
        ("DUAL", "NSE"),
        ("DUAL-BE", "NSE"),
        ("RAMAPPR-B", "BSE"),
        ("XYZ-BE", "BSE"),
    ] {
        assert_eq!(symbol_of(&p, brsymbol, exchange), brsymbol, "{brsymbol}");
    }
}

/// Web test/test_zerodha_series_symbols.py::test_no_two_nse_rows_share_a_symbol.
#[test]
fn no_two_nse_rows_share_a_symbol() {
    let p = processed();
    let nse: Vec<&String> = p
        .iter()
        .filter(|((_, exchange), _)| exchange == "NSE")
        .map(|(_, symbol)| symbol)
        .collect();
    let unique: std::collections::HashSet<&String> = nse.iter().copied().collect();
    assert_eq!(nse.len(), unique.len(), "{nse:?}");
}

/// Web test/test_zerodha_series_symbols.py::test_holdings_report_the_openalgo_symbol.
/// The web stubs `get_oa_symbol`; here the resolver is the master the rows
/// above built, so the holding reaches HFCL only through the series strip.
#[test]
fn holdings_report_the_openalgo_symbol() {
    let r = SymbolResolver::new();
    r.load(parse_instruments(&kite_csv()).unwrap());
    let rows: Vec<KiteHolding> = serde_json::from_value(serde_json::json!([
        {"tradingsymbol": "HFCL-BE", "exchange": "NSE", "product": "CNC"},
        {"tradingsymbol": "RELIANCE", "exchange": "NSE", "product": "CNC"},
        // Not in the master contract: Kite's symbol is kept, not blanked.
        {"tradingsymbol": "DELISTED", "exchange": "BSE", "product": "CNC"},
    ]))
    .unwrap();
    let symbols: Vec<String> = map_holdings(rows, &r)
        .into_iter()
        .map(|h| h.symbol)
        .collect();
    assert_eq!(symbols, ["HFCL", "RELIANCE", "DELISTED"]);
    // Orders still go to Kite under its own tradingsymbol.
    assert_eq!(r.br_symbol("HFCL", "NSE").as_deref(), Some("HFCL-BE"));
}
