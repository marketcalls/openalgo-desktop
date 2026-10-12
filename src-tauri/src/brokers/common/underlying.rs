//! Which underlying a derivative belongs to, and whether it is a future, a
//! call or a put, read from its canonical OpenAlgo symbol (web
//! `extract_underlying_from_symbol`).
//!
//! The master's `name` column is not an identity. Brokers fill it
//! differently: most store the bare underlying (`NIFTY`), Fyers stores the
//! contract description (`NIFTY 06 Oct 26 22400 PE`), and some store a
//! company name. A lookup that compares `name` with an underlying finds no
//! contract on those brokers, which left the option tools without expiries.
//! Every expiry, chain and futures lookup keys on [`extract_underlying`]
//! instead, and keeps `name` for display and text search only. The web
//! compares `name` and has the same gap (CLAUDE.md, known pitfalls).
//!
//! The identity is exact: `NIFTY` never matches `NIFTYNXT50`, `BANKNIFTY`
//! or `FINNIFTY`, and underlyings with digits, `&` or `-` (`360ONE`, `M&M`,
//! `BAJAJ-AUTO`) are read whole.

use super::symbols::SymToken;

/// Web `FNO_EXCHANGES` (F&O plus crypto).
pub const FNO_EXCHANGES: &[&str] = &["NFO", "BFO", "MCX", "CDS", "BCD", "NCDEX", "NCO", "CRYPTO"];

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

pub fn is_fno(exchange: &str) -> bool {
    FNO_EXCHANGES.contains(&exchange)
}

/// `DDMMMYY` at the start of `s` (bytes, upper case).
fn date_at(s: &[u8]) -> bool {
    s.len() >= 7
        && s[0].is_ascii_digit()
        && s[1].is_ascii_digit()
        && std::str::from_utf8(&s[2..5])
            .map(|m| MONTHS.contains(&m))
            .unwrap_or(false)
        && s[5].is_ascii_digit()
        && s[6].is_ascii_digit()
}

