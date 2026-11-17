//! Shared master-contract helpers: OpenAlgo expiry and strike formatting and
//! symbol construction (`docs/prompt/symbol-format.md`).
//!
//! * expiry column: `DD-MMM-YY` uppercase (`28-MAR-24`)
//! * future:  `[base][DDMMMYY]FUT`              (`BANKNIFTY24APR24FUT`)
//! * option:  `[base][DDMMMYY][strike][CE|PE]`  (`VEDL25APR24292.5CE`)

use chrono::NaiveDate;

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

/// The refusal of a master contract download that is missing part of the
/// broker's instruments (MC-02): the download fails as a whole, so the
/// stored master is kept and the next download tries again.
pub fn part_missing(broker: &str, part: &str) -> crate::error::AppError {
    crate::error::AppError::Broker(format!(
        "The {} {} instrument list could not be downloaded. Your existing symbols were kept; try the download again later.",
        broker, part
    ))
}

/// `2024-03-28` -> `28-MAR-24`.
pub fn format_expiry(date: NaiveDate) -> String {
    use chrono::Datelike;
    format!(
        "{:02}-{}-{:02}",
        date.day(),
        MONTHS[date.month0() as usize],
        date.year() % 100
    )
}

/// Parse an OpenAlgo `DD-MMM-YY` expiry (case-insensitive month).
pub fn parse_oa_expiry(s: &str) -> Option<NaiveDate> {
    let mut parts = s.trim().split('-');
    let (d, m, y) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let day: u32 = d.parse().ok()?;
    let mon = MONTHS.iter().position(|x| x.eq_ignore_ascii_case(m))? as u32 + 1;
    let yy: i32 = y.parse().ok()?;
    let year = if y.len() == 4 { yy } else { 2000 + yy };
    NaiveDate::from_ymd_opt(year, mon, day)
}

/// Parse the common broker expiry encodings into a date:
/// `2024-03-28`, `2024-03-28 00:00:00`, `28MAR2024`, `28-Mar-2024`,
/// `28-MAR-24`, `28MAR24`.
pub fn parse_broker_expiry(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let head = s.split([' ', 'T']).next().unwrap_or(s);
    if let Ok(d) = NaiveDate::parse_from_str(head, "%Y-%m-%d") {
        return Some(d);
    }
    if let Some(d) = parse_oa_expiry(s) {
        return Some(d);
    }
    let upper = s.to_ascii_uppercase();
    for fmt in ["%d%b%Y", "%d-%b-%Y", "%d%b%y"] {
        if let Ok(d) = NaiveDate::parse_from_str(&upper, fmt) {
            return Some(d);
        }
    }
    None
}

/// Normalise any broker expiry string to `DD-MMM-YY`; unparsable input is
/// returned uppercased (the web keeps the original on parse failure).
pub fn normalise_expiry(s: &str) -> String {
    match parse_broker_expiry(s) {
        Some(d) => format_expiry(d),
        None => s.trim().to_ascii_uppercase(),
    }
}

/// `28-MAR-24` -> `28MAR24` (the date part of a derivative symbol).
pub fn expiry_compact(expiry: &str) -> String {
    expiry.replace('-', "")
}

/// Strike text in a symbol: whole numbers lose the fraction (`190.0` ->
/// `190`), others keep it (`187.5`, `292.55`).
pub fn format_strike(strike: f64) -> String {
    if strike.is_finite() && strike.fract() == 0.0 && strike.abs() < 1e15 {
        format!("{}", strike as i64)
    } else {
        // Shortest round-trip representation, like Python's str(float).
        let s = format!("{}", strike);
        s
    }
}

/// `BANKNIFTY` + `24-APR-24` -> `BANKNIFTY24APR24FUT`.
pub fn future_symbol(base: &str, expiry: &str) -> String {
    format!("{}{}FUT", base, expiry_compact(expiry))
}

/// `NIFTY` + `28-MAR-24` + 20800 + `CE` -> `NIFTY28MAR2420800CE`.
pub fn option_symbol(base: &str, expiry: &str, strike: f64, option_type: &str) -> String {
    format!(
        "{}{}{}{}",
        base,
        expiry_compact(expiry),
        format_strike(strike),
        option_type
    )
}

