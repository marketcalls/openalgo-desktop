//! The subscribes a broker's web adapter refuses before it looks the
//! symbol up, refused here with the same message (audit C02: never
//! acknowledge what the broker's feed cannot do). Each comes from the
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
pub(super) fn refusal(
    broker: &str,
    key: &InstrumentKey,
    mode: Mode,
    requested_depth: i64,
) -> Option<String> {
    if mode != Mode::Depth {
        return None;
    }
    // nubra_adapter.py:379: an index has no book on Nubra's feed.
    if broker == "nubra" && key.is_index() {
        return Some("Depth mode not supported for index symbols".to_string());
    }
    let (accepted, words) = depths_accepted(broker)?;
    (!accepted.contains(&requested_depth))
        .then(|| words.replace("{}", &requested_depth.to_string()))
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
    }
}