/// `(\d+(\.\d+)?)?(FUT|CE|PE)?$` over the whole tail.
fn strike_suffix(tail: &str) -> bool {
    let rest = tail
        .strip_suffix("FUT")
        .or_else(|| tail.strip_suffix("CE"))
        .or_else(|| tail.strip_suffix("PE"))
        .unwrap_or(tail);
    if rest.is_empty() {
        return true;
    }
    let mut parts = rest.splitn(2, '.');
    let int = parts.next().unwrap_or_default();
    let ok_int = !int.is_empty() && int.bytes().all(|b| b.is_ascii_digit());
    match parts.next() {
        None => ok_int,
        Some(frac) => ok_int && !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// Web `extract_underlying_from_symbol`: the underlying of an F&O or
/// crypto symbol, `None` on a cash venue or for a symbol that carries no
/// `DDMMMYY` expiry.
pub fn extract_underlying(symbol: &str, exchange: &str) -> Option<String> {
    if symbol.is_empty() || !is_fno(exchange) {
        return None;
    }
    let up = symbol.to_uppercase();
    let b = up.as_bytes();
    if exchange == "CRYPTO" {
        for i in 1..b.len() {
            if !b[i - 1].is_ascii_alphanumeric() {
                break;
            }
            if date_at(&b[i..]) {
                return Some(up[..i].to_string());
            }
        }
        let mut s = up.strip_suffix(".P").unwrap_or(&up).to_string();
        for suf in ["USDT", "USD", "_INR", "INR"] {
            if s.ends_with(suf) && s.len() > suf.len() {
                s.truncate(s.len() - suf.len());
                return Some(s);
            }
        }
        return Some(s);
    }
    for i in 1..b.len() {
        if !up.is_char_boundary(i) {
            continue;
        }
        if date_at(&b[i..]) && strike_suffix(&up[i + 7..]) {
            return Some(up[..i].to_string());
        }
    }
    None
}

/// `name` when the symbol starts with it and a digit follows, as on the few
/// masters whose derivative symbols carry no `DDMMMYY` (AliceBlue futures
/// are `BANKNIFTY26OCTFUT`). A description never qualifies: it holds spaces
/// the symbol does not.
fn anchored_name(r: &SymToken) -> Option<String> {
    let (s, n) = (r.symbol.as_bytes(), r.name.trim().as_bytes());
    (!n.is_empty()
        && s.len() > n.len()
        && s[..n.len()].eq_ignore_ascii_case(n)
        && s[n.len()].is_ascii_digit())
    .then(|| r.name.trim().to_ascii_uppercase())
}

/// The underlying a derivative row belongs to: read from its canonical
/// symbol, else the `name` its symbol starts with; `None` for a row that is
/// not a dated derivative on an F&O venue.
pub fn row_underlying(r: &SymToken) -> Option<String> {
    if !is_fno(&r.exchange) {
        return None;
    }
    extract_underlying(&r.symbol, &r.exchange).or_else(|| anchored_name(r))
}

/// Whether a row is a dated derivative of `underlying` (upper case) on its
/// venue. A cheap prefix test runs before the full parse.
pub fn is_derivative_of(r: &SymToken, underlying: &str) -> bool {
    let (s, u) = (r.symbol.as_bytes(), underlying.as_bytes());
    !u.is_empty()
        && s.len() > u.len()
        && s[..u.len()].eq_ignore_ascii_case(u)
        && row_underlying(r).as_deref() == Some(underlying)
}

/// What a derivative contract is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractKind {
    Future,
    Call,
    Put,
}

impl ContractKind {
    pub fn is_option(self) -> bool {
        matches!(self, ContractKind::Call | ContractKind::Put)
    }
}

/// The contract kind from the canonical symbol's suffix (`FUT`, `CE`,
/// `PE`), which every broker builds the same way; the `instrumenttype`
/// column is broker-specific (`OPTIDX`, `FUTSTK`, ...) on some masters.
pub fn contract_kind(symbol: &str) -> Option<ContractKind> {
    let s = symbol.as_bytes();
    let ends =
        |suf: &[u8]| s.len() > suf.len() && s[s.len() - suf.len()..].eq_ignore_ascii_case(suf);
    if ends(b"FUT") {
        Some(ContractKind::Future)
    } else if ends(b"CE") {
        Some(ContractKind::Call)
    } else if ends(b"PE") {
        Some(ContractKind::Put)
    } else {
        None
    }
}

/// Whether a row is of the `instrumenttype` a lookup asked for: `FUT`,
/// `CE` and `PE` are read from the symbol, anything else must equal the
/// row's own column.
pub fn type_matches(r: &SymToken, instrument_type: &str) -> bool {
    match instrument_type {
        "FUT" => contract_kind(&r.symbol) == Some(ContractKind::Future),
        "CE" => contract_kind(&r.symbol) == Some(ContractKind::Call),
        "PE" => contract_kind(&r.symbol) == Some(ContractKind::Put),
        other => r.instrument_type == other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(symbol: &str, exchange: &str, name: &str) -> SymToken {
        SymToken {
            symbol: symbol.into(),
            brsymbol: symbol.into(),
            name: name.into(),
            exchange: exchange.into(),
            brexchange: exchange.into(),
            token: "1".into(),
            expiry: String::new(),
            strike: 0.0,
            lot_size: 1,
            instrument_type: "OPTIDX".into(),
            tick_size: 0.05,
        }
    }

    #[test]
    fn underlying_extraction_matches_the_web_regexes() {
        assert_eq!(
            extract_underlying("NIFTY28MAR2420800CE", "NFO").as_deref(),
            Some("NIFTY")
        );
        assert_eq!(
            extract_underlying("BANKNIFTY24APR24FUT", "NFO").as_deref(),
            Some("BANKNIFTY")
        );
        assert_eq!(
            extract_underlying("M&M28MAR24FUT", "NFO").as_deref(),
            Some("M&M")
        );
        assert_eq!(
            extract_underlying("USDINR28MAR2483.25CE", "CDS").as_deref(),
            Some("USDINR")
        );
        assert_eq!(
            extract_underlying("BTC28FEB2580000CE", "CRYPTO").as_deref(),
            Some("BTC")
        );
        assert_eq!(
            extract_underlying("1INCH28FEB25FUT", "CRYPTO").as_deref(),
            Some("1INCH")
        );
        assert_eq!(
            extract_underlying("BTCUSD.P", "CRYPTO").as_deref(),
            Some("BTC")
        );
        assert_eq!(extract_underlying("SBIN", "NSE"), None);
        assert_eq!(extract_underlying("SBIN", "NFO"), None);
    }

    /// The identity is the whole underlying, never a prefix, whatever the
    /// broker wrote in `name`.
    #[test]
    fn identity_is_exact_and_ignores_the_name_column() {
        let cases = [
            ("NIFTY27OCT2622400CE", "NIFTY"),
            ("NIFTYNXT5027OCT2668000CE", "NIFTYNXT50"),
            ("BANKNIFTY27OCT2651000PE", "BANKNIFTY"),
            ("FINNIFTY27OCT2623000CE", "FINNIFTY"),
            ("MIDCPNIFTY27OCT2612000CE", "MIDCPNIFTY"),
            ("M&M27OCT263000CE", "M&M"),
            ("BAJAJ-AUTO27OCT269000PE", "BAJAJ-AUTO"),
            ("360ONE27OCT261200CE", "360ONE"),
            ("NIFTY27OCT26FUT", "NIFTY"),
            ("USDINR28OCT2687.5CE", "USDINR"),
        ];
        for (symbol, und) in cases {
            let r = row(symbol, "NFO", "NIFTY 27 Oct 26 22400 CE");
            assert_eq!(
                extract_underlying(symbol, "NFO").as_deref(),
                Some(und),
                "{symbol}"
            );
            assert!(is_derivative_of(&r, und), "{symbol} of {und}");
            for other in ["NIFTY", "NIFTYNXT50", "BANKNIFTY", "FINNIFTY", "M&M", "M"] {
                if other != und {
                    assert!(!is_derivative_of(&r, other), "{symbol} is not {other}");
                }
            }
        }
        assert!(!is_derivative_of(
            &row("NIFTY", "NSE_INDEX", "NIFTY"),
            "NIFTY"
        ));
        assert!(!is_derivative_of(
            &row("NIFTY27OCT26FUT", "NFO", "NIFTY"),
            ""
        ));
    }

    /// AliceBlue builds futures as `BANKNIFTY26OCTFUT`, without `DDMMMYY`:
    /// the `name` the symbol starts with decides, and a description never
    /// does.
    #[test]
    fn a_symbol_without_a_date_falls_back_to_its_leading_name() {
        let r = row("BANKNIFTY26OCTFUT", "NFO", "BANKNIFTY");
        assert_eq!(row_underlying(&r).as_deref(), Some("BANKNIFTY"));
        assert!(is_derivative_of(&r, "BANKNIFTY"));
        let r = row("NIFTYNXT5026OCTFUT", "NFO", "NIFTYNXT50");
        assert!(is_derivative_of(&r, "NIFTYNXT50"));
        assert!(!is_derivative_of(&r, "NIFTY"));
        // A name that is not followed by a digit is not the underlying.
        let r = row("NIFTYNXT5026OCTFUT", "NFO", "NIFTY");
        assert!(!is_derivative_of(&r, "NIFTY"));
        // Neither is a description.
        let r = row("BANKNIFTY26OCTFUT", "NFO", "BANKNIFTY 26 Oct FUT");
        assert_eq!(row_underlying(&r), None);
        // Nor anything on a cash venue.
        assert_eq!(row_underlying(&row("SBIN1", "NSE", "SBIN")), None);
    }

    #[test]
    fn kind_comes_from_the_symbol_not_the_broker_type() {
        assert_eq!(
            contract_kind("NIFTY27OCT2622400CE"),
            Some(ContractKind::Call)
        );
        assert_eq!(
            contract_kind("NIFTY27OCT2622400PE"),
            Some(ContractKind::Put)
        );
        assert_eq!(contract_kind("NIFTY27OCT26FUT"), Some(ContractKind::Future));
        assert_eq!(contract_kind("SBIN"), None);
        assert_eq!(contract_kind("CE"), None);
        let r = row("NIFTY27OCT2622400CE", "NFO", "NIFTY");
        assert!(type_matches(&r, "CE"));
        assert!(!type_matches(&r, "PE"));
        assert!(!type_matches(&r, "FUT"));
        assert!(type_matches(&r, "OPTIDX"));
    }
}