/// NSE index names as brokers ship them -> OpenAlgo `NSE_INDEX` symbols
/// (web `broker/zerodha/database/master_contract_db.py`).
pub const NSE_INDEX_RENAMES: &[(&str, &str)] = &[
    ("NIFTY 50", "NIFTY"),
    ("NIFTY NEXT 50", "NIFTYNXT50"),
    ("NIFTY FIN SERVICE", "FINNIFTY"),
    ("NIFTY BANK", "BANKNIFTY"),
    ("NIFTY MID SELECT", "MIDCPNIFTY"),
    ("INDIA VIX", "INDIAVIX"),
    ("HANGSENG BEES NAV", "HANGSENGBEESNAV"),
    ("NIFTY 100", "NIFTY100"),
    ("NIFTY 200", "NIFTY200"),
    ("NIFTY 500", "NIFTY500"),
    ("NIFTY ALPHA 50", "NIFTYALPHA50"),
    ("NIFTY AUTO", "NIFTYAUTO"),
    ("NIFTY COMMODITIES", "NIFTYCOMMODITIES"),
    ("NIFTY CONSUMPTION", "NIFTYCONSUMPTION"),
    ("NIFTY CPSE", "NIFTYCPSE"),
    ("NIFTY DIV OPPS 50", "NIFTYDIVOPPS50"),
    ("NIFTY ENERGY", "NIFTYENERGY"),
    ("NIFTY FMCG", "NIFTYFMCG"),
    ("NIFTY GROWSECT 15", "NIFTYGROWSECT15"),
    ("NIFTY INFRA", "NIFTYINFRA"),
    ("NIFTY IT", "NIFTYIT"),
    ("NIFTY MEDIA", "NIFTYMEDIA"),
    ("NIFTY METAL", "NIFTYMETAL"),
    ("NIFTY MNC", "NIFTYMNC"),
    ("NIFTY PHARMA", "NIFTYPHARMA"),
    ("NIFTY PSE", "NIFTYPSE"),
    ("NIFTY PSU BANK", "NIFTYPSUBANK"),
    ("NIFTY PVT BANK", "NIFTYPVTBANK"),
    ("NIFTY REALTY", "NIFTYREALTY"),
    ("NIFTY SERV SECTOR", "NIFTYSERVSECTOR"),
    ("NIFTY MID LIQ 15", "NIFTYMIDLIQ15"),
    ("NIFTY MIDCAP 50", "NIFTYMIDCAP50"),
    ("NIFTY MIDCAP 100", "NIFTYMIDCAP100"),
    ("NIFTY MIDCAP 150", "NIFTYMIDCAP150"),
    ("NIFTY MIDSML 400", "NIFTYMIDSML400"),
    ("NIFTY SMLCAP 50", "NIFTYSMLCAP50"),
    ("NIFTY SMLCAP 100", "NIFTYSMLCAP100"),
    ("NIFTY SMLCAP 250", "NIFTYSMLCAP250"),
    ("NIFTY100 EQL WGT", "NIFTY100EQLWGT"),
    ("NIFTY100 LIQ 15", "NIFTY100LIQ15"),
    ("NIFTY100 LOWVOL30", "NIFTY100LOWVOL30"),
    ("NIFTY100 QUALTY30", "NIFTY100QUALTY30"),
    ("NIFTY200 QUALTY30", "NIFTY200QUALTY30"),
    ("NIFTY50 DIV POINT", "NIFTY50DIVPOINT"),
    ("NIFTY50 EQL WGT", "NIFTY50EQLWGT"),
    ("NIFTY50 PR 1X INV", "NIFTY50PR1XINV"),
    ("NIFTY50 PR 2X LEV", "NIFTY50PR2XLEV"),
    ("NIFTY50 TR 1X INV", "NIFTY50TR1XINV"),
    ("NIFTY50 TR 2X LEV", "NIFTY50TR2XLEV"),
    ("NIFTY50 VALUE 20", "NIFTY50VALUE20"),
    ("NIFTY GS 10YR", "NIFTYGS10YR"),
    ("NIFTY GS 10YR CLN", "NIFTYGS10YRCLN"),
    ("NIFTY GS 11 15YR", "NIFTYGS1115YR"),
    ("NIFTY GS 15YRPLUS", "NIFTYGS15YRPLUS"),
    ("NIFTY GS 4 8YR", "NIFTYGS48YR"),
    ("NIFTY GS 8 13YR", "NIFTYGS813YR"),
    ("NIFTY GS COMPSITE", "NIFTYGSCOMPSITE"),
];

