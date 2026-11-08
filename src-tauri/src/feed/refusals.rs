//! The subscribes a broker's web adapter refuses before it looks the
//! symbol up, refused here with the same message (audit C02: never
//! acknowledge what the broker's feed cannot do), for feed clients and,
//! through the manager (`WebSocketManager::set_refusal`), for every other
//! consumer of the feed. Each comes from the
//! broker's `streaming/<broker>_adapter.py` at web a1b9e3250, where the
//! adapter answers `{"status": "error", "code": "INVALID_DEPTH", ...}` and
//! the web proxy hands the client the message alone (`server.py:1318-1329`).
//!
//! A broker absent here takes any depth, its web adapter falling back to a
//! level it streams (`dialect::subscribe_depth`).

use crate::feed::source::{InstrumentKey, Mode};

/// The depths a broker's web adapter accepts in Depth mode, and how it
/// words a refusal of any other (`{}` is the depth asked for).
fn depths_accepted(broker: &str) -> Option<(&'static [i64], &'static str)> {
    const FIVE: &[i64] = &[5];
    const FIVE_OR_TWENTY: &[i64] = &[5, 20];
    Some(match broker {
        // angel_adapter.py:360, definedge_adapter.py:401,
        // pocketful_adapter.py:256, samco_adapter.py:234,
        // tradejini_adapter.py:462.
        "angel" | "definedge" | "pocketful" | "samco" | "tradejini" => {
            (FIVE, "Invalid depth level {}. Must be 5")
        }
        // fivepaisa_adapter.py:411.
        "fivepaisa" => (
            FIVE,
            "Invalid depth level {}. 5Paisa only supports 5 levels",
        ),
        // motilal_adapter.py:244.
        "motilal" => (
            FIVE,
            "Invalid depth level {}. Motilal only supports 5-level depth",
        ),
        // mstock_adapter.py:240.
        "mstock" => (
            FIVE,
            "Invalid depth level {}. mstock only supports 5 levels",
        ),
        // paytm_adapter.py:388.
        "paytm" => (FIVE, "Invalid depth level {}. Paytm supports only 5 levels"),
        // The XTS adapters: compositedge_adapter.py:313,
        // fivepaisaxts_adapter.py:495, iifl_adapter.py:311,
        // ibulls_adapter.py:311, jainamxts_adapter.py:311,
        // rmoney_adapter.py:347, wisdom_adapter.py:311.
        "compositedge" | "fivepaisaxts" | "iifl" | "ibulls" | "jainamxts" | "rmoney" | "wisdom" => {
            (FIVE_OR_TWENTY, "Invalid depth level {}. Must be 5 or 20")
        }
        _ => return None,
    })
}

/// Why `broker`'s web adapter refuses this subscribe outright, if it does.
pub(crate) fn refusal(
    broker: &str,
    key: &InstrumentKey,
    mode: Mode,
    requested_depth: i64,
) -> Option<String> {
    if mode != Mode::Depth {
        return None;
    }
    // indmoney_adapter.py:311-315: INVALID_MODE, before the symbol lookup.
    if broker == "indmoney" {
        return Some("Invalid mode 3. INDmoney supports only 1 (LTP) or 2 (Quote)".to_string());
    }
    // nubra_adapter.py:379: an index has no book on Nubra's feed.
    if broker == "nubra" && key.is_index() {
        return Some("Depth mode not supported for index symbols".to_string());
    }
    let (accepted, words) = depths_accepted(broker)?;
    (!accepted.contains(&requested_depth))
        .then(|| words.replace("{}", &requested_depth.to_string()))
}

/// Whether `broker`'s web adapter refuses an exchange before it looks the
/// symbol up (groww_adapter.py:388 after the mode, sahi_adapter.py:204);
/// the others look the symbol up first (indmoney_adapter.py:318-341,
/// dhan_adapter.py:321-333), so an unknown symbol on an exchange they do
/// not stream is answered as unknown.
pub(crate) fn exchange_before_lookup(broker: &str) -> bool {
    matches!(broker, "groww" | "sahi")
}