/// BSE index short names -> OpenAlgo `BSE_INDEX` symbols. Applied only to
/// `BSE_INDEX` rows: equities share short names such as `AUTO`, `METAL`.
pub const BSE_INDEX_RENAMES: &[(&str, &str)] = &[
    ("SNSX50", "SENSEX50"),
    ("SNXT50", "BSESENSEXNEXT50"),
    ("MID150", "BSE150MIDCAPINDEX"),
    ("LMI250", "BSE250LARGEMIDCAPINDEX"),
    ("MSL400", "BSE400MIDSMALLCAPINDEX"),
    ("AUTO", "BSEAUTO"),
    ("BSE CG", "BSECAPITALGOODS"),
    ("CARBON", "BSECARBONEX"),
    ("BSE CD", "BSECONSUMERDURABLES"),
    ("CPSE", "BSECPSE"),
    ("DOL100", "BSEDOLLEX100"),
    ("DOL200", "BSEDOLLEX200"),
    ("DOL30", "BSEDOLLEX30"),
    ("ENERGY", "BSEENERGY"),
    ("BSEFMC", "BSEFASTMOVINGCONSUMERGOODS"),
    ("FINSER", "BSEFINANCIALSERVICES"),
    ("GREENX", "BSEGREENEX"),
    ("BSE HC", "BSEHEALTHCARE"),
    ("INFRA", "BSEINDIAINFRASTRUCTUREINDEX"),
    ("INDSTR", "BSEINDUSTRIALS"),
    ("BSE IT", "BSEINFORMATIONTECHNOLOGY"),
    ("BSEIPO", "BSEIPO"),
    ("LRGCAP", "BSELARGECAP"),
    ("METAL", "BSEMETAL"),
    ("MIDCAP", "BSEMIDCAP"),
    ("MIDSEL", "BSEMIDCAPSELECTINDEX"),
    ("OILGAS", "BSEOIL&GAS"),
    ("POWER", "BSEPOWER"),
    ("BSEPSU", "BSEPSU"),
    ("REALTY", "BSEREALTY"),
    ("SMLCAP", "BSESMALLCAP"),
    ("SMLSEL", "BSESMALLCAPSELECTINDEX"),
    ("SMEIPO", "BSESMEIPO"),
    ("TECK", "BSETECK"),
    ("TELCOM", "BSETELECOM"),
];

/// Split one CSV record (RFC 4180 quoting: `"a, b"` and `""` escapes), so a
/// quoted instrument name containing a comma does not shift the columns.
pub fn split_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = line.trim_end_matches(['\r', '\n']).chars().peekable();
    while let Some(c) = chars.next() {
        match (c, in_quotes) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', true) => in_quotes = false,
            ('"', false) if field.is_empty() => in_quotes = true,
            (',', false) => out.push(std::mem::take(&mut field)),
            (c, _) => field.push(c),
        }
    }
    out.push(field);
    out
}

/// Column positions resolved from a CSV header, so parsing keys on names
/// (like `pd.read_csv`) rather than on fixed indices.
#[derive(Debug, Clone)]
pub struct CsvHeader {
    names: Vec<String>,
}

impl CsvHeader {
    pub fn parse(line: &str) -> Self {
        Self {
            names: split_csv_line(line)
                .into_iter()
                .map(|s| s.trim().trim_start_matches('\u{feff}').to_string())
                .collect(),
        }
    }

    /// Index of a required column.
    pub fn index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }
}