/// How `broker`'s web adapter words a symbol it cannot find: the web's
/// proxy does not look symbols up (`server.py:1282`), each adapter's
/// `subscribe` does, at web a1b9e3250.
pub(crate) fn not_found(broker: &str, key: &InstrumentKey) -> String {
    let (s, e) = (&key.symbol, &key.exchange);
    match broker {
        // The Noren adapters' SYMBOL_NOT_FOUND: flattrade_adapter.py:513,
        // shoonya_adapter.py:495, tradesmart_adapter.py:222,
        // zebu_adapter.py:510.
        "flattrade" | "shoonya" | "tradesmart" | "zebu" => format!("Symbol {s} not found"),
        // TOKEN_NOT_FOUND: arrow_adapter.py:134,
        // hdfcsecurities_adapter.py:194, hdfcsky_adapter.py:217,
        // rupeezy_adapter.py:120.
        "arrow" | "hdfcsecurities" | "hdfcsky" | "rupeezy" => format!("No token for {e}:{s}"),
        // delta_adapter.py:203.
        "deltaexchange" => format!("{s} not found for exchange {e}"),
        // iiflcapital_adapter.py:249.
        "iiflcapital" => format!("Token / brexchange not found for {e}:{s}"),
        // kotak_adapter.py:1190: a missing token is logged, and the
        // subscribe answered as failed.
        "kotak" => format!("Failed to subscribe to {e}:{s}"),
        // nubra_adapter.py:427 for an index, :504 otherwise.
        "nubra" if key.is_index() => format!("No index mapping for {s}"),
        // aliceblue_adapter.py:572, nubra_adapter.py:504,
        // sahi_adapter.py:212, zerodha_adapter.py:284.
        "aliceblue" | "nubra" | "sahi" | "zerodha" => format!("Token not found for {s} on {e}"),
        // The SymbolMapper adapters' SYMBOL_NOT_FOUND (angel_adapter.py:369
        // and the XTS, Definedge, Dhan, Firstock, 5paisa, Groww, IndMoney,
        // Motilal, mstock, Paytm, Pocketful, Samco, Tradejini and Upstox
        // adapters). Fyers and the Dhan sandbox look nothing up in their
        // `subscribe` and acknowledge an unknown symbol on the web, which
        // then never streams: refused here in these words.
        _ => format!("Symbol {s} not found for exchange {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nse() -> InstrumentKey {
        InstrumentKey::new("RELIANCE", "NSE")
    }

    /// The web adapters' own wording, for each depth they refuse; 5 (and
    /// 20 on XTS) and the other modes pass.
    #[test]
    fn each_broker_refuses_what_its_web_adapter_refuses() {
        let cases = [
            ("angel", 20, Some("Invalid depth level 20. Must be 5")),
            ("angel", 5, None),
            ("samco", 50, Some("Invalid depth level 50. Must be 5")),
            (
                "fivepaisa",
                20,
                Some("Invalid depth level 20. 5Paisa only supports 5 levels"),
            ),
            (
                "motilal",
                30,
                Some("Invalid depth level 30. Motilal only supports 5-level depth"),
            ),
            (
                "mstock",
                20,
                Some("Invalid depth level 20. mstock only supports 5 levels"),
            ),
            (
                "paytm",
                20,
                Some("Invalid depth level 20. Paytm supports only 5 levels"),
            ),
            ("compositedge", 20, None),
            (
                "rmoney",
                50,
                Some("Invalid depth level 50. Must be 5 or 20"),
            ),
            ("zerodha", 50, None),
        ];
        for (broker, depth, want) in cases {
            assert_eq!(
                refusal(broker, &nse(), Mode::Depth, depth).as_deref(),
                want,
                "{broker} {depth}"
            );
            assert_eq!(refusal(broker, &nse(), Mode::Quote, depth), None);
        }
        let nifty = InstrumentKey::new("NIFTY", "NSE_INDEX");
        assert_eq!(
            refusal("nubra", &nifty, Mode::Depth, 5).as_deref(),
            Some("Depth mode not supported for index symbols")
        );
        assert_eq!(refusal("nubra", &nse(), Mode::Depth, 5), None);
        assert_eq!(refusal("nubra", &nifty, Mode::Ltp, 5), None);
        assert_eq!(
            refusal("indmoney", &nse(), Mode::Depth, 5).as_deref(),
            Some("Invalid mode 3. INDmoney supports only 1 (LTP) or 2 (Quote)")
        );
        assert_eq!(refusal("indmoney", &nse(), Mode::Quote, 5), None);
    }

    /// Each adapter's words for a symbol it cannot find.
    #[test]
    fn an_unknown_symbol_is_worded_as_the_brokers_web_adapter_words_it() {
        let mcx = InstrumentKey::new("GOLDX", "MCX");
        let cases = [
            ("indmoney", "Symbol GOLDX not found for exchange MCX"),
            ("angel", "Symbol GOLDX not found for exchange MCX"),
            ("compositedge", "Symbol GOLDX not found for exchange MCX"),
            ("shoonya", "Symbol GOLDX not found"),
            ("flattrade", "Symbol GOLDX not found"),
            ("zebu", "Symbol GOLDX not found"),
            ("tradesmart", "Symbol GOLDX not found"),
            ("hdfcsky", "No token for MCX:GOLDX"),
            ("rupeezy", "No token for MCX:GOLDX"),
            ("deltaexchange", "GOLDX not found for exchange MCX"),
            ("iiflcapital", "Token / brexchange not found for MCX:GOLDX"),
            ("kotak", "Failed to subscribe to MCX:GOLDX"),
            ("zerodha", "Token not found for GOLDX on MCX"),
            ("sahi", "Token not found for GOLDX on MCX"),
            ("nubra", "Token not found for GOLDX on MCX"),
        ];
        for (broker, words) in cases {
            assert_eq!(not_found(broker, &mcx), words, "{broker}");
        }
        let nifty = InstrumentKey::new("NIFTYX", "NSE_INDEX");
        assert_eq!(not_found("nubra", &nifty), "No index mapping for NIFTYX");
        assert!(exchange_before_lookup("groww") && exchange_before_lookup("sahi"));
        assert!(!exchange_before_lookup("indmoney") && !exchange_before_lookup("dhan"));
    }
}