/// Look a name up in a rename table.
pub fn rename<'a>(table: &'a [(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    table
        .iter()
        .find(|(from, _)| *from == name)
        .map(|(_, to)| *to)
}

/// NSE series in which a company's own units trade outside EQ (web
/// `_NSE_SERIES_SUFFIXES`, openalgo#2463). Brokers fold the series into the
/// tradingsymbol of these (Kite's HFCL-BE, EMBASSY-RR, GOLDSTAR-ST), while
/// OpenAlgo's equity symbol is the bare exchange symbol, as NSE lists it
/// (Symbol HFCL, Series BE); `brsymbol` keeps the broker's tradingsymbol.
/// Not stripped: the debt and government security series (SG, GS, TB, N0
/// and the like), and -RL, whose bare names exist as rows of their own.
const NSE_SERIES_SUFFIXES: [&str; 6] = ["BE", "BZ", "RR", "IV", "SM", "ST"];

/// The base symbol of a series-suffixed tradingsymbol (`HFCL` of
/// `HFCL-BE`), as the web's `^(.+)-(?:BE|BZ|RR|IV|SM|ST)$` reads it.
pub fn series_base(symbol: &str) -> Option<&str> {
    let (base, series) = symbol.rsplit_once('-')?;
    (!base.is_empty() && NSE_SERIES_SUFFIXES.contains(&series)).then_some(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_round_trip() {
        let d = NaiveDate::from_ymd_opt(2024, 3, 28).unwrap();
        assert_eq!(format_expiry(d), "28-MAR-24");
        assert_eq!(parse_oa_expiry("28-MAR-24"), Some(d));
        assert_eq!(parse_oa_expiry("28-mar-2024"), Some(d));
        assert_eq!(parse_oa_expiry("28MAR24"), None);
        assert_eq!(
            format_expiry(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()),
            "05-JAN-26"
        );
    }

    #[test]
    fn broker_expiry_encodings() {
        for s in [
            "2024-03-28",
            "2024-03-28 00:00:00",
            "28MAR2024",
            "28-Mar-2024",
            "28-MAR-24",
            "28Mar24",
        ] {
            assert_eq!(normalise_expiry(s), "28-MAR-24", "{}", s);
        }
        assert_eq!(normalise_expiry("garbage"), "GARBAGE");
        assert!(parse_broker_expiry("").is_none());
    }

    #[test]
    fn strike_text() {
        assert_eq!(format_strike(190.0), "190");
        assert_eq!(format_strike(187.5), "187.5");
        assert_eq!(format_strike(292.55), "292.55");
        assert_eq!(format_strike(20800.0), "20800");
        assert_eq!(format_strike(83.25), "83.25");
    }

    #[test]
    fn symbols_per_symbol_format_doc() {
        assert_eq!(
            future_symbol("BANKNIFTY", "24-APR-24"),
            "BANKNIFTY24APR24FUT"
        );
        assert_eq!(future_symbol("USDINR", "10-MAY-24"), "USDINR10MAY24FUT");
        assert_eq!(
            option_symbol("NIFTY", "28-MAR-24", 20800.0, "CE"),
            "NIFTY28MAR2420800CE"
        );
        assert_eq!(
            option_symbol("VEDL", "25-APR-24", 292.5, "CE"),
            "VEDL25APR24292.5CE"
        );
        assert_eq!(
            option_symbol("726GS2032", "25-APR-24", 97.0, "PE"),
            "726GS203225APR2497PE"
        );
    }

    #[test]
    fn csv_quoting() {
        assert_eq!(split_csv_line("a,b,,c"), ["a", "b", "", "c"]);
        assert_eq!(
            split_csv_line("1,\"TATA STEEL, LTD\",x\r\n"),
            ["1", "TATA STEEL, LTD", "x"]
        );
        assert_eq!(split_csv_line("\"say \"\"hi\"\"\",2"), ["say \"hi\"", "2"]);
        let h = CsvHeader::parse("\u{feff}instrument_token,exchange_token,tradingsymbol");
        assert_eq!(h.index("instrument_token"), Some(0));
        assert_eq!(h.index("tradingsymbol"), Some(2));
        assert_eq!(h.index("nope"), None);
    }

    #[test]
    fn rename_tables() {
        assert_eq!(rename(NSE_INDEX_RENAMES, "NIFTY 50"), Some("NIFTY"));
        assert_eq!(rename(NSE_INDEX_RENAMES, "NIFTY BANK"), Some("BANKNIFTY"));
        assert_eq!(rename(BSE_INDEX_RENAMES, "AUTO"), Some("BSEAUTO"));
        assert_eq!(rename(BSE_INDEX_RENAMES, "SENSEX"), None);
    }
}
